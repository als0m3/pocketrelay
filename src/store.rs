use crate::{
    config::{self, now, text, Config},
    error::{Error, Result},
};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{collections::HashMap, path::PathBuf};
use subtle::ConstantTimeEq;

pub const PROVIDERS: [&str; 3] = ["claude", "codex", "antigravity"];
pub fn equal(a: &str, b: &str) -> bool {
    a.as_bytes().ct_eq(b.as_bytes()).into()
}
pub fn hash(s: &str) -> String {
    hex::encode(Sha256::digest(s.as_bytes()))
}
fn slugify(s: &str) -> String {
    let s = deunicode::deunicode(s).to_lowercase();
    let mut out = String::new();
    for c in s.chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c)
        } else if !out.ends_with('-') {
            out.push('-')
        }
    }
    out.trim_matches('-')
        .chars()
        .take(24)
        .collect::<String>()
        .trim_end_matches('-')
        .into()
}
pub struct Store {
    pub accounts: Vec<Value>,
    pub states: HashMap<String, Value>,
    pub keys: Vec<Value>,
    pub dirty: bool,
    data: PathBuf,
}
impl Store {
    pub fn load(c: &Config) -> anyhow::Result<Self> {
        let mut store = Self {
            accounts: config::load(&c.data.join("accounts.json"), json!([]))?
                .as_array()
                .cloned()
                .ok_or_else(|| anyhow::anyhow!("accounts.json must be a list"))?,
            states: serde_json::from_value(config::load(
                &c.data.join("accounts_state.json"),
                json!({}),
            )?)?,
            keys: config::load(&c.data.join("api_keys.json"), json!([]))?
                .as_array()
                .cloned()
                .ok_or_else(|| anyhow::anyhow!("api_keys.json must be a list"))?,
            dirty: false,
            data: c.data.clone(),
        };
        store.accounts.retain(|a| {
            !a["system"].as_bool().unwrap_or(false) || PROVIDERS.contains(&text(a, "provider"))
        });
        for i in 0..store.accounts.len() {
            let a = &store.accounts[i];
            if text(a, "slug").is_empty() {
                let slug = if a["system"] == true
                    // Accept the legacy stored label so existing system slugs remain stable.
                    && ["Host login", "Login de la machine"].contains(&text(a, "label"))
                {
                    format!("system-{}", text(a, "provider"))
                } else {
                    store.unique_slug(&slugify(text(a, "label")), text(a, "id"))
                };
                store.accounts[i]["slug"] = json!(slug);
            }
        }
        for p in &c.system_accounts {
            if !store
                .accounts
                .iter()
                .any(|a| text(a, "provider") == p && a["system"] == true)
            {
                store.accounts.push(json!({"id":format!("system-{p}"),"slug":format!("system-{p}"),"provider":p,"label":"Host login","enabled":true,"system":true,"created":now()}))
            }
        }
        let legacy = c.data.join("claude_oauth_token");
        if legacy.exists() {
            let token = std::fs::read_to_string(&legacy)?;
            let a = store.add("claude", "Primary", Some(token.trim()))?;
            store.accounts.retain(|v| v["id"] != a["id"]);
            store.accounts.insert(0, a);
            std::fs::remove_file(legacy)?;
        }
        store.flush()?;
        Ok(store)
    }
    pub fn flush(&mut self) -> anyhow::Result<()> {
        config::save(&self.data.join("accounts.json"), &json!(self.accounts))?;
        config::save(&self.data.join("accounts_state.json"), &json!(self.states))?;
        config::save(&self.data.join("api_keys.json"), &json!(self.keys))?;
        self.dirty = false;
        Ok(())
    }
    pub fn account(&self, id: &str) -> Result<Value> {
        self.accounts
            .iter()
            .find(|a| {
                text(a, "id") == id
                    || text(a, "slug") == id
                    || a["aliases"]
                        .as_array()
                        .is_some_and(|v| v.iter().any(|v| v.as_str() == Some(id)))
            })
            .cloned()
            .ok_or_else(|| Error::new(404, "Unknown account"))
    }
    pub fn state(&mut self, id: &str) -> &mut Value {
        self.states
            .entry(id.into())
            .or_insert_with(|| json!({"requests":0,"errors":0}))
    }
    fn unique_slug(&self, base: &str, skip: &str) -> String {
        let base = if base.is_empty() { "account" } else { base };
        let mut name = base.to_string();
        let mut n = 1;
        let reserved = [
            "anthropic",
            "claude",
            "claude-code",
            "openai",
            "google",
            "antigravity",
            "gemini",
            "codex",
            "system",
            "auto",
        ];
        while reserved.contains(&name.as_str())
            || self
                .accounts
                .iter()
                .filter(|a| text(a, "id") != skip)
                .any(|a| {
                    text(a, "slug") == name
                        || a["aliases"]
                            .as_array()
                            .is_some_and(|v| v.contains(&json!(name)))
                })
        {
            n += 1;
            name = format!("{base}-{n}");
        }
        name
    }
    pub fn add(&mut self, provider: &str, label: &str, token: Option<&str>) -> Result<Value> {
        if !PROVIDERS.contains(&provider) {
            return Err(Error::new(400, "Unknown provider"));
        }
        let label = if label.trim().is_empty() {
            provider
        } else {
            label.trim()
        };
        if label.chars().count() > 80 {
            return Err(Error::new(400, "Account name too long"));
        }
        let id = config::random(5);
        let slug = self.unique_slug(&slugify(label), "");
        let a = json!({"id":id,"slug":slug,"provider":provider,"label":label,"enabled":true,"system":false,"created":now()});
        if let Some(t) = token {
            self.write_token(&id, t)?;
        }
        std::fs::create_dir_all(self.data.join("accounts").join(&id))?;
        self.accounts.push(a.clone());
        self.flush()?;
        Ok(a)
    }
    pub fn write_token(&mut self, id: &str, token: &str) -> Result<()> {
        config::atomic(
            &self.data.join("accounts").join(id).join("token"),
            token.as_bytes(),
        )?;
        self.resume(id);
        Ok(())
    }
    pub fn token(&self, a: &Value) -> Option<String> {
        std::fs::read_to_string(self.data.join("accounts").join(text(a, "id")).join("token"))
            .ok()
            .map(|s| s.trim().into())
    }
    pub fn cli_home(&self, a: &Value, provider: &str) -> PathBuf {
        if a["system"] == true {
            std::env::var(format!("{}_HOME", provider.to_uppercase()))
                .map(PathBuf::from)
                .unwrap_or_else(|_| {
                    if provider == "codex" {
                        config::home().join(".codex")
                    } else {
                        self.data.join(provider)
                    }
                })
        } else {
            self.data
                .join("accounts")
                .join(text(a, "id"))
                .join(provider)
        }
    }
    pub fn update(&mut self, id: &str, patch: &Value) -> Result<Value> {
        let a = self.account(id)?;
        let idx = self
            .accounts
            .iter()
            .position(|v| v["id"] == a["id"])
            .unwrap();
        if let Some(label) = patch.get("label").and_then(Value::as_str) {
            let label = label.trim();
            if label.is_empty() || label.chars().count() > 80 {
                return Err(Error::new(400, "Invalid account name"));
            }
            let slug = self.unique_slug(&slugify(label), text(&a, "id"));
            let mut aliases = a["aliases"].as_array().cloned().unwrap_or_default();
            aliases.retain(|v| v != &json!(slug));
            if text(&a, "slug") != slug {
                aliases.push(a["slug"].clone());
            }
            self.accounts[idx]["label"] = json!(label);
            self.accounts[idx]["slug"] = json!(slug);
            self.accounts[idx]["aliases"] = json!(aliases);
        }
        if let Some(enabled) = patch.get("enabled") {
            if !enabled.is_boolean() {
                return Err(Error::new(400, "enabled must be a boolean"));
            }
            self.accounts[idx]["enabled"] = enabled.clone();
        }
        self.flush()?;
        Ok(self.accounts[idx].clone())
    }
    pub fn move_account(&mut self, id: &str, delta: i64) -> Result<()> {
        let a = self.account(id)?;
        let ids: Vec<usize> = self
            .accounts
            .iter()
            .enumerate()
            .filter(|(_, v)| v["provider"] == a["provider"])
            .map(|(i, _)| i)
            .collect();
        let i = ids
            .iter()
            .position(|i| self.accounts[*i]["id"] == a["id"])
            .unwrap();
        let j = (i as i64 + delta.signum()).clamp(0, ids.len().saturating_sub(1) as i64) as usize;
        self.accounts.swap(ids[i], ids[j]);
        self.flush()?;
        Ok(())
    }
    pub fn remove(&mut self, id: &str, c: &Config) -> Result<()> {
        let a = self.account(id)?;
        if a["system"] == true && c.system_accounts.contains(&text(&a, "provider").into()) {
            return Err(Error::new(
                400,
                "Remove this provider from REMOTE_SYSTEM_ACCOUNTS before deleting the account.",
            ));
        }
        self.accounts.retain(|v| v["id"] != a["id"]);
        self.states.remove(text(&a, "id"));
        let path = self.data.join("accounts").join(text(&a, "id"));
        if path.exists() {
            std::fs::remove_dir_all(path)?;
        }
        self.flush()?;
        Ok(())
    }
    pub fn resume(&mut self, id: &str) {
        if let Some(m) = self.state(id).as_object_mut() {
            for k in ["paused_until", "pause_reason", "last_error"] {
                m.remove(k);
            }
        }
        self.dirty = true;
    }
    pub fn success(&mut self, id: &str) {
        self.resume(id);
        let s = self.state(id);
        s["requests"] = json!(s["requests"].as_u64().unwrap_or(0) + 1);
        s["last_used"] = json!(now());
        self.dirty = true;
    }
    pub fn failure(&mut self, id: &str, e: &Error) -> bool {
        let pause = if e.status == 429 {
            900.
        } else if e.status == 401 || e.status == 503 || e.kind == "authentication_error" {
            600.
        } else {
            0.
        };
        let s = self.state(id);
        s["errors"] = json!(s["errors"].as_u64().unwrap_or(0) + 1);
        s["last_error"] = json!({"at":now(),"kind":e.kind,"message":e.message.chars().take(300).collect::<String>()});
        if pause > 0. {
            s["paused_until"] = json!(now() + pause);
            s["pause_reason"] = json!(if e.status == 429 {
                "quota reached"
            } else {
                "not signed in"
            });
        }
        self.dirty = true;
        pause > 0.
    }
    pub fn public(&self, a: &Value) -> Value {
        let mut a = a.clone();
        if let Some(s) = self.states.get(text(&a, "id")) {
            if let Some(m) = s.as_object() {
                a.as_object_mut().unwrap().extend(m.clone());
            }
        }
        let status = if a["enabled"] != true {
            "disabled"
        } else if a["paused_until"].as_f64().unwrap_or(0.) > now() {
            "paused"
        } else if !a["last_error"].is_null() {
            "error"
        } else if !a["last_used"].is_null() {
            "ok"
        } else {
            "unknown"
        };
        a["status"] = json!(status);
        if text(&a, "provider") == "claude" && a["system"] != true {
            a["masked"] = self
                .token(&a)
                .map(|t| {
                    json!(format!(
                        "{}…{}",
                        t.chars().take(14).collect::<String>(),
                        t.chars()
                            .rev()
                            .take(4)
                            .collect::<String>()
                            .chars()
                            .rev()
                            .collect::<String>()
                    ))
                })
                .unwrap_or(Value::Null);
        }
        a
    }
    pub fn available(&self, a: &Value) -> bool {
        a["enabled"] == true
            && self
                .states
                .get(text(a, "id"))
                .and_then(|s| s["paused_until"].as_f64())
                .unwrap_or(0.)
                <= now()
    }
    pub fn create_key(&mut self, name: &str) -> Result<Value> {
        let key = format!("sk-cr-{}", config::random(32));
        let item = json!({"id":config::random(6),"name":if name.trim().is_empty(){"untitled"}else{name.trim()},"prefix":format!("{}…{}",&key[..10],&key[key.len()-4..]),"hash":hash(&key),"created":now(),"last_used":null,"requests":0,"revoked":false});
        self.keys.push(item.clone());
        self.flush()?;
        let mut public = item;
        public.as_object_mut().unwrap().remove("hash");
        Ok(json!({"key":key,"item":public}))
    }
    pub fn public_keys(&self) -> Vec<Value> {
        self.keys
            .iter()
            .map(|k| {
                let mut k = k.clone();
                k.as_object_mut().unwrap().remove("hash");
                k
            })
            .collect()
    }
    pub fn verify_key(&mut self, key: &str) -> Option<Value> {
        let h = hash(key);
        let k = self
            .keys
            .iter_mut()
            .find(|k| equal(text(k, "hash"), &h) && k["revoked"] != true)?;
        k["requests"] = json!(k["requests"].as_u64().unwrap_or(0) + 1);
        k["last_used"] = json!(now());
        self.dirty = true;
        Some(json!({"key_id":k["id"],"key_name":k["name"]}))
    }
    pub fn revoke_key(&mut self, id: &str, delete: bool) -> Result<()> {
        let k = self
            .keys
            .iter_mut()
            .find(|k| text(k, "id") == id)
            .ok_or_else(|| Error::new(404, "Unknown key"))?;
        k["revoked"] = json!(true);
        if delete {
            self.keys.retain(|k| text(k, "id") != id);
        }
        self.flush()?;
        Ok(())
    }
}
