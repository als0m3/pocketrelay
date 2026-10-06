use anyhow::{Context, Result};
use rand::RngCore;
use serde_json::Value;
use std::{
    collections::HashMap,
    env,
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

pub fn now() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs_f64()
}
pub fn envs(name: &str, default: &str) -> String {
    env::var(name).unwrap_or_else(|_| default.into())
}
pub fn flag(name: &str, default: bool) -> bool {
    envs(name, if default { "1" } else { "0" }) == "1"
}
pub fn number(name: &str, default: u64) -> u64 {
    env::var(name)
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(default)
}
pub fn random(n: usize) -> String {
    let mut bytes = vec![0; n];
    rand::thread_rng().fill_bytes(&mut bytes);
    hex::encode(bytes)
}
pub fn id(prefix: &str) -> String {
    format!(
        "{prefix}{}",
        &uuid::Uuid::new_v4().simple().to_string()[..24]
    )
}
pub fn text<'a>(v: &'a Value, key: &str) -> &'a str {
    v.get(key).and_then(Value::as_str).unwrap_or("")
}
pub fn home() -> PathBuf {
    PathBuf::from(envs("HOME", "."))
}
pub fn expand(s: &str) -> PathBuf {
    if s == "~" {
        home()
    } else if let Some(p) = s.strip_prefix("~/") {
        home().join(p)
    } else {
        PathBuf::from(s)
    }
}
pub fn atomic(path: &Path, value: &[u8]) -> Result<()> {
    use std::io::Write;
    let dir = path.parent().context("Path has no parent directory")?;
    std::fs::create_dir_all(dir)?;
    let mut tmp = tempfile::NamedTempFile::new_in(dir)?;
    tmp.write_all(value)?;
    tmp.as_file().sync_all()?;
    tmp.persist(path).map_err(|e| e.error)?;
    Ok(())
}
pub fn save(path: &Path, value: &Value) -> Result<()> {
    atomic(path, &serde_json::to_vec(value)?)
}
pub fn load(path: &Path, default: Value) -> Result<Value> {
    match std::fs::read(path) {
        Ok(b) => Ok(serde_json::from_slice(&b)
            .with_context(|| format!("Invalid JSON: {}", path.display()))?),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(default),
        Err(e) => Err(e.into()),
    }
}
fn secret(data: &Path, var: &str, file: &str) -> Result<String> {
    if let Ok(v) = env::var(var) {
        if !v.is_empty() {
            return Ok(v);
        }
    }
    let path = data.join(file);
    if path.exists() {
        return Ok(std::fs::read_to_string(path)?.trim().into());
    }
    let s = random(32);
    atomic(&path, s.as_bytes())?;
    Ok(s)
}
pub fn binary(var: &str, name: &str) -> String {
    if let Ok(s) = env::var(var) {
        return s;
    }
    env::split_paths(&env::var_os("PATH").unwrap_or_default())
        .map(|p| p.join(name))
        .find(|p| p.is_file())
        .map(|p| p.to_string_lossy().into())
        .unwrap_or_default()
}
pub fn clean_env(full: bool) -> HashMap<String, String> {
    let keep = [
        "PATH",
        "HOME",
        "USER",
        "LOGNAME",
        "SHELL",
        "LANG",
        "LANGUAGE",
        "TZ",
        "TMPDIR",
        "TERM",
        "HTTP_PROXY",
        "HTTPS_PROXY",
        "NO_PROXY",
        "ALL_PROXY",
        "SSL_CERT_FILE",
        "SSL_CERT_DIR",
        "NODE_EXTRA_CA_CERTS",
        "DISABLE_AUTOUPDATER",
    ];
    env::vars()
        .filter(|(k, _)| {
            !k.starts_with("REMOTE_")
                && (full
                    || keep.contains(&k.to_uppercase().as_str())
                    || ["LC_", "XDG_", "CLAUDE_", "ANTHROPIC_", "CODEX_"]
                        .iter()
                        .any(|p| k.starts_with(p)))
        })
        .filter(|(k, _)| {
            (full || !["OPENAI_API_KEY", "GEMINI_API_KEY", "GOOGLE_API_KEY"].contains(&k.as_str()))
                && (k != "ANTHROPIC_API_KEY" || flag("REMOTE_KEEP_API_KEY", false))
        })
        .collect()
}

#[derive(Clone)]
pub struct Config {
    pub data: PathBuf,
    pub static_dir: PathBuf,
    pub host: String,
    pub port: u16,
    pub token: String,
    pub session_secret: String,
    pub sessions: bool,
    pub docs: bool,
    pub secure: bool,
    pub public_url: String,
    pub allowed_hosts: Vec<String>,
    pub claude: String,
    pub codex: String,
    pub antigravity: String,
    pub system_accounts: Vec<String>,
    pub require_account: bool,
    pub request_timeout: u64,
    pub max_concurrency: usize,
}
impl Config {
    pub fn read() -> Result<Self> {
        let data = expand(&envs("REMOTE_DATA", "data"));
        std::fs::create_dir_all(&data)?;
        let data = data.canonicalize()?;
        let public_url = envs("REMOTE_PUBLIC_URL", "")
            .trim_end_matches('/')
            .to_owned();
        let mut allowed_hosts = vec![
            "localhost".into(),
            "127.0.0.1".into(),
            "::1".into(),
            "host.docker.internal".into(),
        ];
        allowed_hosts.extend(
            envs("REMOTE_ALLOWED_HOSTS", "")
                .split(',')
                .filter(|s| !s.trim().is_empty())
                .map(|s| s.trim().to_string()),
        );
        if let Ok(u) = url::Url::parse(&public_url) {
            if let Some(h) = u.host_str() {
                allowed_hosts.push(h.into());
            }
        }
        let codex = if flag("REMOTE_ENABLE_CODEX", true) {
            binary("CODEX_BIN", "codex")
        } else {
            String::new()
        };
        let antigravity = if flag("REMOTE_ENABLE_ANTIGRAVITY", true) {
            binary("ANTIGRAVITY_BIN", "antigravity")
        } else {
            String::new()
        };
        Ok(Self {
            token: secret(&data, "REMOTE_TOKEN", "token")?,
            session_secret: secret(&data, "REMOTE_SESSION_SECRET", "session_secret")?,
            data,
            static_dir: expand(&envs("REMOTE_STATIC", "static")),
            host: envs("REMOTE_HOST", "127.0.0.1"),
            port: number("REMOTE_PORT", 8787).try_into()?,
            sessions: flag("REMOTE_ENABLE_SESSIONS", true),
            docs: flag("REMOTE_ENABLE_DOCS", public_url.is_empty()),
            secure: flag("REMOTE_HTTPS", false),
            public_url,
            allowed_hosts,
            claude: {
                let c = binary("CLAUDE_BIN", "claude");
                if c.is_empty() {
                    home().join(".local/bin/claude").to_string_lossy().into()
                } else {
                    c
                }
            },
            codex,
            antigravity,
            system_accounts: envs("REMOTE_SYSTEM_ACCOUNTS", "claude,codex,antigravity")
                .split(',')
                .filter(|s| ["claude", "codex", "antigravity"].contains(s))
                .map(String::from)
                .collect(),
            require_account: flag("REMOTE_REQUIRE_ACCOUNT", true),
            request_timeout: number("REMOTE_REQUEST_TIMEOUT", 600),
            max_concurrency: number("REMOTE_MAX_CONCURRENCY", 6) as usize,
        })
    }
}
