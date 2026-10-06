use crate::{
    backend,
    config::{self, now, text, Config},
    error::{Error, Result},
    App,
};
use futures_util::StreamExt;
use serde_json::{json, Value};
use std::{
    collections::{HashMap, VecDeque},
    io::{BufRead, Write},
    path::PathBuf,
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::io::AsyncWriteExt;
use tokio::sync::{broadcast, mpsc, oneshot, Mutex as AsyncMutex};
pub const MODES: [&str; 6] = [
    "manual",
    "acceptEdits",
    "auto",
    "plan",
    "dontAsk",
    "bypassPermissions",
];
pub const EFFORTS: [&str; 6] = ["", "low", "medium", "high", "xhigh", "max"];
pub struct Manager {
    pub items: Mutex<HashMap<String, Arc<Session>>>,
    pub events: broadcast::Sender<Value>,
    pub limits: Mutex<Value>,
    data: PathBuf,
}
pub struct Session {
    pub meta: Mutex<Value>,
    pub pending: Mutex<HashMap<String, Value>>,
    pub events: broadcast::Sender<Value>,
    log: Mutex<Vec<Value>>,
    path: PathBuf,
    process: AsyncMutex<Option<Process>>,
    spawn: AsyncMutex<()>,
}
struct Process {
    tx: mpsc::Sender<Value>,
    stop: oneshot::Sender<()>,
    done: oneshot::Receiver<()>,
}
impl Manager {
    pub fn new(c: &Config) -> anyhow::Result<Self> {
        let mut items = HashMap::new();
        std::fs::create_dir_all(c.data.join("events"))?;
        for mut meta in config::load(&c.data.join("sessions.json"), json!([]))?
            .as_array()
            .cloned()
            .unwrap_or_default()
        {
            meta["status"] = json!("idle");
            let id = text(&meta, "id").to_string();
            if !id.chars().all(|v| v.is_ascii_alphanumeric() || v == '-') {
                anyhow::bail!("Invalid session ID")
            };
            items.insert(id, Session::new(meta, &c.data)?);
        }
        Ok(Self {
            items: Mutex::new(items),
            events: broadcast::channel(256).0,
            limits: Mutex::new(config::load(&c.data.join("limits.json"), json!({}))?),
            data: c.data.clone(),
        })
    }
    pub fn get(&self, id: &str) -> Result<Arc<Session>> {
        self.items
            .lock()
            .unwrap()
            .get(id)
            .cloned()
            .ok_or_else(|| Error::new(404, "Unknown session"))
    }
    pub fn list(&self) -> Vec<Value> {
        let mut v = self
            .items
            .lock()
            .unwrap()
            .values()
            .map(|s| s.summary())
            .collect::<Vec<_>>();
        v.sort_by(|a, b| {
            b["pinned"]
                .as_bool()
                .unwrap_or(false)
                .cmp(&a["pinned"].as_bool().unwrap_or(false))
                .then_with(|| {
                    b["updated"]
                        .as_f64()
                        .unwrap_or(0.)
                        .total_cmp(&a["updated"].as_f64().unwrap_or(0.))
                })
        });
        v
    }
    pub fn save(&self) -> Result<()> {
        let m = self
            .items
            .lock()
            .unwrap()
            .values()
            .map(|s| s.meta.lock().unwrap().clone())
            .collect::<Vec<_>>();
        config::save(&self.data.join("sessions.json"), &json!(m))?;
        Ok(())
    }
    pub fn touch(&self, s: &Session) {
        s.meta.lock().unwrap()["updated"] = json!(now());
        if let Err(e) = self.save() {
            eprintln!("Sessions: {}", e.message);
        }
        let _ = self
            .events
            .send(json!({"type":"session_update","session":s.summary()}));
    }
    pub fn create(&self, b: &Value) -> Result<Arc<Session>> {
        validate(b)?;
        let cwd = config::expand(b["cwd"].as_str().unwrap_or("~"))
            .canonicalize()
            .map_err(|_| Error::new(400, "Directory not found"))?;
        if !cwd.is_dir() {
            return Err(Error::new(400, "Not a directory"));
        }
        let resume = text(b, "resume");
        if !resume.is_empty()
            && !resume
                .chars()
                .all(|v| v.is_ascii_alphanumeric() || v == '-')
        {
            return Err(Error::new(400, "Invalid resume identifier"));
        }
        if !resume.is_empty() && b["fork"] != true {
            if let Some(s) = self
                .items
                .lock()
                .unwrap()
                .values()
                .find(|s| s.meta.lock().unwrap()["claude_session_id"] == resume)
            {
                return Ok(s.clone());
            }
        }
        let id = config::random(6);
        let name = if text(b, "name").is_empty() {
            cwd.file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .to_string()
        } else {
            text(b, "name").into()
        };
        let meta = json!({"id":id,"name":name,"cwd":cwd,"model":text(b,"model"),"permission_mode":b["permission_mode"].as_str().unwrap_or("manual"),"effort":text(b,"effort"),"append_system_prompt":text(b,"append_system_prompt"),"add_dirs":b["add_dirs"].as_array().cloned().unwrap_or_default(),"claude_session_id":if resume.is_empty(){uuid::Uuid::new_v4().to_string()}else{resume.into()},"started":!resume.is_empty(),"fork_on_resume":!resume.is_empty()&&b["fork"]==true,"created":now(),"updated":now(),"status":"idle","cost_usd":0.,"turns":0,"auto_allow":[]});
        let s = Session::new(meta, &self.data)?;
        self.items.lock().unwrap().insert(id, s.clone());
        if !resume.is_empty() {
            for e in import(resume) {
                s.emit(e, true);
            }
            s.emit(json!({"type":"remote","event":"resumed","claude_session_id":resume,"fork":b["fork"]==true}),true);
        }
        self.touch(&s);
        Ok(s)
    }
    pub async fn delete(&self, id: &str) -> Result<()> {
        let s = self.get(id)?;
        s.stop().await;
        self.items.lock().unwrap().remove(id);
        match std::fs::remove_file(&s.path) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e.into()),
        };
        self.save()?;
        let _ = self.events.send(json!({"type":"session_deleted","id":id}));
        Ok(())
    }
    pub async fn shutdown(&self) {
        let all = self
            .items
            .lock()
            .unwrap()
            .values()
            .cloned()
            .collect::<Vec<_>>();
        for s in all {
            s.stop().await;
        }
        let _ = self.save();
    }
}
fn validate(b: &Value) -> Result<()> {
    if let Some(m) = b.get("permission_mode") {
        if !m.as_str().is_some_and(|m| MODES.contains(&m)) {
            return Err(Error::new(400, "Unknown permission mode"));
        }
    }
    if let Some(e) = b.get("effort") {
        if !e.as_str().is_some_and(|e| EFFORTS.contains(&e)) {
            return Err(Error::new(400, "Unknown effort"));
        }
    }
    for k in ["name", "cwd", "model", "append_system_prompt", "resume"] {
        if b.get(k).is_some_and(|v| !v.is_string()) {
            return Err(Error::new(400, format!("{k} must be text")));
        }
    }
    Ok(())
}
fn read_log(path: &std::path::Path) -> Vec<Value> {
    std::fs::File::open(path)
        .map(|f| {
            std::io::BufReader::new(f)
                .lines()
                .map_while(std::result::Result::ok)
                .filter_map(|s| serde_json::from_str(&s).ok())
                .collect()
        })
        .unwrap_or_default()
}
impl Session {
    fn new(mut meta: Value, data: &std::path::Path) -> Result<Arc<Self>> {
        if !meta["auto_allow"].is_array() {
            meta["auto_allow"] = json!([]);
        }
        let path = data
            .join("events")
            .join(format!("{}.jsonl", text(&meta, "id")));
        let log = read_log(&path);
        Ok(Arc::new(Self {
            meta: Mutex::new(meta),
            pending: Mutex::default(),
            events: broadcast::channel(512).0,
            log: Mutex::new(log),
            path,
            process: AsyncMutex::new(None),
            spawn: AsyncMutex::new(()),
        }))
    }
    pub fn summary(&self) -> Value {
        let mut m = self.meta.lock().unwrap().clone();
        m["alive"] = json!(m["_alive"] == true);
        m.as_object_mut().unwrap().remove("_alive");
        m["pending"] = json!(self.pending.lock().unwrap().keys().collect::<Vec<_>>());
        m
    }
    pub fn history(&self) -> Vec<Value> {
        self.log.lock().unwrap().clone()
    }
    fn emit(&self, mut e: Value, persist: bool) {
        if persist {
            let mut log = self.log.lock().unwrap();
            e["seq"] = json!(log.last().and_then(|v| v["seq"].as_u64()).unwrap_or(0) + 1);
            e["ts"] = json!(now());
            let res = (|| -> std::io::Result<()> {
                let mut options = std::fs::OpenOptions::new();
                options.create(true).append(true);
                #[cfg(unix)]
                {
                    use std::os::unix::fs::OpenOptionsExt;
                    options.mode(0o600);
                }
                let mut f = options.open(&self.path)?;
                writeln!(f, "{e}")
            })();
            if let Err(err) = res {
                eprintln!("Session log: {err}");
            }
            log.push(e.clone());
        }
        let _ = self.events.send(e);
    }
    fn status(&self, app: &App, status: &str) {
        self.meta.lock().unwrap()["status"] = json!(status);
        self.emit(
            json!({"type":"remote","event":"status","status":status}),
            false,
        );
        app.sessions.touch(self);
    }
    async fn write(&self, v: Value) -> Result<()> {
        let tx = self
            .process
            .lock()
            .await
            .as_ref()
            .map(|p| p.tx.clone())
            .ok_or_else(|| Error::new(409, "Claude process has not started"))?;
        tokio::time::timeout(Duration::from_secs(10), tx.send(v))
            .await
            .map_err(|_| Error::new(504, "CLI unavailable"))?
            .map_err(|_| Error::new(502, "CLI stopped"))
    }
    async fn control(&self, v: Value) -> Result<()> {
        self.write(json!({"type":"control_request","request_id":config::id("req_"),"request":v}))
            .await
    }
    async fn ensure(self: &Arc<Self>, app: Arc<App>) -> Result<()> {
        let _spawn = self.spawn.lock().await;
        if self.meta.lock().unwrap()["_alive"] == true {
            return Ok(());
        }
        self.status(&app, "starting");
        let m = self.meta.lock().unwrap().clone();
        let token = {
            let st = app.store.lock().unwrap();
            st.accounts
                .iter()
                .find(|a| a["provider"] == "claude" && st.available(a))
                .and_then(|a| st.token(a))
        };
        let mut cmd = backend::command(&app.config.claude, backend::claude_env(token, true));
        cmd.current_dir(text(&m, "cwd")).args([
            "-p",
            "--input-format",
            "stream-json",
            "--output-format",
            "stream-json",
            "--verbose",
            "--include-partial-messages",
            "--permission-prompts",
            "host",
            "--permission-mode",
            text(&m, "permission_mode"),
        ]);
        if m["permission_mode"] == "bypassPermissions" {
            cmd.arg("--allow-dangerously-skip-permissions");
        }
        for (k, arg) in [
            ("model", "--model"),
            ("effort", "--effort"),
            ("append_system_prompt", "--append-system-prompt"),
        ] {
            if !text(&m, k).is_empty() {
                cmd.args([arg, text(&m, k)]);
            }
        }
        for d in m["add_dirs"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
        {
            cmd.args(["--add-dir", d]);
        }
        cmd.args([
            if m["started"] == true {
                "--resume"
            } else {
                "--session-id"
            },
            text(&m, "claude_session_id"),
        ]);
        if m["fork_on_resume"] == true {
            cmd.arg("--fork-session");
        }
        let mut child = match cmd.spawn() {
            Ok(c) => c,
            Err(e) => {
                self.status(&app, "error");
                return Err(e.into());
            }
        };
        let stderr = backend::stderr(&mut child);
        let mut out = backend::lines(child.stdout.take().unwrap());
        let mut stdin = child.stdin.take().unwrap();
        let (tx, mut rx) = mpsc::channel::<Value>(32);
        let (stop, mut stopped) = oneshot::channel();
        let (done, finished) = oneshot::channel();
        *self.process.lock().await = Some(Process {
            tx,
            stop,
            done: finished,
        });
        {
            let mut m = self.meta.lock().unwrap();
            m["_alive"] = json!(true);
            m["started"] = json!(true);
            m["fork_on_resume"] = json!(false);
        }
        self.status(&app, "idle");
        let s = self.clone();
        tokio::spawn(async move {
            let mut cost = 0.;
            let init = json!({"type":"control_request","request_id":"initialize","request":{"subtype":"initialize"}});
            let _ = stdin.write_all(format!("{init}\n").as_bytes()).await;
            loop {
                tokio::select! {_=&mut stopped=>{let _=child.kill().await;break},v=rx.recv()=>{let Some(v)=v else{break};if stdin.write_all(format!("{v}\n").as_bytes()).await.is_err(){break}},line=out.next()=>{match line{Some(Ok(line))=>{if let Ok(o)=serde_json::from_str::<Value>(&line){if let Some(reply)=s.handle(&app,o,&mut cost){if stdin.write_all(format!("{reply}\n").as_bytes()).await.is_err(){break}}}else{s.emit(json!({"type":"remote","event":"raw","text":line.chars().take(2000).collect::<String>()}),true)}},_=>break}}}
            }
            let status = match tokio::time::timeout(Duration::from_secs(3), child.wait()).await {
                Ok(Ok(s)) => s.code(),
                _ => {
                    let _ = child.kill().await;
                    None
                }
            };
            s.meta.lock().unwrap()["_alive"] = json!(false);
            let ids = s
                .pending
                .lock()
                .unwrap()
                .drain()
                .map(|(id, _)| id)
                .collect::<Vec<_>>();
            for id in ids {
                s.emit(
                    json!({"type":"permission_resolved","request_id":id,"behavior":"cancelled"}),
                    true,
                );
            }
            s.emit(json!({"type":"remote","event":"exited","code":status,"stderr":stderr.lock().unwrap().clone()}),true);
            s.status(
                &app,
                if status.is_some_and(|c| c != 0) {
                    "error"
                } else {
                    "idle"
                },
            );
            let _ = done.send(());
        });
        Ok(())
    }
    fn handle(&self, app: &App, o: Value, cost: &mut f64) -> Option<Value> {
        match text(&o, "type") {
            "control_request" => {
                let req = &o["request"];
                let rid = text(&o, "request_id");
                if req["subtype"] != "can_use_tool" {
                    return Some(
                        json!({"type":"control_response","response":{"subtype":"error","request_id":rid,"error":"Unsupported subtype"}}),
                    );
                }
                if self.meta.lock().unwrap()["auto_allow"]
                    .as_array()
                    .is_some_and(|v| v.contains(&req["tool_name"]))
                {
                    self.emit(json!({"type":"permission_auto","tool_name":req["tool_name"],"input":req["input"]}),true);
                    return Some(
                        json!({"type":"control_response","response":{"subtype":"success","request_id":rid,"response":{"behavior":"allow","updatedInput":req["input"]}}}),
                    );
                }
                self.pending.lock().unwrap().insert(rid.into(), req.clone());
                self.emit(json!({"type":"permission_request","request_id":rid,"tool_name":req["tool_name"],"input":req["input"],"tool_use_id":req["tool_use_id"],"suggestions":req["permission_suggestions"],"decision_reason":req["decision_reason"],"blocked_path":req["blocked_path"]}),true);
                self.status(app, "waiting");
            }
            "control_response" => {
                if o["response"]["request_id"] == "initialize" {
                    let r = &o["response"]["response"];
                    {
                        let mut m = self.meta.lock().unwrap();
                        for k in ["commands", "models"] {
                            if r[k].is_array() {
                                m[k] = r[k].clone();
                            }
                        }
                    }
                    app.sessions.touch(self);
                }
            }
            "stream_event" => self.emit(o, false),
            "keep_alive" => {}
            "rate_limit_event" => {
                let mut info = o["rate_limit_info"].clone();
                if info.is_object() {
                    info["seen_at"] = json!(now());
                    *app.sessions.limits.lock().unwrap() = info.clone();
                    let _ = config::save(&app.config.data.join("limits.json"), &info);
                    let _ = app
                        .sessions
                        .events
                        .send(json!({"type":"limits","limits":info}));
                }
            }
            "result" => {
                let total = o["total_cost_usd"].as_f64().unwrap_or(0.);
                {
                    let mut m = self.meta.lock().unwrap();
                    m["cost_usd"] =
                        json!(m["cost_usd"].as_f64().unwrap_or(0.) + (total - *cost).max(0.));
                    m["turns"] = json!(m["turns"].as_u64().unwrap_or(0) + 1);
                    m["last_result"] = json!({"is_error":o["is_error"],"subtype":o["subtype"],"duration_ms":o["duration_ms"]});
                }
                *cost = total;
                self.emit(o, true);
                let waiting = !self.pending.lock().unwrap().is_empty();
                self.status(app, if waiting { "waiting" } else { "idle" });
            }
            _ => {
                if o["type"] == "system" && o["subtype"] == "init" {
                    let mut m = self.meta.lock().unwrap();
                    if o["session_id"].is_string() {
                        m["claude_session_id"] = o["session_id"].clone();
                    }
                    m["active_model"] = o["model"].clone();
                    m["tools"] = o["tools"].clone();
                } else if o["type"] == "assistant" {
                    let usage = &o["message"]["usage"];
                    let mut m = self.meta.lock().unwrap();
                    m["context_tokens"] = json!([
                        "input_tokens",
                        "cache_read_input_tokens",
                        "cache_creation_input_tokens"
                    ]
                    .iter()
                    .map(|k| usage[k].as_u64().unwrap_or(0))
                    .sum::<u64>());
                    for b in o["message"]["content"].as_array().into_iter().flatten() {
                        if b["type"] == "text" {
                            m["preview"] =
                                json!(text(b, "text").chars().take(140).collect::<String>());
                        }
                    }
                }
                self.emit(o, true);
                app.sessions.touch(self);
            }
        }
        None
    }
    pub async fn send(self: &Arc<Self>, app: Arc<App>, body: &Value) -> Result<()> {
        let t = text(body, "text");
        let images = body["images"].as_array().cloned().unwrap_or_default();
        if t.trim().is_empty() && images.is_empty() {
            return Err(Error::new(400, "Empty message"));
        }
        let content = if images.is_empty() {
            json!(t)
        } else {
            let mut blocks = vec![];
            for im in images {
                if !text(&im, "media_type").starts_with("image/") || text(&im, "data").is_empty() {
                    return Err(Error::new(400, "Invalid image"));
                }
                blocks.push(json!({"type":"image","source":{"type":"base64","media_type":im["media_type"],"data":im["data"]}}));
            }
            if !t.trim().is_empty() {
                blocks.push(json!({"type":"text","text":t}));
            }
            json!(blocks)
        };
        self.ensure(app.clone()).await?;
        let sid = self.meta.lock().unwrap()["claude_session_id"].clone();
        self.status(&app, "running");
        self.emit(
            json!({"type":"user","message":{"role":"user","content":content},"local":true}),
            true,
        );
        self.write(json!({"type":"user","message":{"role":"user","content":content},"parent_tool_use_id":null,"session_id":sid})).await
    }
    pub async fn decide(&self, app: &App, rid: &str, b: &Value) -> Result<()> {
        let behavior = text(b, "behavior");
        if !["allow", "deny"].contains(&behavior) {
            return Err(Error::new(400, "Invalid decision"));
        }
        let req = self
            .pending
            .lock()
            .unwrap()
            .get(rid)
            .cloned()
            .ok_or_else(|| Error::new(404, "Unknown or expired permission request"))?;
        let decision = if behavior == "allow" {
            json!({"behavior":"allow","updatedInput":b.get("updated_input").filter(|v|v.is_object()).unwrap_or(&req["input"])})
        } else {
            json!({"behavior":"deny","message":if text(b,"message").is_empty(){"Denied from PocketRelay"}else{text(b,"message")}})
        };
        self.write(json!({"type":"control_response","response":{"subtype":"success","request_id":rid,"response":decision}})).await?;
        self.pending.lock().unwrap().remove(rid);
        if behavior == "allow" && b["always"] == true {
            let mut m = self.meta.lock().unwrap();
            let allow = m["auto_allow"].as_array_mut().unwrap();
            if !allow.contains(&req["tool_name"]) {
                allow.push(req["tool_name"].clone());
            }
        }
        self.emit(json!({"type":"permission_resolved","request_id":rid,"behavior":behavior,"always":b["always"]==true,"message":b["message"]}),true);
        let waiting = !self.pending.lock().unwrap().is_empty();
        self.status(app, if waiting { "waiting" } else { "running" });
        Ok(())
    }
    pub async fn interrupt(&self, app: &App) -> Result<()> {
        let ids = self
            .pending
            .lock()
            .unwrap()
            .keys()
            .cloned()
            .collect::<Vec<_>>();
        for id in ids {
            let _ = self
                .decide(
                    app,
                    &id,
                    &json!({"behavior":"deny","message":"Interrupted by the user"}),
                )
                .await;
        }
        if self.meta.lock().unwrap()["_alive"] == true {
            self.control(json!({"subtype":"interrupt"})).await?;
        }
        self.emit(json!({"type":"remote","event":"interrupted"}), true);
        Ok(())
    }
    pub async fn update(&self, app: &App, b: &Value) -> Result<()> {
        validate(b)?;
        let mut changes = json!({});
        {
            let mut m = self.meta.lock().unwrap();
            for k in [
                "name",
                "model",
                "permission_mode",
                "effort",
                "append_system_prompt",
                "pinned",
            ] {
                if let Some(v) = b.get(k) {
                    if v != &m[k] {
                        m[k] = v.clone();
                        changes[k] = v.clone();
                    }
                }
            }
        }
        let alive = self.meta.lock().unwrap()["_alive"] == true;
        if alive {
            if !changes["permission_mode"].is_null() {
                self.control(
                    json!({"subtype":"set_permission_mode","mode":changes["permission_mode"]}),
                )
                .await?;
            }
            if !changes["model"].is_null() {
                self.control(json!({"subtype":"set_model","model":changes["model"]}))
                    .await?;
            }
            if changes.get("effort").is_some() || changes.get("append_system_prompt").is_some() {
                self.stop().await;
            }
        }
        self.emit(
            json!({"type":"remote","event":"config","changes":changes}),
            true,
        );
        app.sessions.touch(self);
        Ok(())
    }
    pub async fn stop(&self) {
        let p = self.process.lock().await.take();
        if let Some(p) = p {
            let _ = p.stop.send(());
            let _ = tokio::time::timeout(Duration::from_secs(5), p.done).await;
        }
    }
}
fn files() -> Vec<PathBuf> {
    let root = config::expand(&config::envs(
        "CLAUDE_PROJECTS",
        &config::home().join(".claude/projects").to_string_lossy(),
    ));
    let mut files = vec![];
    if let Ok(dirs) = std::fs::read_dir(root) {
        for dir in dirs.flatten() {
            if let Ok(children) = std::fs::read_dir(dir.path()) {
                files.extend(
                    children
                        .flatten()
                        .map(|e| e.path())
                        .filter(|p| p.extension().is_some_and(|e| e == "jsonl")),
                );
            }
        }
    }
    files.sort_by_key(|p| std::cmp::Reverse(p.metadata().and_then(|m| m.modified()).ok()));
    files
}
pub fn history(query: &str, limit: usize) -> Vec<Value> {
    let mut result = vec![];
    for p in files() {
        if result.len() >= limit.min(1000) {
            break;
        }
        let mut title = String::new();
        let mut cwd = String::new();
        let mut custom = String::new();
        if let Ok(f) = std::fs::File::open(&p) {
            for l in std::io::BufReader::new(f)
                .lines()
                .take(401)
                .map_while(std::result::Result::ok)
            {
                let Ok(v) = serde_json::from_str::<Value>(&l) else {
                    continue;
                };
                if cwd.is_empty() {
                    cwd = text(&v, "cwd").into();
                }
                if v["type"] == "custom-title" || v["type"] == "summary" {
                    custom = v["customTitle"]
                        .as_str()
                        .unwrap_or(text(&v, "summary"))
                        .into();
                }
                if title.is_empty()
                    && v["type"] == "user"
                    && v["isMeta"] != true
                    && v["isSidechain"] != true
                {
                    let c = &v["message"]["content"];
                    let t = c.as_str().map(String::from).unwrap_or_else(|| {
                        c.as_array()
                            .into_iter()
                            .flatten()
                            .map(|b| text(b, "text"))
                            .collect::<Vec<_>>()
                            .join(" ")
                    });
                    if human(&t) {
                        title = t.trim().replace('\n', " ").chars().take(160).collect();
                    }
                }
            }
        }
        if !custom.is_empty() {
            title = custom;
        }
        if title.is_empty()
            || !format!("{title} {cwd}")
                .to_lowercase()
                .contains(&query.to_lowercase())
        {
            continue;
        }
        if let Ok(st) = p.metadata() {
            result.push(json!({"session_id":p.file_stem().unwrap_or_default().to_string_lossy(),"cwd":cwd,"title":title,"mtime":st.modified().ok().and_then(|t|t.duration_since(std::time::UNIX_EPOCH).ok()).map(|d|d.as_secs_f64()).unwrap_or(0.),"size":st.len()}));
        }
    }
    result
}
fn human(t: &str) -> bool {
    let t = t.trim();
    !t.is_empty() && !t.starts_with('<') && !t.starts_with("Caveat:")
}
fn import(id: &str) -> Vec<Value> {
    let Some(path) = files()
        .into_iter()
        .find(|p| p.file_stem().is_some_and(|s| s == id))
    else {
        return vec![];
    };
    let mut events = VecDeque::new();
    if let Ok(f) = std::fs::File::open(path) {
        for l in std::io::BufReader::new(f)
            .lines()
            .map_while(std::result::Result::ok)
        {
            let Ok(o) = serde_json::from_str::<Value>(&l) else {
                continue;
            };
            if !["user", "assistant"].contains(&text(&o, "type"))
                || o["isMeta"] == true
                || o["isSidechain"] == true
                || o["message"].is_null()
            {
                continue;
            }
            if o["type"] == "user" && o["message"]["content"].as_str().is_some_and(|s| !human(s)) {
                continue;
            }
            events.push_back(json!({"type":o["type"],"message":o["message"],"imported":true}));
            if events.len() > 400 {
                events.pop_front();
            }
        }
    }
    events.into_iter().collect()
}
