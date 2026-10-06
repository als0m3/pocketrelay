use crate::{
    auth,
    backend::{Event, Prompt},
    config::{self, now, text},
    error::{Error, Result},
    openai, App,
};
use axum::{
    body::Bytes,
    http::{header, HeaderMap, Method, Uri},
    response::{IntoResponse, Redirect, Response},
    Json,
};
use futures_util::StreamExt;
use serde_json::{json, Value};
use std::{collections::HashMap, sync::Arc};
fn with_cookie(mut r: Response, cookie: String) -> Result<Response> {
    r.headers_mut().append(
        header::SET_COOKIE,
        cookie
            .parse()
            .map_err(|_| Error::new(500, "Invalid cookie"))?,
    );
    Ok(r)
}
fn quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}
async fn login_account(app: &Arc<App>, a: &Value) -> Result<Value> {
    let p = text(a, "provider");
    let home = app.store.lock().unwrap().cli_home(a, p);
    if p == "antigravity" {
        std::fs::create_dir_all(&home)?;
        return Ok(
            json!({"command":format!("env HOME={} {}",quote(&home.to_string_lossy()),quote(&app.config.antigravity))}),
        );
    }
    if p != "codex" {
        return Err(Error::new(
            400,
            "Device-code login is only available for Codex accounts",
        ));
    }
    app.codex
        .get(&app.config, a, home)
        .await?
        .call("account/login/start", json!({"type":"chatgptDeviceCode"}))
        .await
}
async fn test(app: &Arc<App>, a: Value) -> Value {
    let start = now();
    let p = text(&a, "provider").to_string();
    if p == "codex" {
        app.refresh_catalog(true).await;
    }
    let model = match p.as_str() {
        "claude" => "haiku".into(),
        "antigravity" => config::envs("REMOTE_ANTIGRAVITY_FAST_MODEL", "gemini-3-flash"),
        _ => app.catalog.lock().unwrap()["codex"]
            .as_array()
            .and_then(|v| v.first())
            .map(|v| text(v, "id").to_string())
            .unwrap_or_default(),
    };
    let m = openai::Model {
        public: format!("{}/{}", text(&a, "slug"), model),
        plain: model.clone(),
        provider: p,
        pinned: Some(a),
    };
    let mut stream = openai::run(
        app.clone(),
        m,
        Prompt {
            system: "Reply with exactly: pong".into(),
            blocks: vec![json!({"type":"text","text":"ping"})],
            model: model.clone(),
            effort: "low".into(),
            schema: None,
        },
    );
    let mut reply = String::new();
    while let Some(ev) = stream.next().await {
        match ev {
            Ok(Event::Text(t)) => reply += &t,
            Ok(_) => {}
            Err(e) => {
                return json!({"ok":false,"error":e.message,"model":model,"seconds":now()-start})
            }
        }
    }
    json!({"ok":true,"reply":reply.trim(),"model":model,"seconds":now()-start})
}
fn check_token(t: &str) -> Result<()> {
    if !t.starts_with("sk-ant-") {
        return Err(Error::new(
            400,
            "This is not a claude setup-token token (expected sk-ant- prefix).",
        ));
    }
    Ok(())
}
pub async fn handle(
    app: Arc<App>,
    method: Method,
    uri: Uri,
    h: HeaderMap,
    bytes: Bytes,
    peer: String,
) -> Result<Response> {
    let path = uri.path().trim_end_matches('/');
    let q: HashMap<String, String> =
        url::form_urlencoded::parse(uri.query().unwrap_or("").as_bytes())
            .into_owned()
            .collect();
    let b: Value = if bytes.is_empty() {
        json!({})
    } else {
        serde_json::from_slice(&bytes)?
    };
    let c = &app.config;
    match (method.as_str(), path) {
        ("GET", "/admin/auth/config") => return Ok(Json(app.auth.config(c, &h)?).into_response()),
        ("POST", "/admin/auth/password") | ("POST", "/admin/auth/token") => {
            let cookie = app
                .auth
                .login(
                    c,
                    &h,
                    b,
                    if path.ends_with("password") {
                        "password"
                    } else {
                        "token"
                    },
                    &peer,
                )
                .await?;
            return with_cookie(Json(json!({"ok":true})).into_response(), cookie);
        }
        ("POST", "/admin/auth/logout") => {
            auth::check_origin(c, &h)?;
            return with_cookie(
                Json(json!({"ok":true})).into_response(),
                auth::cookie(c, "cr_admin", "", 0),
            );
        }
        ("GET", "/admin/auth/login") => {
            let (url, cookie) = app.auth.oidc_start(c, &h, &app.client).await?;
            return with_cookie(Redirect::to(&url).into_response(), cookie);
        }
        ("GET", "/admin/auth/callback") => {
            let cookie = app.auth.oidc_finish(c, &h, &q, &app.client).await?;
            return with_cookie(
                with_cookie(Redirect::to("/admin").into_response(), cookie)?,
                auth::cookie(c, "cr_oidc", "", 0),
            );
        }
        _ => {}
    }
    let user = auth::current(c, &h, method != Method::GET)?;
    let v = match (method.as_str(), path) {
        ("GET", "/admin/api/state") => app.state(user),
        ("GET", "/admin/api/catalog") => {
            app.refresh_catalog(q.get("refresh").is_some_and(|v| v == "1"))
                .await;
            app.state(user)
        }
        ("POST", "/admin/api/keys") => app.store.lock().unwrap().create_key(text(&b, "name"))?,
        ("POST", "/admin/api/accounts") => {
            let provider = text(&b, "provider");
            if provider == "codex" && c.codex.is_empty()
                || provider == "antigravity" && c.antigravity.is_empty()
            {
                return Err(Error::new(400, "CLI not installed on the server"));
            }
            let token = if provider == "claude" {
                check_token(text(&b, "token"))?;
                Some(text(&b, "token"))
            } else {
                None
            };
            let a = app
                .store
                .lock()
                .unwrap()
                .add(provider, text(&b, "label"), token)?;
            let public = app.store.lock().unwrap().public(&a);
            if provider == "claude" {
                json!({"account":public,"test":test(&app,a).await})
            } else {
                json!({"account":public,"login":login_account(&app,&a).await?})
            }
        }
        _ => {
            let parts = path.trim_start_matches('/').split('/').collect::<Vec<_>>();
            if parts.len() >= 4 && parts[..3] == ["admin", "api", "keys"] {
                let id = parts[3];
                if method == Method::DELETE && parts.len() == 4
                    || method == Method::POST && parts.get(4) == Some(&"revoke")
                {
                    app.store
                        .lock()
                        .unwrap()
                        .revoke_key(id, method == Method::DELETE)?;
                    json!({"ok":true})
                } else {
                    return Err(Error::new(404, "Unknown route"));
                }
            } else if parts.len() >= 4 && parts[..3] == ["admin", "api", "accounts"] {
                let a = app.store.lock().unwrap().account(parts[3])?;
                let id = text(&a, "id");
                match (method.as_str(), parts.get(4).copied().unwrap_or("")) {
                    ("PATCH", "") => {
                        let a = app.store.lock().unwrap().update(id, &b)?;
                        if a["enabled"] == false && a["provider"] == "codex" {
                            app.codex.stop(id).await;
                        }
                        app.store.lock().unwrap().public(&a)
                    }
                    ("DELETE", "") => {
                        app.codex.stop(id).await;
                        app.store.lock().unwrap().remove(id, c)?;
                        json!({"ok":true})
                    }
                    ("POST", "login") => {
                        app.store.lock().unwrap().resume(id);
                        login_account(&app, &a).await?
                    }
                    ("POST", "resume") => {
                        app.store.lock().unwrap().resume(id);
                        json!({"ok":true})
                    }
                    ("POST", "move") => {
                        app.store
                            .lock()
                            .unwrap()
                            .move_account(id, b["delta"].as_i64().unwrap_or(1))?;
                        json!({"ok":true})
                    }
                    ("POST", "test") => test(&app, a).await,
                    ("PUT", "token") => {
                        if a["system"] == true || a["provider"] != "claude" {
                            return Err(Error::new(400, "This account does not accept a token"));
                        }
                        check_token(text(&b, "token"))?;
                        app.store
                            .lock()
                            .unwrap()
                            .write_token(id, text(&b, "token"))?;
                        let public = app.store.lock().unwrap().public(&a);
                        json!({"account":public,"test":test(&app,a).await})
                    }
                    ("POST", "logout") => {
                        if a["provider"] == "codex" {
                            let home = app.store.lock().unwrap().cli_home(&a, "codex");
                            app.codex
                                .get(c, &a, home)
                                .await?
                                .call("account/logout", json!({}))
                                .await?;
                            app.store
                                .lock()
                                .unwrap()
                                .state(id)
                                .as_object_mut()
                                .unwrap()
                                .remove("identity");
                        } else if a["provider"] == "antigravity" {
                            let home = app.store.lock().unwrap().cli_home(&a, "antigravity");
                            if home.exists() {
                                std::fs::remove_dir_all(home)?;
                            }
                        }
                        json!({"ok":true})
                    }
                    _ => return Err(Error::new(404, "Unknown route")),
                }
            } else {
                return Err(Error::new(404, "Unknown route"));
            }
        }
    };
    Ok(Json(v).into_response())
}
