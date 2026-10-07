use super::*;
use axum::{
    body::{to_bytes, Body},
    http::{HeaderMap, Request},
};
use serde_json::{json, Value};
use tower::ServiceExt;
fn fixture() -> (tempfile::TempDir, Arc<App>) {
    let dir = tempfile::tempdir().unwrap();
    let c = config::Config {
        managed_tools: None,
        data: dir.path().into(),
        static_dir: std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("static"),
        host: "127.0.0.1".into(),
        port: 8787,
        token: "test-master".into(),
        session_secret: "test-secret".into(),
        sessions: true,
        docs: true,
        secure: false,
        public_url: String::new(),
        allowed_hosts: vec!["localhost".into()],
        claude: "/bin/false".into(),
        codex: String::new(),
        antigravity: String::new(),
        system_accounts: vec![],
        require_account: true,
        request_timeout: 2,
        max_concurrency: 2,
    };
    let app = App::new(c).unwrap();
    (dir, app)
}
async fn request(
    app: Arc<App>,
    method: &str,
    path: &str,
    body: Value,
    headers: &[(&str, &str)],
) -> (u16, HeaderMap, Value) {
    let mut r = Request::builder()
        .method(method)
        .uri(path)
        .header("host", "localhost");
    for (k, v) in headers {
        r = r.header(*k, *v);
    }
    let r = router(app)
        .oneshot(
            r.body(if body.is_null() {
                Body::empty()
            } else {
                Body::from(body.to_string())
            })
            .unwrap(),
        )
        .await
        .unwrap();
    let status = r.status().as_u16();
    let h = r.headers().clone();
    let bytes = to_bytes(r.into_body(), 8 * 1024 * 1024).await.unwrap();
    (
        status,
        h,
        serde_json::from_slice(&bytes).unwrap_or_else(|_| json!(String::from_utf8_lossy(&bytes))),
    )
}
fn cookie(app: &App) -> String {
    format!(
        "cr_admin={}",
        auth::sign(
            &app.config,
            &json!({"who":"test","how":"token","exp":config::now()+60.})
        )
    )
}
#[tokio::test]
async fn admin_requires_auth_and_origin() {
    let (_d, a) = fixture();
    assert_eq!(
        request(a.clone(), "GET", "/admin/api/state", Value::Null, &[])
            .await
            .0,
        401
    );
    let cookie = cookie(&a);
    assert_eq!(
        request(
            a.clone(),
            "GET",
            "/admin/api/state",
            Value::Null,
            &[("cookie", &cookie)]
        )
        .await
        .0,
        200
    );
    assert_eq!(
        request(
            a.clone(),
            "POST",
            "/admin/api/keys",
            json!({}),
            &[("cookie", &cookie)]
        )
        .await
        .0,
        403
    );
    assert_eq!(
        request(
            a.clone(),
            "POST",
            "/admin/api/keys",
            json!({}),
            &[
                ("cookie", &cookie),
                ("x-admin", "1"),
                ("origin", "https://attacker.invalid")
            ]
        )
        .await
        .0,
        403
    );
}
#[tokio::test]
async fn password_rotation_invalidates_sessions() {
    let (_d, a) = fixture();
    auth::configure(&a.config, "admin", "correct horse battery").unwrap();
    let r = request(
        a.clone(),
        "POST",
        "/admin/auth/password",
        json!({"username":"admin","password":"correct horse battery"}),
        &[("x-admin", "1")],
    )
    .await;
    assert_eq!(r.0, 200);
    let c = r.1["set-cookie"]
        .to_str()
        .unwrap()
        .split(';')
        .next()
        .unwrap();
    assert_eq!(
        request(
            a.clone(),
            "GET",
            "/admin/api/state",
            Value::Null,
            &[("cookie", c)]
        )
        .await
        .0,
        200
    );
    auth::configure(&a.config, "admin", "another correct horse").unwrap();
    assert_eq!(
        request(
            a.clone(),
            "GET",
            "/admin/api/state",
            Value::Null,
            &[("cookie", c)]
        )
        .await
        .0,
        401
    );
}
#[tokio::test]
async fn key_create_use_revoke_persist() {
    let (d, a) = fixture();
    let c = cookie(&a);
    let h = [("cookie", c.as_str()), ("x-admin", "1")];
    let r = request(
        a.clone(),
        "POST",
        "/admin/api/keys",
        json!({"name":"test"}),
        &h,
    )
    .await;
    assert_eq!(r.0, 200);
    assert!(r.2["item"].get("hash").is_none());
    let token = format!("Bearer {}", r.2["key"].as_str().unwrap());
    assert_eq!(
        request(
            a.clone(),
            "GET",
            "/v1/models",
            Value::Null,
            &[("authorization", &token)]
        )
        .await
        .0,
        200
    );
    assert!(a.store.lock().unwrap().dirty);
    a.store.lock().unwrap().flush().unwrap();
    let raw = std::fs::read_to_string(d.path().join("api_keys.json")).unwrap();
    assert!(!raw.contains(r.2["key"].as_str().unwrap()));
    let path = format!(
        "/admin/api/keys/{}/revoke",
        r.2["item"]["id"].as_str().unwrap()
    );
    assert_eq!(
        request(a.clone(), "POST", &path, json!({}), &h).await.0,
        200
    );
    assert_eq!(
        request(
            a.clone(),
            "GET",
            "/v1/models",
            Value::Null,
            &[("authorization", &token)]
        )
        .await
        .0,
        401
    );
}
#[test]
fn slug_rename_retains_previous_name() {
    let (_d, a) = fixture();
    let mut s = a.store.lock().unwrap();
    // Keep an accented fixture to verify Unicode-to-ASCII slug normalization.
    let v = s.add("claude", "Été & Café", Some("sk-ant-test")).unwrap();
    assert_eq!(v["slug"], "ete-cafe");
    let id = v["id"].as_str().unwrap();
    let renamed = s.update(id, &json!({"label":"New"})).unwrap();
    assert_eq!(renamed["slug"], "new");
    assert_eq!(s.account("ete-cafe").unwrap()["id"], v["id"]);
}
#[test]
fn signed_cookie_rejects_tamper_and_expiry() {
    let (_d, a) = fixture();
    let signed = auth::sign(&a.config, &json!({"who":"test","exp":1}));
    assert!(auth::unsign(&a.config, &format!("{signed}x")).is_none());
    let mut h = HeaderMap::new();
    h.insert("cookie", format!("cr_admin={signed}").parse().unwrap());
    assert!(auth::user(&a.config, &h).is_none());
}
#[test]
fn throttle_bounds_global_and_per_peer() {
    let auth = auth::Auth::default();
    for _ in 0..5 {
        auth.throttle("peer").unwrap();
    }
    assert_eq!(auth.throttle("peer").unwrap_err().status, 429);
    for i in 0..25 {
        auth.throttle(&format!("peer{i}")).unwrap();
    }
    assert_eq!(auth.throttle("fresh").unwrap_err().status, 429);
}
#[tokio::test]
async fn static_cache_and_security_headers() {
    let (_d, a) = fixture();
    let r = request(a.clone(), "GET", "/static/admin.js", Value::Null, &[]).await;
    assert_eq!(r.0, 200);
    assert_eq!(r.1["x-content-type-options"], "nosniff");
    let etag = r.1["etag"].to_str().unwrap();
    assert_eq!(
        request(
            a.clone(),
            "GET",
            "/static/admin.js",
            Value::Null,
            &[("if-none-match", etag)]
        )
        .await
        .0,
        304
    );
    assert_eq!(
        request(a.clone(), "GET", "/static/../Cargo.toml", Value::Null, &[])
            .await
            .0,
        404
    );
}
#[tokio::test]
async fn session_create_update_and_delete() {
    let (d, a) = fixture();
    let h = [("authorization", "Bearer test-master")];
    let r = request(
        a.clone(),
        "POST",
        "/api/sessions",
        json!({"cwd":d.path(),"name":"Test"}),
        &h,
    )
    .await;
    assert_eq!(r.0, 200);
    let path = format!("/api/sessions/{}", r.2["id"].as_str().unwrap());
    assert_eq!(
        request(
            a.clone(),
            "PATCH",
            &path,
            json!({"permission_mode":"oops"}),
            &h
        )
        .await
        .0,
        400
    );
    assert_eq!(
        request(a.clone(), "PATCH", &path, json!({"name":"Renamed"}), &h)
            .await
            .2["name"],
        "Renamed"
    );
    assert_eq!(
        request(a.clone(), "DELETE", &path, Value::Null, &h).await.0,
        200
    );
    assert_eq!(
        request(a.clone(), "GET", &path, Value::Null, &h).await.0,
        404
    );
}
#[tokio::test]
async fn unknown_host_rejected_health_still_works() {
    let (_d, a) = fixture();
    for (path, status) in [("/admin", 403), ("/healthz", 200)] {
        let r = router(a.clone())
            .oneshot(
                Request::builder()
                    .uri(path)
                    .header("host", "evil.invalid")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(r.status().as_u16(), status);
    }
}
#[test]
fn detects_existing_password_configuration() {
    let (d, a) = fixture();
    let record = json!({"username":"admin","salt":"00000000000000000000000000000000","hash":"unused","revision":"r"});
    config::save(&d.path().join("admin.json"), &record).unwrap();
    assert_eq!(
        a.auth.config(&a.config, &HeaderMap::new()).unwrap()["password_login"],
        true
    );
}

#[test]
fn rate_limit_counts_each_generation() {
    let (_d, app) = fixture();
    app.admit("parallel", false, 199).unwrap();
    assert_eq!(app.admit("parallel", false, 2).unwrap_err().status, 429);
    app.admit("parallel", false, 1).unwrap();
}

#[test]
fn legacy_system_label_keeps_model_prefix() {
    let (_dir, app) = fixture();
    for label in ["Host login", "Login de la machine"] {
        config::save(
            &app.config.data.join("accounts.json"),
            &json!([{"id":"system-claude","provider":"claude","system":true,"label":label}]),
        )
        .unwrap();
        let store = store::Store::load(&app.config).unwrap();
        assert_eq!(
            store.account("system-claude").unwrap()["slug"],
            "system-claude"
        );
    }
}

#[test]
fn jwt_dates_reject_malformed_and_future_claims() {
    use jsonwebtoken::{decode, encode, Algorithm, DecodingKey, EncodingKey, Header};
    let secret = config::random(32);
    let mut validation = auth::jwt_validation(Algorithm::HS256);
    validation.validate_aud = false;
    let now = config::now() as u64;
    for (label, overrides, accepted) in [
        ("optional nbf absent", json!({}), true),
        ("nbf already passed", json!({"nbf":now-1}), true),
        ("future nbf", json!({"nbf":now+300}), false),
        ("string nbf", json!({"nbf":(now+300).to_string()}), false),
        ("boolean nbf", json!({"nbf":true}), false),
        ("null nbf", json!({"nbf":null}), false),
        ("expired", json!({"exp":now-120}), false),
        ("string exp", json!({"exp":"never"}), false),
        ("null exp", json!({"exp":null}), false),
    ] {
        let mut claims = json!({"sub":"fixture", "exp":now+300});
        claims
            .as_object_mut()
            .unwrap()
            .extend(overrides.as_object().unwrap().clone());
        let token = encode(
            &Header::default(),
            &claims,
            &EncodingKey::from_secret(secret.as_bytes()),
        )
        .unwrap();
        let result = decode::<Value>(
            &token,
            &DecodingKey::from_secret(secret.as_bytes()),
            &validation,
        );
        assert_eq!(result.is_ok(), accepted, "{label}");
    }
}
