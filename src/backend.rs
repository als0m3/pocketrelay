use crate::{
    config::{self, now, text, Config},
    error::{Error, Result},
    App,
};
use futures_util::{Stream, StreamExt};
use serde_json::{json, Value};
use std::{
    collections::HashMap,
    path::PathBuf,
    pin::Pin,
    process::Stdio,
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        Arc, Mutex,
    },
    time::Duration,
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    process::{Child, ChildStdin, Command},
    sync::{mpsc, oneshot, Mutex as AsyncMutex},
};
use tokio_util::codec::{FramedRead, LinesCodec};

pub enum Event {
    Text(String),
    Done(Value),
}
pub type Output = Pin<Box<dyn Stream<Item = Result<Event>> + Send>>;
#[derive(Clone)]
pub struct Prompt {
    pub system: String,
    pub blocks: Vec<Value>,
    pub model: String,
    pub effort: String,
    pub schema: Option<Value>,
}
pub fn classify(message: &str) -> Error {
    let low = message.to_lowercase();
    let status = if [
        "rate limit",
        "rate_limit",
        "ratelimit",
        "quota",
        "429",
        "resource_exhausted",
    ]
    .iter()
    .any(|w| low.contains(w))
        || (low.contains("usage") && low.contains("limit"))
    {
        429
    } else if [
        "not logged in",
        "/login",
        "authenticat",
        "oauth",
        "401",
        "403",
        "token has expired",
        "invalid api key",
        "unauthorized",
        "credential",
        "sign in",
        "log in",
    ]
    .iter()
    .any(|w| low.contains(w))
    {
        503
    } else if [
        "invalid_request",
        "invalid_json_schema",
        "unknown model",
        "invalid schema",
        "invalid argument",
    ]
    .iter()
    .any(|w| low.contains(w))
    {
        400
    } else {
        502
    };
    Error::new(status, message.chars().take(1500).collect::<String>())
}
pub fn claude_env(token: Option<String>, full: bool) -> HashMap<String, String> {
    let mut env = config::clean_env(full);
    for k in [
        "CLAUDECODE",
        "CLAUDE_CODE_ENTRYPOINT",
        "CLAUDE_CODE_SSE_PORT",
    ] {
        env.remove(k);
    }
    if !config::flag("REMOTE_KEEP_API_KEY", false) {
        env.remove("ANTHROPIC_AUTH_TOKEN");
        env.remove("ANTHROPIC_API_KEY");
    }
    if let Some(t) = token {
        env.insert("CLAUDE_CODE_OAUTH_TOKEN".into(), t);
    }
    env.insert("DISABLE_AUTOUPDATER".into(), "1".into());
    env
}
pub fn command(binary: &str, env: HashMap<String, String>) -> Command {
    let mut c = Command::new(binary);
    c.env_clear()
        .envs(env)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    c
}
pub fn stderr(child: &mut Child) -> Arc<Mutex<String>> {
    let tail = Arc::new(Mutex::new(String::new()));
    if let Some(mut err) = child.stderr.take() {
        let t = tail.clone();
        tokio::spawn(async move {
            let mut buf = [0; 1024];
            while let Ok(n) = err.read(&mut buf).await {
                if n == 0 {
                    break;
                }
                let mut s = t.lock().unwrap();
                s.push_str(&String::from_utf8_lossy(&buf[..n]));
                if s.len() > 8192 {
                    let mut cut = s.len() - 4096;
                    while !s.is_char_boundary(cut) {
                        cut += 1;
                    }
                    s.drain(..cut);
                }
            }
        });
    }
    tail
}
pub fn lines(
    stdout: tokio::process::ChildStdout,
) -> FramedRead<tokio::process::ChildStdout, LinesCodec> {
    FramedRead::new(stdout, LinesCodec::new_with_max_length(32 * 1024 * 1024))
}

