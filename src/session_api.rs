use crate::{
    backend,
    config::{self, text},
    error::{Error, Result},
    sessions, App,
};
use axum::{
    body::Bytes,
    http::{HeaderMap, Method, Uri},
    response::{IntoResponse, Response},
    Json,
};
use serde_json::{json, Value};
use std::{
    collections::{HashMap, HashSet},
    sync::Arc,
    time::Duration,
};
fn sse(v: Value) -> String {
    format!("data: {v}\n\n")
}
pub async fn handle(
    app: Arc<App>,
    method: Method,
    uri: Uri,
    h: HeaderMap,
    bytes: Bytes,
) -> Result<Response> {
    if !app.config.sessions {
        return Err(Error::new(404, "Session mode is disabled on this instance"));
    }
    let q: HashMap<String, String> =
        url::form_urlencoded::parse(uri.query().unwrap_or("").as_bytes())
            .into_owned()
            .collect();
    let key = h
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.split_once(' '))
        .filter(|(s, _)| s.eq_ignore_ascii_case("bearer"))
        .map(|(_, v)| v)
        .or_else(|| q.get("token").map(String::as_str))
        .unwrap_or("");
    if !crate::store::equal(key, &app.config.token) {
        return Err(Error::new(401, "Invalid token"));
    }
    let b: Value = if bytes.is_empty() {
        json!({})
    } else {
        serde_json::from_slice(&bytes)?
    };
    let path = uri.path();
    let m = &app.sessions;
    if method == Method::GET && path == "/api/events" {
        let mut rx = m.events.subscribe();
        let initial = json!({"type":"sessions","sessions":m.list()});
        let limits = json!({"type":"limits","limits":m.limits.lock().unwrap().clone()});
        return Ok(crate::openai::event_response(async_stream::stream! {
            yield sse(initial);
            yield sse(limits);
            loop {
                match rx.recv().await {
                    Ok(e) => yield sse(e),
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {
                        yield sse(json!({"type":"sessions","sessions":app.sessions.list()}));
                    }
                    Err(_) => break,
                }
            }
        }));
    }
    let v = match (method.as_str(), path) {
        ("GET", "/api/health") => {
            let all = m.list();
            json!({"ok":true,"claude_bin":app.config.claude,"sessions":all.len(),"alive":all.iter().filter(|s|s["alive"]==true).count(),"permission_modes":sessions::MODES,"efforts":sessions::EFFORTS})
        }
        ("GET", "/api/sessions") => json!(m.list()),
        ("POST", "/api/sessions") => m.create(&b)?.summary(),
        ("GET", "/api/history") => {
            let q = q.get("q").cloned().unwrap_or_default();
            let limit = uri
                .query()
                .and_then(|q| {
                    url::form_urlencoded::parse(q.as_bytes())
                        .find(|(k, _)| k == "limit")
                        .and_then(|(_, v)| v.parse().ok())
                })
                .unwrap_or(150);
            json!(
                tokio::task::spawn_blocking(move || sessions::history(&q, limit))
                    .await
                    .map_err(|_| Error::new(500, "Read interrupted"))?
            )
        }
        ("GET", "/api/fs") => {
            let p = config::expand(q.get("path").map(String::as_str).unwrap_or("~"))
                .canonicalize()
                .map_err(|_| Error::new(404, "Not a directory"))?;
            let home = config::home().canonicalize()?;
            if !p.starts_with(&home) {
                return Err(Error::new(403, "Outside the home directory"));
            }
            if !p.is_dir() {
                return Err(Error::new(404, "Not a directory"));
            }
            let mut dirs = std::fs::read_dir(&p)?
                .flatten()
                .filter(|e| e.path().is_dir() && !e.file_name().to_string_lossy().starts_with('.'))
                .map(|e| e.file_name().to_string_lossy().to_string())
                .collect::<Vec<_>>();
            dirs.sort_by_key(|s| s.to_lowercase());
            json!({"path":p,"parent":if p==home{None}else{p.parent()},"is_git":p.join(".git").exists(),"dirs":dirs})
        }
        ("GET", "/api/recent-dirs") => {
            let mut seen = HashSet::new();
            let mut all = m.list();
            all.sort_by(|a, b| {
                b["updated"]
                    .as_f64()
                    .unwrap_or(0.)
                    .total_cmp(&a["updated"].as_f64().unwrap_or(0.))
            });
            json!(all
                .iter()
                .filter_map(|a| {
                    let d = text(a, "cwd");
                    if seen.insert(d.to_string()) {
                        Some(d)
                    } else {
                        None
                    }
                })
                .take(15)
                .collect::<Vec<_>>())
        }
        ("GET", "/api/snippets") => {
            config::load(&app.config.data.join("snippets.json"), json!([]))?
        }
        ("PUT", "/api/snippets") => {
            if !b.as_array().is_some_and(|v| v.iter().all(Value::is_object)) {
                return Err(Error::new(400, "Expected a list of snippets"));
            }
            config::save(&app.config.data.join("snippets.json"), &b)?;
            b
        }
        ("GET", "/api/usage") => {
            let all = m.list();
            json!({"limits":m.limits.lock().unwrap().clone(),"cost_usd":all.iter().map(|s|s["cost_usd"].as_f64().unwrap_or(0.)).sum::<f64>(),"turns":all.iter().map(|s|s["turns"].as_u64().unwrap_or(0)).sum::<u64>(),"by_session":all.iter().map(|s|json!({"id":s["id"],"name":s["name"],"cost_usd":s["cost_usd"],"turns":s["turns"]})).collect::<Vec<_>>()})
        }
        ("POST", "/api/run") => {
            let mode = b["permission_mode"].as_str().unwrap_or("dontAsk");
            if !sessions::MODES.contains(&mode) {
                return Err(Error::new(400, "Unknown mode"));
            }
            if !b["prompt"].is_string() {
                return Err(Error::new(400, "prompt is required"));
            }
            let token = {
                let st = app.store.lock().unwrap();
                st.accounts
                    .iter()
                    .find(|a| a["provider"] == "claude" && st.available(a))
                    .and_then(|a| st.token(a))
            };
            let mut cmd = backend::command(&app.config.claude, backend::claude_env(token, true));
            cmd.current_dir(config::expand(b["cwd"].as_str().unwrap_or("~")))
                .args([
                    "-p",
                    text(&b, "prompt"),
                    "--output-format",
                    "json",
                    "--permission-mode",
                    mode,
                    "--permission-prompts",
                    "none",
                    "--no-session-persistence",
                ])
                .stdin(std::process::Stdio::null());
            if !text(&b, "model").is_empty() {
                cmd.args(["--model", text(&b, "model")]);
            }
            let out = tokio::time::timeout(
                Duration::from_secs(b["timeout"].as_u64().unwrap_or(600).clamp(1, 3600)),
                cmd.output(),
            )
            .await
            .map_err(|_| Error::new(504, "Timed out"))??;
            serde_json::from_slice(&out.stdout).map_err(|_| {
                Error::new(
                    502,
                    String::from_utf8_lossy(&out.stderr)
                        .chars()
                        .rev()
                        .take(2000)
                        .collect::<String>()
                        .chars()
                        .rev()
                        .collect::<String>(),
                )
            })?
        }
        _ => {
            let parts = path.trim_matches('/').split('/').collect::<Vec<_>>();
            if parts.len() < 3 || parts[..2] != ["api", "sessions"] {
                return Err(Error::new(404, "Unknown route"));
            }
            let id = parts[2];
            let s = m.get(id)?;
            let tail = parts.get(3).copied().unwrap_or("");
            if method == Method::GET && tail == "stream" {
                let mut rx = s.events.subscribe();
                let history = s.history();
                let mut last = q
                    .get("since")
                    .and_then(|v| v.parse::<u64>().ok())
                    .unwrap_or(0);
                return Ok(crate::openai::event_response(async_stream::stream! {
                    for e in history {
                        let seq = e["seq"].as_u64().unwrap_or(0);
                        if seq > last {
                            last = seq;
                            yield sse(e);
                        }
                    }
                    yield sse(json!({"type":"remote","event":"snapshot","session":s.summary()}));
                    loop {
                        match rx.recv().await {
                            Ok(e) => {
                                if let Some(seq) = e["seq"].as_u64() {
                                    if seq <= last {
                                        continue;
                                    }
                                    last = seq;
                                }
                                yield sse(e);
                            }
                            Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {
                                for e in s.history() {
                                    let seq = e["seq"].as_u64().unwrap_or(0);
                                    if seq > last {
                                        last = seq;
                                        yield sse(e);
                                    }
                                }
                                yield sse(json!({"type":"remote","event":"snapshot","session":s.summary()}));
                            }
                            Err(_) => break,
                        }
                    }
                }));
            }
            match (method.as_str(), tail) {
                ("GET", "") => json!({"session":s.summary(),"events":s.history()}),
                ("PATCH", "") => {
                    s.update(&app, &b).await?;
                    s.summary()
                }
                ("DELETE", "") => {
                    m.delete(id).await?;
                    json!({"ok":true})
                }
                ("POST", "messages") => {
                    s.send(app.clone(), &b).await?;
                    json!({"ok":true})
                }
                ("POST", "interrupt") => {
                    s.interrupt(&app).await?;
                    json!({"ok":true})
                }
                ("POST", "stop") => {
                    s.stop().await;
                    json!({"ok":true})
                }
                ("POST", "permissions") => {
                    s.decide(&app, parts.get(4).copied().unwrap_or(""), &b)
                        .await?;
                    json!({"ok":true})
                }
                ("DELETE", "auto-allow") => {
                    if let Some(a) = s.meta.lock().unwrap()["auto_allow"].as_array_mut() {
                        a.retain(|v| v.as_str() != parts.get(4).copied());
                    }
                    m.touch(&s);
                    s.summary()
                }
                _ => return Err(Error::new(404, "Unknown route")),
            }
        }
    };
    Ok(Json(v).into_response())
}
