mod admin;
mod app;
mod auth;
mod backend;
mod config;
mod content;
mod error;
mod openai;
mod prompt;
mod session_api;
mod sessions;
mod store;
pub use app::App;
use axum::{
    body::to_bytes,
    extract::{ConnectInfo, Request, State},
    http::{header, HeaderValue, StatusCode},
    response::{Html, IntoResponse, Redirect, Response},
    Json, Router,
};
use clap::{Parser, Subcommand};
use serde_json::json;
use std::{net::SocketAddr, sync::Arc};
#[derive(Parser)]
#[command(version, about = "PocketRelay — standalone AI gateway")]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,
}
#[derive(Subcommand)]
enum Command {
    Serve {
        /// Stop the server when the parent application closes its stdin pipe.
        #[arg(long)]
        exit_on_stdin_close: bool,
    },
    Setup {
        #[arg(long)]
        if_missing: bool,
        /// Read {username,password} from stdin, keeping secrets out of process arguments.
        #[arg(long)]
        json_stdin: bool,
    },
    Healthcheck,
    CreateKey {
        #[arg(long)]
        name: String,
    },
}
#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    let config = config::Config::read()?;
    match cli.command.unwrap_or(Command::Serve {
        exit_on_stdin_close: false,
    }) {
        Command::Setup {
            if_missing,
            json_stdin,
        } => {
            if json_stdin {
                auth::setup_stdin(&config, if_missing)
            } else {
                auth::setup(&config, if_missing)
            }
        }
        Command::CreateKey { name } => {
            // Use the running server to avoid overwriting a cached file.
            let client = reqwest::Client::new();
            let base = format!("http://127.0.0.1:{}", config.port);
            let login = client
                .post(format!("{base}/admin/auth/token"))
                .header("X-Admin", "1")
                .json(&json!({"token": config.token}))
                .send()
                .await?
                .error_for_status()?;
            let cookie = login
                .headers()
                .get(header::SET_COOKIE)
                .ok_or_else(|| anyhow::anyhow!("Missing cookie"))?
                .to_str()?
                .split(';')
                .next()
                .unwrap_or("")
                .to_string();
            let key: serde_json::Value = client
                .post(format!("{base}/admin/api/keys"))
                .header("X-Admin", "1")
                .header(header::COOKIE, cookie)
                .json(&json!({"name":name}))
                .send()
                .await?
                .error_for_status()?
                .json()
                .await?;
            println!("{}", config::text(&key, "key"));
            Ok(())
        }
        Command::Healthcheck => {
            reqwest::Client::new()
                .get(format!("http://127.0.0.1:{}/healthz", config.port))
                .timeout(std::time::Duration::from_secs(3))
                .send()
                .await?
                .error_for_status()?;
            Ok(())
        }
        Command::Serve {
            exit_on_stdin_close,
        } => {
            let app = App::new(config)?;
            let task_app = Arc::downgrade(&app);
            let flush = tokio::spawn(async move {
                let mut tick = tokio::time::interval(std::time::Duration::from_secs(2));
                loop {
                    tick.tick().await;
                    let Some(app) = task_app.upgrade() else { break };
                    let mut s = app.store.lock().unwrap();
                    if s.dirty {
                        if let Err(e) = s.flush() {
                            eprintln!("Save failed: {e}");
                        }
                    }
                }
            });
            let listener =
                tokio::net::TcpListener::bind((app.config.host.as_str(), app.config.port)).await?;
            println!(
                "PocketRelay ready at http://{}:{} — console /admin",
                app.config.host, app.config.port
            );
            let router = router(app.clone());
            axum::serve(
                listener,
                router.into_make_service_with_connect_info::<SocketAddr>(),
            )
            .with_graceful_shutdown({
                let app = app.clone();
                async move {
                    if exit_on_stdin_close {
                        tokio::select! { _ = shutdown() => {}, _ = parent_closed() => {} }
                    } else {
                        shutdown().await;
                    }
                    app.shutdown.send_replace(true);
                    app.sem.close();
                }
            })
            .await?;
            app.sessions.shutdown().await;
            app.codex.shutdown().await;
            flush.abort();
            app.store.lock().unwrap().flush()?;
            Ok(())
        }
    }
}
async fn parent_closed() {
    let (tx, rx) = tokio::sync::oneshot::channel();
    // Use a dedicated thread so a pending read cannot block runtime shutdown.
    std::thread::spawn(move || {
        use std::io::Read;
        let mut byte = [0u8; 1];
        while matches!(std::io::stdin().read(&mut byte), Ok(n) if n > 0) {}
        let _ = tx.send(());
    });
    let _ = rx.await;
}
async fn shutdown() {
    #[cfg(unix)]
    {
        let mut term = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .expect("signal");
        tokio::select! {_=tokio::signal::ctrl_c()=>{},_=term.recv()=>{}}
    }
    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
    }
}
pub fn router(app: Arc<App>) -> Router {
    Router::new()
        .fallback(dispatch)
        .with_state(app)
        .layer(tower_http::compression::CompressionLayer::new())
}
async fn dispatch(State(app): State<Arc<App>>, request: Request) -> Response {
    let path = request.uri().path().to_string();
    let mut stop = app.shutdown.subscribe();
    let response = tokio::select! {
        response = dispatch_inner(app.clone(), request) => response.unwrap_or_else(IntoResponse::into_response),
        _ = stop.changed() => error::Error::new(503, "Server is shutting down").into_response(),
    };
    let mut response = if response
        .headers()
        .get(header::CONTENT_TYPE)
        .is_some_and(|v| v == "text/event-stream")
    {
        use futures_util::StreamExt;
        let (parts, body) = response.into_parts();
        let mut stream = body.into_data_stream();
        let body = axum::body::Body::from_stream(async_stream::stream! {
            loop {
                if *stop.borrow() { break; }
                tokio::select! {
                    _ = stop.changed() => break,
                    item = stream.next() => match item { Some(item) => yield item, None => break },
                }
            }
        });
        Response::from_parts(parts, body)
    } else {
        response
    };
    let h = response.headers_mut();
    h.insert(
        "x-content-type-options",
        HeaderValue::from_static("nosniff"),
    );
    h.insert("x-frame-options", HeaderValue::from_static("DENY"));
    h.insert("referrer-policy", HeaderValue::from_static("same-origin"));
    if path.starts_with("/admin") {
        h.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
        h.insert("content-security-policy",HeaderValue::from_static("default-src 'self'; img-src 'self' data:; style-src 'self' 'unsafe-inline'; frame-ancestors 'none'; form-action 'self' https:"));
    }
    if path.starts_with("/v1/") {
        h.insert("access-control-allow-origin", HeaderValue::from_static("*"));
        h.insert(
            "access-control-allow-methods",
            HeaderValue::from_static("GET, POST, OPTIONS"),
        );
        h.insert(
            "access-control-allow-headers",
            HeaderValue::from_static(
                "Authorization, Content-Type, X-API-Key, X-OpenWebUI-User-JWT",
            ),
        );
    }
    response
}
async fn dispatch_inner(app: Arc<App>, request: Request) -> error::Result<Response> {
    let (parts, body) = request.into_parts();
    let path = parts.uri.path();
    let host = parts
        .headers
        .get(header::HOST)
        .and_then(|h| h.to_str().ok())
        .unwrap_or("");
    let host = host
        .parse::<axum::http::uri::Authority>()
        .ok()
        .map(|a| a.host().trim_matches(['[', ']']).to_string())
        .unwrap_or_default();
    if path != "/healthz" && !app.config.allowed_hosts.contains(&host) {
        return Err(error::Error::new(403, "Host rejected"));
    }
    if parts.method == axum::http::Method::OPTIONS && path.starts_with("/v1/") {
        return Ok(StatusCode::NO_CONTENT.into_response());
    }
    if path == "/healthz" {
        return Ok(Json(json!({"ok":true})).into_response());
    }
    if parts.method == axum::http::Method::GET || parts.method == axum::http::Method::HEAD {
        if path.starts_with("/static/") {
            return static_file(
                &app,
                path.trim_start_matches("/static/"),
                &parts.headers,
                parts.method == axum::http::Method::HEAD,
            )
            .await;
        }
        if path == "/" {
            return if app.config.sessions {
                static_file(&app, "index.html", &parts.headers, false).await
            } else {
                Ok(Redirect::to("/admin").into_response())
            };
        }
        if path == "/admin" || path == "/admin/" {
            return static_file(&app, "admin.html", &parts.headers, false).await;
        }
        if app.config.docs && path == "/docs" {
            return Ok(Html(include_str!("../static/api-docs.html")).into_response());
        }
        if app.config.docs && path == "/openapi.json" {
            return Ok(Json(json!({"openapi":"3.1.0","info":{"title":"PocketRelay","version":env!("CARGO_PKG_VERSION")},"components":{"securitySchemes":{"bearer":{"type":"http","scheme":"bearer"}}},"security":[{"bearer":[]}],"paths":{"/v1/models":{"get":{"responses":{"200":{"description":"Models by account"}}}},"/v1/chat/completions":{"post":{"responses":{"200":{"description":"JSON or SSE response"}}}},"/v1/responses":{"post":{"responses":{"200":{"description":"JSON or SSE response"}}}},"/v1/completions":{"post":{"responses":{"200":{"description":"JSON or SSE completion"}}}}}})).into_response());
        }
    }
    let bytes = to_bytes(body, 64 * 1024 * 1024)
        .await
        .map_err(|_| error::Error::new(413, "Request too large (maximum 64 MiB)"))?;
    if path.starts_with("/admin/") {
        let peer = parts
            .extensions
            .get::<ConnectInfo<SocketAddr>>()
            .map(|p| p.0.ip().to_string())
            .unwrap_or_else(|| "local".into());
        admin::handle(app, parts.method, parts.uri, parts.headers, bytes, peer).await
    } else if path.starts_with("/v1/") {
        openai::handle(app, parts.method, parts.uri, parts.headers, bytes).await
    } else if path.starts_with("/api/") {
        session_api::handle(app, parts.method, parts.uri, parts.headers, bytes).await
    } else {
        Err(error::Error::new(404, "Unknown route"))
    }
}
async fn static_file(
    app: &App,
    name: &str,
    h: &axum::http::HeaderMap,
    head: bool,
) -> error::Result<Response> {
    if name.split('/').any(|p| p == ".." || p.starts_with('.'))
        || name.contains('\\')
        || name.contains('%')
    {
        return Err(error::Error::new(404, "Unknown file"));
    }
    let path = app.config.static_dir.join(name);
    let meta = tokio::fs::metadata(&path)
        .await
        .map_err(|_| error::Error::new(404, "Unknown file"))?;
    if !meta.is_file() {
        return Err(error::Error::new(404, "Unknown file"));
    }
    let modified = meta
        .modified()
        .ok()
        .and_then(|v| v.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let etag = format!("\"{:x}-{:x}\"", meta.len(), modified);
    let mut r = if h
        .get(header::IF_NONE_MATCH)
        .is_some_and(|v| v == etag.as_str())
    {
        StatusCode::NOT_MODIFIED.into_response()
    } else if head {
        StatusCode::OK.into_response()
    } else {
        tokio::fs::read(&path).await?.into_response()
    };
    let mime = match path.extension().and_then(|e| e.to_str()).unwrap_or("") {
        "html" => "text/html; charset=utf-8",
        "js" => "text/javascript; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        "svg" => "image/svg+xml",
        "png" => "image/png",
        "ico" => "image/x-icon",
        "json" => "application/json",
        _ => "application/octet-stream",
    };
    r.headers_mut()
        .insert(header::CONTENT_TYPE, HeaderValue::from_static(mime));
    r.headers_mut().insert(header::ETAG, etag.parse().unwrap());
    r.headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-cache"));
    Ok(r)
}

#[cfg(test)]
mod tests;