#[derive(Default)]
pub struct CodexPool {
    servers: AsyncMutex<HashMap<String, Arc<Codex>>>,
}
pub struct Codex {
    child: AsyncMutex<Child>,
    stdin: AsyncMutex<ChildStdin>,
    pending: Mutex<HashMap<u64, oneshot::Sender<Value>>>,
    threads: Mutex<HashMap<String, mpsc::Sender<Value>>>,
    next: AtomicU64,
    alive: AtomicBool,
    pub limits: Mutex<Value>,
    models: AsyncMutex<(f64, Vec<Value>)>,
}
impl CodexPool {
    pub async fn get(&self, c: &Config, account: &Value, home: PathBuf) -> Result<Arc<Codex>> {
        let mut pool = self.servers.lock().await;
        let id = text(account, "id");
        if let Some(s) = pool.get(id) {
            if s.alive.load(Ordering::SeqCst) {
                return Ok(s.clone());
            }
        }
        std::fs::create_dir_all(&home)?;
        let cwd = c.data.join("codex-cwd");
        std::fs::create_dir_all(&cwd)?;
        let mut env = config::clean_env(false);
        env.insert("CODEX_HOME".into(), home.to_string_lossy().into());
        let mut cmd = command(&c.codex, env);
        cmd.current_dir(cwd)
            .args(["app-server", "-c", "web_search=\"disabled\""]);
        for f in [
            "shell_tool",
            "apps",
            "browser_use",
            "browser_use_external",
            "computer_use",
            "image_generation",
            "in_app_browser",
            "memories",
            "goals",
        ] {
            cmd.args(["--disable", f]);
        }
        let mut child = cmd
            .spawn()
            .map_err(|e| Error::new(503, format!("Codex unavailable: {e}")))?;
        stderr(&mut child);
        let stdin = child.stdin.take().unwrap();
        let stdout = child.stdout.take().unwrap();
        let srv = Arc::new(Codex {
            child: AsyncMutex::new(child),
            stdin: AsyncMutex::new(stdin),
            pending: Mutex::default(),
            threads: Mutex::default(),
            next: AtomicU64::new(1),
            alive: AtomicBool::new(true),
            limits: Mutex::new(json!({})),
            models: AsyncMutex::new((0., vec![])),
        });
        let reader = srv.clone();
        tokio::spawn(async move {
            let mut input = lines(stdout);
            while let Some(Ok(line)) = input.next().await {
                let Ok(v) = serde_json::from_str::<Value>(&line) else {
                    continue;
                };
                if let Some(id) = v["id"].as_u64() {
                    if v.get("result").is_some() || v.get("error").is_some() {
                        if let Some(tx) = reader.pending.lock().unwrap().remove(&id) {
                            let _ = tx.send(v);
                        }
                        continue;
                    }
                }
                if v.get("id").is_some() {
                    let _ = reader
                        .write(&json!({"id":v["id"],"result":{"decision":"decline"}}))
                        .await;
                    continue;
                }
                if text(&v, "method") == "account/rateLimits/updated" {
                    let mut limits = v["params"]["rateLimits"].clone();
                    limits["seen_at"] = json!(now());
                    *reader.limits.lock().unwrap() = limits;
                }
                let tid = text(&v["params"], "threadId");
                if !tid.is_empty() {
                    let mut threads = reader.threads.lock().unwrap();
                    if let Some(tx) = threads.get(tid) {
                        if tx.try_send(v.clone()).is_err() {
                            threads.remove(tid);
                        }
                    }
                }
            }
            reader.alive.store(false, Ordering::SeqCst);
            reader.pending.lock().unwrap().clear();
            reader.threads.lock().unwrap().clear();
            let _ = reader.child.lock().await.kill().await;
        });
        if let Err(e) = srv
            .call(
                "initialize",
                json!({"clientInfo":{"name":"pocketrelay","version":"0.2.0"}}),
            )
            .await
        {
            srv.stop().await;
            return Err(e);
        }
        srv.write(&json!({"method":"initialized"})).await?;
        pool.insert(id.into(), srv.clone());
        Ok(srv)
    }
    pub async fn stop(&self, id: &str) {
        if let Some(s) = self.servers.lock().await.remove(id) {
            s.stop().await;
        }
    }
    pub async fn shutdown(&self) {
        let mut pool = self.servers.lock().await;
        for (_, s) in pool.drain() {
            s.stop().await;
        }
    }
}
impl Codex {
    pub async fn write(&self, v: &Value) -> Result<()> {
        let mut stdin = self.stdin.lock().await;
        let mut b = serde_json::to_vec(v)?;
        b.push(b'\n');
        stdin.write_all(&b).await?;
        stdin.flush().await?;
        Ok(())
    }
    pub async fn call(&self, method: &str, params: Value) -> Result<Value> {
        let id = self.next.fetch_add(1, Ordering::SeqCst);
        let (tx, rx) = oneshot::channel();
        self.pending.lock().unwrap().insert(id, tx);
        struct Pending<'a>(&'a Mutex<HashMap<u64, oneshot::Sender<Value>>>, u64);
        impl Drop for Pending<'_> {
            fn drop(&mut self) {
                self.0.lock().unwrap().remove(&self.1);
            }
        }
        let _guard = Pending(&self.pending, id);
        self.write(&json!({"id":id,"method":method,"params":params}))
            .await?;
        let r = tokio::time::timeout(Duration::from_secs(60), rx)
            .await
            .map_err(|_| Error::new(504, "Codex timed out"))?
            .map_err(|_| Error::new(502, "Codex stopped"))?;
        if let Some(e) = r.get("error") {
            return Err(classify(&e.to_string()));
        }
        Ok(r.get("result").cloned().unwrap_or(json!({})))
    }
    pub async fn models(&self) -> Result<Vec<Value>> {
        let mut cache = self.models.lock().await;
        if now() - cache.0 < 600. && !cache.1.is_empty() {
            return Ok(cache.1.clone());
        }
        let r = self.call("model/list", json!({})).await?;
        cache.1 = r["data"]
            .as_array()
            .cloned()
            .unwrap_or_default()
            .into_iter()
            .filter(|m| m["hidden"] != true)
            .collect();
        cache.0 = now();
        Ok(cache.1.clone())
    }
    async fn stop(&self) {
        self.alive.store(false, Ordering::SeqCst);
        let _ = self.child.lock().await.kill().await;
        self.pending.lock().unwrap().clear();
        self.threads.lock().unwrap().clear();
    }
}
struct ThreadGuard {
    srv: Arc<Codex>,
    id: String,
    finished: bool,
}
impl Drop for ThreadGuard {
    fn drop(&mut self) {
        self.srv.threads.lock().unwrap().remove(&self.id);
        let s = self.srv.clone();
        let id = self.id.clone();
        let finished = self.finished;
        tokio::spawn(async move {
            if !finished {
                let _ = tokio::time::timeout(
                    Duration::from_secs(5),
                    s.call("turn/interrupt", json!({"threadId":id})),
                )
                .await;
            }
            let _ = tokio::time::timeout(
                Duration::from_secs(5),
                s.call("thread/unsubscribe", json!({"threadId":id})),
            )
            .await;
        });
    }
}
struct Purge {
    home: PathBuf,
    cid: String,
}
impl Drop for Purge {
    fn drop(&mut self) {
        if self.cid.len() != 36 || !self.cid.chars().all(|c| c.is_ascii_hexdigit() || c == '-') {
            return;
        }
        let base = self.home.join(".gemini/antigravity-cli");
        if let Ok(dirs) = std::fs::read_dir(&base) {
            for d in dirs.flatten().filter(|e| e.path().is_dir()) {
                if let Ok(files) = std::fs::read_dir(d.path()) {
                    for f in files.flatten() {
                        if f.file_name().to_string_lossy().starts_with(&self.cid) {
                            if f.path().is_dir() {
                                let _ = std::fs::remove_dir_all(f.path());
                            } else {
                                let _ = std::fs::remove_file(f.path());
                            }
                        }
                    }
                }
            }
        }
        for file in [
            "jetbox_summaries_proto.pb",
            "cache/last_conversations.json",
            "cache/conversation_metadata.json",
        ] {
            let _ = std::fs::remove_file(base.join(file));
        }
        let db = base.join("conversation_summaries.db");
        if db.is_file() {
            if let Ok(c) = rusqlite::Connection::open(db) {
                let _ = c.busy_timeout(Duration::from_secs(2));
                let _ = c.execute(
                    "DELETE FROM conversation_summaries WHERE conversation_id = ?",
                    [&self.cid],
                );
            }
        }
    }
}
// On cancellation, wait for the CLI to exit before deleting its files.
// A kill_on_drop followed by cleanup alone could let the CLI write files after cleanup.
struct RequestProcess {
    child: Option<Child>,
    purge: Option<Purge>,
    temp: Option<tempfile::TempDir>,
}
impl std::ops::Deref for RequestProcess {
    type Target = Child;
    fn deref(&self) -> &Child {
        self.child.as_ref().unwrap()
    }
}
impl std::ops::DerefMut for RequestProcess {
    fn deref_mut(&mut self) -> &mut Child {
        self.child.as_mut().unwrap()
    }
}
impl RequestProcess {
    async fn finish(mut self) {
        if let Some(mut child) = self.child.take() {
            let _ = child.kill().await;
            let _ = child.wait().await;
        }
        let cleanup = (self.purge.take(), self.temp.take());
        let _ = tokio::task::spawn_blocking(move || drop(cleanup)).await;
    }
}
impl Drop for RequestProcess {
    fn drop(&mut self) {
        let Some(mut child) = self.child.take() else {
            return;
        };
        let cleanup = (self.purge.take(), self.temp.take());
        tokio::spawn(async move {
            let _ = child.kill().await;
            let _ = child.wait().await;
            let _ = tokio::task::spawn_blocking(move || drop(cleanup)).await;
        });
    }
}
pub fn stream(app: Arc<App>, account: Value, p: Prompt) -> Output {
    Box::pin(async_stream::try_stream! {
        let provider = text(&account, "provider").to_string();
        if provider == "codex" {
            let home = app.store.lock().unwrap().cli_home(&account, "codex");
            let srv = app.codex.get(&app.config, &account, home).await?;
            let blocks = crate::content::expand_documents(p.blocks).await?;
            let inputs:Vec<Value>=blocks.into_iter().filter_map(|b|match text(&b,"type"){"text"=>Some(json!({"type":"text","text":b["text"]})),"image"=>Some(json!({"type":"image","url":format!("data:{};base64,{}",text(&b["source"],"media_type"),text(&b["source"],"data")),"detail":b.get("detail").unwrap_or(&json!("auto"))})),_=>None}).collect();
            let cwd = app.config.data.join("codex-cwd");
            let r=srv.call("thread/start",json!({"baseInstructions":p.system,"ephemeral":true,"sandbox":"read-only","approvalPolicy":"never","cwd":cwd,"model":p.model})).await?;
            let tid = text(&r["thread"], "id").to_string();
            if tid.is_empty() {
                Err(Error::new(502, "Codex did not return a thread"))?;
            }
            let (tx, mut rx) = mpsc::channel(256);
            srv.threads.lock().unwrap().insert(tid.clone(), tx);
            let mut guard = ThreadGuard {
                srv: srv.clone(),
                id: tid.clone(),
                finished: false,
            };
            let mut params = json!({"threadId":tid,"input":inputs,"model":p.model});
            if !p.effort.is_empty() {
                params["effort"] = json!(if ["none", "minimal"].contains(&p.effort.as_str()) {
                    "low"
                } else {
                    &p.effort
                });
            }
            if let Some(schema) = p.schema {
                params["outputSchema"] = schema;
            }
            srv.call("turn/start", params).await?;
            let mut usage = json!({});
            let mut last_item = String::new();
            while let Some(v) = rx.recv().await {
                let params = &v["params"];
                match text(&v, "method") {
                    "item/agentMessage/delta" => {
                        let item = text(params, "itemId");
                        if !last_item.is_empty() && item != last_item {
                            yield Event::Text("\n\n".into());
                        }
                        last_item = item.into();
                        yield Event::Text(text(params,"delta").into());
                    }
                    "thread/tokenUsage/updated" => {
                        let u = &params["tokenUsage"]["last"];
                        let cached = u["cachedInputTokens"].as_u64().unwrap_or(0);
                        usage = json!({"input_tokens":u["inputTokens"].as_u64().unwrap_or(0).saturating_sub(cached),"cache_read_input_tokens":cached,"output_tokens":u["outputTokens"].as_u64().unwrap_or(0),"output_tokens_details":{"thinking_tokens":u["reasoningOutputTokens"].as_u64().unwrap_or(0)}});
                    }
                    "error" if params["willRetry"] != true => {
                        Err(classify(&params["error"].to_string()))?;
                    }
                    "turn/completed" => {
                        if params["turn"]["status"] == "failed" {
                            Err(classify(&params["turn"]["error"].to_string()))?;
                        }
                        guard.finished = true;
                        break;
                    }
                    _ => {}
                }
            }
            if !guard.finished {
                Err(Error::new(502, "Codex stream interrupted or client too slow"))?;
            }
            {
                let mut store = app.store.lock().unwrap();
                store.state(text(&account, "id"))["limits"] = srv.limits.lock().unwrap().clone();
            }
            yield Event::Done(json!({"model":p.model,"usage":usage}));
        } else {
            let base = app.config.data.join(if provider == "claude" {
                "oai-cwd"
            } else {
                "antigravity-cwd"
            });
            std::fs::create_dir_all(&base)?;
            let tmp = tempfile::Builder::new()
                .prefix("request-")
                .tempdir_in(base)?;
            let mut purge = Purge {
                home: PathBuf::new(),
                cid: String::new(),
            };
            if p.system.contains('\0') || p.model.contains('\0') || p.effort.contains('\0') {
                Err(Error::new(400, "NUL characters are not allowed in CLI parameters"))?;
            }
            if provider == "claude" && p.system.len() > 120 * 1024 {
                Err(Error::new(400, "System instructions are too long (Claude limit: 120 KiB)."))?;
            }
            let mut cmd = if provider == "claude" {
                let token = app.store.lock().unwrap().token(&account);
                let mut cmd = command(&app.config.claude, claude_env(token, false));
                cmd.args([
                    "-p",
                    "--input-format",
                    "stream-json",
                    "--output-format",
                    "stream-json",
                    "--verbose",
                    "--include-partial-messages",
                    "--tools",
                    "",
                    "--system-prompt",
                    &p.system,
                    "--no-session-persistence",
                    "--safe-mode",
                    "--permission-mode",
                    "dontAsk",
                    "--model",
                    &p.model,
                ]);
                if !p.effort.is_empty() {
                    cmd.args([
                        "--effort",
                        if ["none", "minimal"].contains(&p.effort.as_str()) {
                            "low"
                        } else {
                            &p.effort
                        },
                    ]);
                }
                cmd
            } else {
                let home = app.store.lock().unwrap().cli_home(&account, "antigravity");
                std::fs::create_dir_all(&home)?;
                purge.home = home.clone();
                let mut env = config::clean_env(false);
                env.insert("HOME".into(), home.to_string_lossy().into());
                env.insert("NO_COLOR".into(), "1".into());
                env.insert("TERM".into(), "dumb".into());
                if p.blocks.iter().any(|b| b["type"] == "image") {
                    Err(Error::new(
                        400,
                        "Antigravity does not accept image input through this gateway.",
                    ))?;
                }
                let blocks = crate::content::expand_documents(p.blocks.clone()).await?;
                let omitted = blocks.iter().filter(|b| b["type"] == "image").count();
                let mut prompt = format!(
                    "<system_instructions>\n{}\n</system_instructions>\n\n{}",
                    p.system,
                    crate::content::text_of(&blocks)
                );
                if omitted > 0 {
                    prompt+=&format!("\n[{omitted} page(s) without extractable text omitted: this provider does not accept images.]");
                }
                if prompt.contains('\0') || prompt.len() > 120 * 1024 {
                    Err(Error::new(400, "Antigravity conversation exceeds 120 KiB or contains a NUL character."))?;
                }
                let mut cmd = command(&app.config.antigravity, env);
                cmd.args([
                    "-p",
                    &prompt,
                    "--output-format",
                    "stream-json",
                    "--model",
                    &p.model,
                    "--disable-slash-commands",
                    "--log-file",
                ])
                .arg(tmp.path().join("cli.log"))
                .stdin(Stdio::null());
                if ["low", "medium", "high"].contains(&p.effort.as_str()) {
                    cmd.args(["--effort", &p.effort]);
                }
                if let Some(schema) = &p.schema {
                    cmd.args(["--json-schema", &schema.to_string()]);
                }
                cmd
            };
            let child = cmd
                .current_dir(tmp.path())
                .spawn()
                .map_err(|e| Error::new(503, format!("CLI {provider} unavailable: {e}")))?;
            let mut child = RequestProcess { child: Some(child), purge: Some(purge), temp: Some(tmp) };
            let tail = stderr(&mut child);
            if provider == "claude" {
                let mut stdin = child.stdin.take().unwrap();
                let mut b = serde_json::to_vec(
                    &json!({"type":"user","message":{"role":"user","content":p.blocks},"parent_tool_use_id":null,"session_id":""}),
                )?;
                b.push(b'\n');
                stdin.write_all(&b).await?;
                stdin.shutdown().await?;
            }
            let mut lines = lines(child.stdout.take().unwrap());
            let mut streamed = false;
            let mut done = false;
            let mut model = p.model.clone();
            let mut usage = json!({});
            while let Some(line) = lines.next().await {
                let line =
                    line.map_err(|_| Error::new(502, "Invalid or oversized CLI output"))?;
                let Ok(v) = serde_json::from_str::<Value>(&line) else {
                    continue;
                };
                if provider == "claude" {
                    match text(&v, "type") {
                        "stream_event" if v["event"]["delta"]["type"] == "text_delta" => {
                            streamed = true;
                            yield Event::Text(text(&v["event"]["delta"],"text").into());
                        }
                        "system" if v["subtype"] == "init" => {
                            if let Some(m) = v["model"].as_str() {
                                model = m.into();
                            }
                        }
                        "rate_limit_event" => {
                            app.store.lock().unwrap().state(text(&account, "id"))["limits"] =
                                v["rate_limit_info"].clone();
                        }
                        "result" => {
                            if v["is_error"] == true {
                                Err(classify(text(&v, "result")))?;
                            }
                            if !streamed && !text(&v, "result").is_empty() {
                                yield Event::Text(text(&v,"result").into());
                            }
                            usage = v["usage"].clone();
                            done = true;
                            break;
                        }
                        _ => {}
                    }
                } else {
                    let purge = child.purge.as_mut().unwrap();
                    if purge.cid.is_empty() {
                        purge.cid = if !text(&v, "conversation_id").is_empty() {
                            text(&v, "conversation_id").into()
                        } else {
                            text(&v["result"], "conversation_id").into()
                        };
                    }
                    match text(&v, "event") {
                        "step_update" => {
                            let d = if v["step_update"].is_object() {
                                &v["step_update"]
                            } else {
                                &v
                            };
                            if let Some(t) = d["text_delta"].as_str() {
                                streamed = true;
                                yield Event::Text(t.into());
                            }
                        }
                        "init" => {
                            let i = if v["init"].is_object() {
                                &v["init"]
                            } else {
                                &v
                            };
                            if let Some(m) = i["model"].as_str() {
                                model = m.into();
                            }
                        }
                        "result" => {
                            let r = if v["result"].is_object() {
                                &v["result"]
                            } else {
                                &v
                            };
                            if text(r, "status").to_uppercase() != "SUCCESS" {
                                Err(classify(&r["error"].to_string()))?;
                            }
                            if !streamed && !text(r, "response").is_empty() {
                                yield Event::Text(text(r,"response").into());
                            }
                            let u = &r["usage"];
                            let cached = u["cache_read_tokens"].as_u64().unwrap_or(0);
                            usage = json!({"input_tokens":u["input_tokens"].as_u64().unwrap_or(0).saturating_sub(cached),"cache_read_input_tokens":cached,"output_tokens":u["output_tokens"].as_u64().unwrap_or(0),"output_tokens_details":{"thinking_tokens":u["thinking_tokens"].as_u64().unwrap_or(0)}});
                            done = true;
                            break;
                        }
                        _ => {}
                    }
                }
            }
            child.finish().await;
            if !done {
                let err = tail.lock().unwrap().clone();
                Err(Error::new(
                    502,
                    format!("{provider} stopped without a result. {err}"),
                ))?;
            }
            yield Event::Done(json!({"model":model,"usage":usage}));
        }
    })
}
