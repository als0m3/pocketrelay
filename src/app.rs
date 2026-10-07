use crate::{
    auth::Auth,
    backend::CodexPool,
    config::{self, now, text, Config},
    error::{Error, Result},
    store::Store,
};
use axum::http::HeaderMap;
use serde_json::{json, Value};
use std::{
    collections::{HashMap, VecDeque},
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        Arc, Mutex,
    },
    time::Duration,
};
use tokio::sync::Semaphore;

pub struct Memory {
    items: HashMap<String, (String, Vec<Value>, usize)>,
    order: VecDeque<String>,
    bytes: usize,
    limit: usize,
}
impl Memory {
    pub fn new(limit: usize) -> Self {
        Self {
            items: HashMap::new(),
            order: VecDeque::new(),
            bytes: 0,
            limit,
        }
    }
    pub fn get(&self, id: &str, owner: &str) -> Option<Vec<Value>> {
        self.items
            .get(id)
            .filter(|(o, _, _)| o == owner)
            .map(|(_, t, _)| t.clone())
    }
    pub fn remember(&mut self, id: String, owner: String, turns: Vec<Value>) {
        if self.limit == 0 {
            return;
        }
        let size = serde_json::to_vec(&turns)
            .map(|b| b.len())
            .unwrap_or(self.limit + 1);
        if size > self.limit {
            return;
        }
        if let Some((_, _, s)) = self.items.remove(&id) {
            self.bytes -= s;
            self.order.retain(|v| v != &id);
        }
        while self.bytes + size > self.limit || self.items.len() >= 500 {
            let Some(old) = self.order.pop_front() else {
                break;
            };
            if let Some((_, _, s)) = self.items.remove(&old) {
                self.bytes -= s;
            }
        }
        self.bytes += size;
        self.order.push_back(id.clone());
        self.items.insert(id, (owner, turns, size));
    }
}
pub struct App {
    pub shutdown: tokio::sync::watch::Sender<bool>,
    pub config: Config,
    pub store: Mutex<Store>,
    pub auth: Auth,
    pub client: reqwest::Client,
    pub codex: CodexPool,
    pub sem: Arc<Semaphore>,
    pub requests: AtomicU64,
    pub errors: AtomicU64,
    pub output_tokens: AtomicU64,
    pub catalog: Mutex<Value>,
    pub catalog_at: Mutex<f64>,
    pub catalog_busy: AtomicBool,
    pub responses: Mutex<Memory>,
    pub rates: Mutex<HashMap<String, VecDeque<f64>>>,
    pub sessions: crate::sessions::Manager,
}
impl App {
    pub fn new(config: Config) -> anyhow::Result<Arc<Self>> {
        let store = Store::load(&config)?;
        let catalog = initial_catalog(&config);
        let concurrency = if config.max_concurrency == 0 {
            100000
        } else {
            config.max_concurrency
        };
        let sessions = crate::sessions::Manager::new(&config)?;
        Ok(Arc::new(Self {
            shutdown: tokio::sync::watch::channel(false).0,
            config,
            store: Mutex::new(store),
            auth: Auth::default(),
            client: reqwest::Client::builder()
                .timeout(Duration::from_secs(20))
                .redirect(reqwest::redirect::Policy::none())
                .build()?,
            codex: CodexPool::default(),
            sem: Arc::new(Semaphore::new(concurrency)),
            requests: AtomicU64::new(0),
            errors: AtomicU64::new(0),
            output_tokens: AtomicU64::new(0),
            catalog: Mutex::new(catalog),
            catalog_at: Mutex::new(0.),
            catalog_busy: AtomicBool::new(false),
            responses: Mutex::new(Memory::new(
                config::number("REMOTE_RESPONSES_MAX_MB", 64) as usize * 1024 * 1024,
            )),
            rates: Mutex::default(),
            sessions,
        }))
    }
    pub fn api_auth(&self, h: &HeaderMap) -> Result<Value> {
        let key = h
            .get("authorization")
            .and_then(|v| v.to_str().ok())
            .and_then(|s| s.split_once(' '))
            .filter(|(scheme, _)| scheme.eq_ignore_ascii_case("bearer"))
            .map(|(_, s)| s.trim())
            .or_else(|| h.get("x-api-key").and_then(|v| v.to_str().ok()))
            .unwrap_or("");
        if !key.is_empty()
            && config::flag("REMOTE_V1_ALLOW_MASTER", true)
            && crate::store::equal(key, &self.config.token)
        {
            return Ok(json!({"master":true}));
        }
        self.store
            .lock()
            .unwrap()
            .verify_key(key)
            .ok_or_else(|| Error::new(401, "Incorrect API key provided.").code("invalid_api_key"))
    }
    pub fn subject(&self, h: &HeaderMap, ident: &Value) -> (String, bool) {
        if ident["master"] == true {
            return ("master".into(), true);
        }
        let default = (format!("key:{}", text(ident, "key_id")), false);
        let names = config::envs("REMOTE_FORWARDER_KEY_NAME", "open-webui");
        if !names
            .split(',')
            .any(|s| s.trim() == text(ident, "key_name"))
        {
            return default;
        }
        let secret = config::envs("REMOTE_FORWARD_JWT_SECRET", "");
        let Some(token) = h.get("x-openwebui-user-jwt").and_then(|v| v.to_str().ok()) else {
            return default;
        };
        if secret.is_empty() {
            return default;
        }
        let mut validation = jsonwebtoken::Validation::new(jsonwebtoken::Algorithm::HS256);
        validation.validate_aud = false;
        let Ok(v) = jsonwebtoken::decode::<Value>(
            token,
            &jsonwebtoken::DecodingKey::from_secret(secret.as_bytes()),
            &validation,
        ) else {
            return default;
        };
        let c = v.claims;
        let subject = if !text(&c, "email").is_empty() {
            text(&c, "email")
        } else {
            text(&c, "sub")
        };
        if subject.is_empty() {
            return default;
        }
        (format!("owui:{subject}"), c["role"] == "admin")
    }
    pub fn admit(&self, owner: &str, exempt: bool, cost: usize) -> Result<()> {
        let rate = config::envs("REMOTE_USER_RATE", "200/h");
        if exempt || rate.is_empty() || rate == "0" {
            return Ok(());
        }
        let (n, period) = rate.split_once('/').unwrap_or((&rate, "h"));
        let n = n
            .parse::<usize>()
            .map_err(|_| Error::new(500, "Invalid REMOTE_USER_RATE"))?;
        let secs = match period {
            "m" => 60.,
            "d" => 86400.,
            _ => 3600.,
        };
        let t = now();
        let mut rates = self.rates.lock().unwrap();
        rates.retain(|_, v| v.back().is_some_and(|last| *last > t - secs));
        if !rates.contains_key(owner) && rates.len() >= 10000 {
            return Err(Error::new(503, "Too many active users; try again later"));
        }
        let q = rates.entry(owner.into()).or_default();
        while q.front().is_some_and(|v| *v <= t - secs) {
            q.pop_front();
        }
        if q.len() + cost > n {
            return Err(Error::new(
                429,
                format!("Rate limit reached: {rate}. Try again after the reset."),
            )
            .code("user_rate_limit"));
        }
        q.extend(std::iter::repeat_n(t, cost));
        Ok(())
    }
    pub fn state(&self, user: Value) -> Value {
        let store = self.store.lock().unwrap();
        let mut accounts = json!({});
        for p in crate::store::PROVIDERS {
            accounts[p] = json!(store
                .accounts
                .iter()
                .filter(|a| text(a, "provider") == p)
                .map(|a| store.public(a))
                .collect::<Vec<_>>());
        }
        json!({"user":user,"keys":store.public_keys(),"accounts":accounts,"stats":{"requests":self.requests.load(Ordering::Relaxed),"errors":self.errors.load(Ordering::Relaxed),"output_tokens":self.output_tokens.load(Ordering::Relaxed)},"managed_tools":self.config.managed_tools.is_some(),"codex_enabled":!self.config.codex.is_empty(),"antigravity_enabled":!self.config.antigravity.is_empty(),"deletable_system":crate::store::PROVIDERS.iter().filter(|p|!self.config.system_accounts.contains(&p.to_string())).collect::<Vec<_>>(),"models":self.catalog.lock().unwrap().clone(),"docs_enabled":self.config.docs,"require_account":self.config.require_account,"catalog_loading":self.catalog_busy.load(Ordering::Relaxed)})
    }
    pub async fn refresh_catalog(self: &Arc<Self>, force: bool) {
        if !force && now() - *self.catalog_at.lock().unwrap() < 60. {
            return;
        }
        if self.catalog_busy.swap(true, Ordering::SeqCst) {
            return;
        }
        struct Busy<'a>(&'a AtomicBool);
        impl Drop for Busy<'_> {
            fn drop(&mut self) {
                self.0.store(false, Ordering::SeqCst);
            }
        }
        let _guard = Busy(&self.catalog_busy);
        let accounts = self
            .store
            .lock()
            .unwrap()
            .accounts
            .iter()
            .filter(|a| a["enabled"] == true && a["provider"] == "codex")
            .cloned()
            .collect::<Vec<_>>();
        let mut models = vec![];
        let tools_ready = self
            .config
            .managed_tools
            .as_ref()
            .is_none_or(|tools| tools.status("codex").is_ok_and(|s| s["state"] == "ready"));
        if !self.config.codex.is_empty() && tools_ready {
            for a in accounts {
                let home = self.store.lock().unwrap().cli_home(&a, "codex");
                let result = tokio::time::timeout(Duration::from_secs(8), async {
                    let srv = self.codex.get(&self.config, &a, home).await?;
                    let identity = srv.call("account/read", json!({})).await?;
                    let models = srv.models().await?;
                    let limits = srv.limits.lock().unwrap().clone();
                    Ok::<_, Error>((identity, models, limits))
                })
                .await;
                if let Ok(Ok((identity, ms, limits))) = result {
                    let mut store = self.store.lock().unwrap();
                    let state = store.state(text(&a, "id"));
                    state["identity"] = identity["account"].clone();
                    state["limits"] = limits;
                    if !ms.is_empty() {
                        models=ms.into_iter().map(|m|json!({"id":m["id"],"name":m.get("displayName").unwrap_or(&m["id"]),"isDefault":m["isDefault"]})).collect();
                    }
                }
            }
        }
        if !models.is_empty() {
            self.catalog.lock().unwrap()["codex"] = json!(models);
        }
        *self.catalog_at.lock().unwrap() = now();
    }
}
fn initial_catalog(c: &Config) -> Value {
    let mut models = json!({"claude":[{"id":"opus","name":"Opus"},{"id":"sonnet","name":"Sonnet"},{"id":"haiku","name":"Haiku"},{"id":"fable","name":"Fable"}],"codex":[],"antigravity":[]});
    if !c.antigravity.is_empty() {
        models["antigravity"] = if let Ok(custom) = std::env::var("REMOTE_ANTIGRAVITY_MODELS") {
            json!(custom
                .split(',')
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(|s| json!({"id":s,"name":s}))
                .collect::<Vec<_>>())
        } else {
            json!([{"id":"gemini-3-pro","name":"Gemini 3 Pro"},{"id":"gemini-3-flash","name":"Gemini 3 Flash"},{"id":"gemini-2.5-flash","name":"Gemini 2.5 Flash"}])
        };
    }
    models
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn memory_is_bounded_and_private() {
        let mut m = Memory::new(100);
        m.remember("a".into(), "alice".into(), vec![json!({"text":"hello"})]);
        assert!(m.get("a", "bob").is_none());
        assert!(m.get("a", "alice").is_some());
        m.remember(
            "b".into(),
            "bob".into(),
            vec![json!({"text":"x".repeat(75)})],
        );
        assert!(m.bytes <= 100);
        assert!(m.get("a", "alice").is_none());
    }
    #[test]
    fn disabled_memory_stores_nothing() {
        let mut m = Memory::new(0);
        m.remember("a".into(), "a".into(), vec![]);
        assert!(m.get("a", "a").is_none());
    }
}
