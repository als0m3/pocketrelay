use crate::{
    config::{self, now, text, Config},
    error::{Error, Result},
    store::equal,
};
use axum::http::{header, HeaderMap};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use hmac::{Hmac, Mac};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    collections::{HashMap, VecDeque},
    sync::Mutex,
};

type Hmac256 = Hmac<Sha256>;
#[derive(Default)]
pub struct Auth {
    attempts: Mutex<VecDeque<(f64, String)>>,
    pending: Mutex<HashMap<String, Value>>,
    discovery: Mutex<Option<(f64, Value)>>,
}
fn record(c: &Config) -> Result<Value> {
    Ok(config::load(&c.data.join("admin.json"), Value::Null)?)
}
fn password_hash(password: &str, salt: &str) -> Result<String> {
    let mut out = [0u8; 64];
    let salt = hex::decode(salt).map_err(|_| Error::new(500, "Invalid account configuration"))?;
    scrypt::scrypt(
        password.as_bytes(),
        &salt,
        &scrypt::Params::new(14, 8, 1, 64)
            .map_err(|_| Error::new(500, "Invalid scrypt parameters"))?,
        &mut out,
    )
    .map_err(|_| Error::new(500, "Invalid hash"))?;
    Ok(hex::encode(out))
}
pub fn configure(c: &Config, username: &str, password: &str) -> Result<()> {
    let username = username.trim();
    if username.is_empty() || username.chars().count() > 80 {
        return Err(Error::new(
            400,
            "The username must contain 1 to 80 characters.",
        ));
    }
    if !(12..=256).contains(&password.chars().count()) {
        return Err(Error::new(
            400,
            "The password must contain 12 to 256 characters.",
        ));
    }
    let salt = config::random(16);
    config::save(
        &c.data.join("admin.json"),
        &json!({"username":username,"salt":salt,"hash":password_hash(password,&salt)?,"revision":config::random(16)}),
    )?;
    Ok(())
}
pub fn setup_stdin(c: &Config, if_missing: bool) -> anyhow::Result<()> {
    use std::io::Read;
    if if_missing && !record(c)?.is_null() {
        return Ok(());
    }
    let mut bytes = Vec::new();
    std::io::stdin().take(4097).read_to_end(&mut bytes)?;
    anyhow::ensure!(bytes.len() <= 4096, "Configuration is too large");
    let value: Value = serde_json::from_slice(&bytes)
        .map_err(|_| anyhow::anyhow!("Invalid JSON configuration"))?;
    configure(c, text(&value, "username"), text(&value, "password"))?;
    Ok(())
}
pub fn setup(c: &Config, if_missing: bool) -> anyhow::Result<()> {
    use std::io::{self, IsTerminal, Write};
    if if_missing && !record(c)?.is_null() {
        println!("An administrator account already exists; it has been preserved.");
        return Ok(());
    }
    if !io::stdin().is_terminal() {
        anyhow::bail!("Open an interactive terminal to choose your password.")
    }
    println!("PocketRelay console setup.");
    loop {
        print!("Username [admin]: ");
        io::stdout().flush()?;
        let mut username = String::new();
        if io::stdin().read_line(&mut username)? == 0 {
            anyhow::bail!("Setup canceled")
        };
        let username = if username.trim().is_empty() {
            "admin"
        } else {
            username.trim()
        };
        let password = rpassword::prompt_password("Password (at least 12 characters): ")?;
        let confirm = rpassword::prompt_password("Confirm password: ")?;
        if !equal(&password, &confirm) {
            println!("Passwords do not match.");
            continue;
        }
        match configure(c, username, &password) {
            Ok(()) => {
                println!("Account saved. Previous local login sessions have been invalidated.");
                return Ok(());
            }
            Err(e) => println!("{}", e.message),
        }
    }
}
pub fn cookie_value(headers: &HeaderMap, name: &str) -> String {
    headers
        .get_all(header::COOKIE)
        .iter()
        .filter_map(|h| h.to_str().ok())
        .flat_map(|s| s.split(';'))
        .filter_map(|s| s.trim().split_once('='))
        .find(|(k, _)| *k == name)
        .map(|(_, v)| v.into())
        .unwrap_or_default()
}
pub fn sign(c: &Config, v: &Value) -> String {
    let payload = URL_SAFE_NO_PAD.encode(serde_json::to_vec(v).unwrap());
    let mut mac = Hmac256::new_from_slice(c.session_secret.as_bytes()).unwrap();
    mac.update(payload.as_bytes());
    format!(
        "{payload}.{}",
        URL_SAFE_NO_PAD.encode(mac.finalize().into_bytes())
    )
}
pub fn unsign(c: &Config, s: &str) -> Option<Value> {
    let (payload, sig) = s.split_once('.')?;
    let mut mac = Hmac256::new_from_slice(c.session_secret.as_bytes()).ok()?;
    mac.update(payload.as_bytes());
    mac.verify_slice(&URL_SAFE_NO_PAD.decode(sig).ok()?).ok()?;
    serde_json::from_slice(&URL_SAFE_NO_PAD.decode(payload).ok()?).ok()
}
pub fn cookie(c: &Config, name: &str, value: &str, seconds: u64) -> String {
    format!(
        "{name}={value}; Path=/; HttpOnly; SameSite=Lax; Max-Age={seconds}{}",
        if c.secure { "; Secure" } else { "" }
    )
}
pub fn user(c: &Config, h: &HeaderMap) -> Option<Value> {
    let u = unsign(c, &cookie_value(h, "cr_admin"))?;
    if u["exp"].as_f64().unwrap_or(0.) <= now() {
        return None;
    }
    if text(&u, "how") == "password" {
        let r = record(c).ok()?;
        if r.is_null() || !equal(text(&r, "revision"), text(&u, "revision")) {
            return None;
        }
    }
    Some(u)
}
pub fn current(c: &Config, h: &HeaderMap, mutating: bool) -> Result<Value> {
    let u = user(c, h).ok_or_else(|| Error::new(401, "Not signed in"))?;
    if mutating {
        check_origin(c, h)?;
    }
    Ok(u)
}
pub fn check_origin(c: &Config, h: &HeaderMap) -> Result<()> {
    if h.get("x-admin").and_then(|v| v.to_str().ok()) != Some("1") {
        return Err(Error::new(403, "Missing X-Admin header"));
    }
    if let Some(origin) = h.get("origin").and_then(|v| v.to_str().ok()) {
        let host = h.get("host").and_then(|v| v.to_str().ok()).unwrap_or("");
        let scheme = if c.secure { "https" } else { "http" };
        if origin != format!("{scheme}://{host}")
            && (c.public_url.is_empty() || origin != c.public_url)
        {
            return Err(Error::new(403, "Origin not allowed"));
        }
    }
    Ok(())
}
pub fn local_allowed(c: &Config, h: &HeaderMap) -> bool {
    if config::envs("REMOTE_OIDC_ISSUER", "").is_empty() {
        return true;
    }
    let public = url::Url::parse(&c.public_url)
        .ok()
        .and_then(|u| u.host_str().map(String::from));
    let host = h
        .get("host")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .split(':')
        .next()
        .unwrap_or("");
    let fwd = h
        .get("x-forwarded-host")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .split(',')
        .next()
        .unwrap_or("")
        .trim()
        .split(':')
        .next()
        .unwrap_or("");
    !public.is_some_and(|p| p == host || p == fwd)
}
impl Auth {
    pub fn config(&self, c: &Config, h: &HeaderMap) -> Result<Value> {
        let configured = !record(c)?.is_null();
        let allowed = local_allowed(c, h);
        Ok(
            json!({"oidc":!config::envs("REMOTE_OIDC_ISSUER","").is_empty(),"token_login":allowed,"password_login":allowed&&configured,"setup_needed":allowed&&!configured,"user":user(c,h)}),
        )
    }
    pub fn throttle(&self, peer: &str) -> Result<()> {
        let mut a = self.attempts.lock().unwrap();
        let now = now();
        while a.front().is_some_and(|(t, _)| *t <= now - 60.) {
            a.pop_front();
        }
        if a.len() >= 30 || a.iter().filter(|(_, ip)| ip == peer).count() >= 5 {
            return Err(Error::new(429, "Too many attempts. Try again in a minute."));
        }
        a.push_back((now, peer.into()));
        Ok(())
    }
    pub async fn login(
        &self,
        c: &Config,
        h: &HeaderMap,
        body: Value,
        mode: &str,
        peer: &str,
    ) -> Result<String> {
        check_origin(c, h)?;
        if !local_allowed(c, h) {
            return Err(Error::new(403, "Use SSO from this address."));
        }
        self.throttle(peer)?;
        let (who, revision) = if mode == "token" {
            if !equal(text(&body, "token"), &c.token) {
                return Err(Error::new(401, "Invalid master token"));
            }
            ("master token".to_string(), Value::Null)
        } else {
            let name = text(&body, "username").to_string();
            let password = text(&body, "password").to_string();
            if name.is_empty()
                || name.chars().count() > 80
                || password.is_empty()
                || password.chars().count() > 256
            {
                return Err(Error::new(400, "Check the values you entered."));
            }
            let r = record(c)?;
            if r.is_null() {
                return Err(Error::new(401, "Incorrect username or password"));
            }
            let salt = text(&r, "salt").to_string();
            let digest = tokio::task::spawn_blocking(move || password_hash(&password, &salt))
                .await
                .map_err(|_| Error::new(500, "Verification interrupted"))??;
            if !equal(&digest, text(&r, "hash")) || !equal(&name, text(&r, "username")) {
                return Err(Error::new(401, "Incorrect username or password"));
            }
            (name, r["revision"].clone())
        };
        Ok(cookie(
            c,
            "cr_admin",
            &sign(
                c,
                &json!({"who":who,"how":mode,"revision":revision,"exp":now()+43200.}),
            ),
            43200,
        ))
    }
    async fn discover(&self, client: &reqwest::Client) -> Result<Value> {
        if let Some((t, v)) = &*self.discovery.lock().unwrap() {
            if now() - t < 3600. {
                return Ok(v.clone());
            }
        }
        let issuer = config::envs("REMOTE_OIDC_ISSUER", "")
            .trim_end_matches('/')
            .to_string();
        if issuer.is_empty() {
            return Err(Error::new(404, "OIDC is not configured"));
        }
        let d: Value = client
            .get(format!("{issuer}/.well-known/openid-configuration"))
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;
        if text(&d, "issuer") != issuer {
            return Err(Error::new(502, "Unexpected OIDC issuer"));
        }
        *self.discovery.lock().unwrap() = Some((now(), d.clone()));
        Ok(d)
    }
    pub async fn oidc_start(
        &self,
        c: &Config,
        h: &HeaderMap,
        client: &reqwest::Client,
    ) -> Result<(String, String)> {
        let d = self.discover(client).await?;
        let state = config::random(24);
        let nonce = config::random(24);
        let verifier = config::random(32);
        let challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()));
        let base = if !c.public_url.is_empty() {
            c.public_url.clone()
        } else {
            format!(
                "{}://{}",
                if c.secure { "https" } else { "http" },
                h.get("host")
                    .and_then(|v| v.to_str().ok())
                    .unwrap_or("localhost")
            )
        };
        let redirect = format!("{base}/admin/auth/callback");
        {
            let mut p = self.pending.lock().unwrap();
            p.retain(|_, v| v["exp"].as_f64().unwrap_or(0.) > now());
            if p.len() >= 1000 {
                return Err(Error::new(429, "Too many pending logins"));
            }
            p.insert(
                state.clone(),
                json!({"nonce":nonce,"verifier":verifier,"redirect":redirect,"exp":now()+600.}),
            );
        }
        let mut url = url::Url::parse(text(&d, "authorization_endpoint"))
            .map_err(|_| Error::new(502, "Invalid OIDC URL"))?;
        url.query_pairs_mut().extend_pairs([
            ("response_type", "code"),
            ("client_id", &config::envs("REMOTE_OIDC_CLIENT_ID", "")),
            ("redirect_uri", &redirect),
            ("scope", "openid email profile"),
            ("state", &state),
            ("nonce", &nonce),
            ("code_challenge", &challenge),
            ("code_challenge_method", "S256"),
        ]);
        Ok((
            url.into(),
            cookie(c, "cr_oidc", &sign(c, &json!({"state":state})), 600),
        ))
    }
    pub async fn oidc_finish(
        &self,
        c: &Config,
        h: &HeaderMap,
        q: &HashMap<String, String>,
        client: &reqwest::Client,
    ) -> Result<String> {
        let state = q.get("state").map(String::as_str).unwrap_or("");
        let bound = unsign(c, &cookie_value(h, "cr_oidc")).unwrap_or(Value::Null);
        if state.is_empty() || !equal(state, text(&bound, "state")) {
            return Err(Error::new(403, "Invalid login session"));
        }
        let p = self
            .pending
            .lock()
            .unwrap()
            .remove(state)
            .ok_or_else(|| Error::new(403, "Login expired"))?;
        if p["exp"].as_f64().unwrap_or(0.) < now() {
            return Err(Error::new(403, "Login expired"));
        }
        let code = q
            .get("code")
            .ok_or_else(|| Error::new(403, "SSO login canceled"))?;
        let d = self.discover(client).await?;
        let client_id = config::envs("REMOTE_OIDC_CLIENT_ID", "");
        let tokens: Value = client
            .post(text(&d, "token_endpoint"))
            .form(&[
                ("grant_type", "authorization_code"),
                ("code", code.as_str()),
                ("redirect_uri", text(&p, "redirect")),
                ("code_verifier", text(&p, "verifier")),
                ("client_id", &client_id),
                (
                    "client_secret",
                    &config::envs("REMOTE_OIDC_CLIENT_SECRET", ""),
                ),
            ])
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;
        let token = text(&tokens, "id_token");
        let head =
            jsonwebtoken::decode_header(token).map_err(|_| Error::new(403, "Invalid SSO token"))?;
        if !matches!(
            head.alg,
            jsonwebtoken::Algorithm::RS256
                | jsonwebtoken::Algorithm::RS384
                | jsonwebtoken::Algorithm::RS512
                | jsonwebtoken::Algorithm::ES256
                | jsonwebtoken::Algorithm::ES384
        ) {
            return Err(Error::new(403, "SSO algorithm not allowed"));
        }
        let jwks: jsonwebtoken::jwk::JwkSet = client
            .get(text(&d, "jwks_uri"))
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;
        let jwk = jwks
            .keys
            .iter()
            .find(|k| k.common.key_id == head.kid)
            .ok_or_else(|| Error::new(403, "Unknown SSO key"))?;
        let key = jsonwebtoken::DecodingKey::from_jwk(jwk)
            .map_err(|_| Error::new(403, "Invalid SSO key"))?;
        let mut validation = jwt_validation(head.alg);
        validation.set_audience(&[&client_id]);
        validation.set_issuer(&[text(&d, "issuer")]);
        validation.set_required_spec_claims(&["exp", "iss", "aud", "sub"]);
        let claims = jsonwebtoken::decode::<Value>(token, &key, &validation)
            .map_err(|_| Error::new(403, "Invalid or expired SSO token"))?
            .claims;
        if !equal(text(&claims, "nonce"), text(&p, "nonce")) {
            return Err(Error::new(403, "Invalid SSO nonce"));
        }
        let email = text(&claims, "email").to_lowercase();
        let emails = config::envs("REMOTE_ADMIN_EMAILS", "");
        let groups = config::envs("REMOTE_ADMIN_GROUPS", "");
        let mut roles: Vec<String> = Vec::new();
        for v in [
            &claims["groups"],
            &claims["roles"],
            &claims["realm_access"]["roles"],
            &claims["resource_access"][&client_id]["roles"],
        ] {
            if let Some(a) = v.as_array() {
                roles.extend(
                    a.iter()
                        .filter_map(|v| v.as_str())
                        .map(|s| s.trim_matches('/').into()),
                );
            }
        }
        let allowed = (!email.is_empty()
            && claims["email_verified"] != false
            && emails.split(',').any(|e| e.trim().to_lowercase() == email))
            || groups
                .split(',')
                .filter(|g| !g.trim().is_empty())
                .any(|g| roles.contains(&g.trim().trim_matches('/').to_string()));
        if !allowed {
            return Err(Error::new(403, "This account is not an administrator."));
        }
        let who = if email.is_empty() {
            text(&claims, "preferred_username").to_string()
        } else {
            email
        };
        Ok(cookie(
            c,
            "cr_admin",
            &sign(c, &json!({"who":who,"how":"oidc","exp":now()+43200.})),
            43200,
        ))
    }
}

/// Validate optional not-before dates as well as the required expiration.
pub fn jwt_validation(algorithm: jsonwebtoken::Algorithm) -> jsonwebtoken::Validation {
    let mut validation = jsonwebtoken::Validation::new(algorithm);
    validation.validate_nbf = true;
    validation
}
