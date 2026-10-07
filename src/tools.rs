//! Optional macOS provider downloads. Docker and standalone installations keep their own CLIs.
use crate::error::{Error, Result};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    collections::HashMap,
    io::{Read, Write},
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::io::AsyncWriteExt;

const MAX_BINARY: u64 = 512 * 1024 * 1024;
pub struct Installer {
    root: PathBuf,
    specs: Value,
    client: reqwest::Client,
    states: Mutex<HashMap<String, Value>>,
}
impl Installer {
    pub fn configured() -> anyhow::Result<Option<Arc<Self>>> {
        let path = crate::config::envs("REMOTE_MANAGED_TOOLS", "");
        if path.is_empty() {
            return Ok(None);
        }
        anyhow::ensure!(cfg!(target_os = "macos"), "Managed tools require macOS");
        let manifest: Value = serde_json::from_str(include_str!("provider-tools.json"))?;
        let specs = manifest[std::env::consts::ARCH].clone();
        anyhow::ensure!(specs.is_object(), "Unsupported tool architecture");
        Ok(Some(Arc::new(Self::new(PathBuf::from(path), specs)?)))
    }
    fn new(root: PathBuf, specs: Value) -> anyhow::Result<Self> {
        std::fs::create_dir_all(&root)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700))?;
        }
        Ok(Self {
            root,
            specs,
            states: Mutex::default(),
            client: reqwest::Client::builder()
                .https_only(true)
                .connect_timeout(Duration::from_secs(20))
                .timeout(Duration::from_secs(600))
                .user_agent("PocketRelay-tool-installer")
                .redirect(reqwest::redirect::Policy::limited(5))
                .build()?,
        })
    }
    fn spec(&self, provider: &str) -> Result<&Value> {
        self.specs
            .get(provider)
            .filter(|s| s.is_object())
            .ok_or_else(|| Error::new(400, "Unknown provider"))
    }
    pub fn binary(&self, provider: &str) -> String {
        self.root.join(provider).to_string_lossy().into_owned()
    }
    fn installed(&self, provider: &str) -> bool {
        let Ok(spec) = self.spec(provider) else {
            return false;
        };
        let receipt = std::fs::read(self.root.join(format!("{provider}.json")))
            .ok()
            .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok());
        receipt
            .as_ref()
            .is_some_and(|r| r["sha256"] == spec["sha256"])
            && std::fs::symlink_metadata(self.binary(provider))
                .is_ok_and(|m| m.is_file() && m.len() > 0)
    }
    pub fn status(&self, provider: &str) -> Result<Value> {
        let spec = self.spec(provider)?;
        if self.installed(provider) {
            return Ok(json!({"state":"ready","provider":provider,"version":spec["version"]}));
        }
        Ok(self.states.lock().unwrap().get(provider).cloned().unwrap_or_else(||
            json!({"state":"missing","provider":provider,"version":spec["version"],"total":spec["size"]})))
    }
    fn update(&self, provider: &str, state: &str, downloaded: u64, error: Option<&str>) {
        self.states.lock().unwrap().insert(
            provider.into(),
            json!({"provider":provider,"state":state,
            "downloaded":downloaded,"total":self.specs[provider]["size"],"error":error}),
        );
    }
    pub fn start(self: &Arc<Self>, provider: &str) -> Result<Value> {
        self.spec(provider)?;
        if self.installed(provider) {
            return self.status(provider);
        }
        let mut states = self.states.lock().unwrap();
        if states
            .get(provider)
            .is_some_and(|s| matches!(s["state"].as_str(), Some("downloading" | "installing")))
        {
            return Ok(states[provider].clone());
        }
        states.insert(provider.into(), json!({"state":"downloading","provider":provider,"downloaded":0,"total":self.specs[provider]["size"]}));
        drop(states);
        let this = self.clone();
        let provider = provider.to_string();
        tokio::spawn(async move {
            let outcome =
                tokio::time::timeout(Duration::from_secs(660), this.download(&provider)).await;
            match outcome {
                Ok(Ok(())) => this.update(&provider, "ready", 0, None),
                Ok(Err(e)) => this.update(
                    &provider,
                    "error",
                    0,
                    Some(&format!(
                        "Tool installation failed: {}. Retry to download again.",
                        e.message
                    )),
                ),
                Err(_) => this.update(
                    &provider,
                    "error",
                    0,
                    Some("Tool installation timed out. Check your connection and retry."),
                ),
            }
        });
        Ok(json!({"state":"downloading"}))
    }
    pub async fn ensure(self: &Arc<Self>, provider: &str) -> Result<()> {
        self.start(provider)?;
        loop {
            let status = self.status(provider)?;
            match status["state"].as_str() {
                Some("ready") => return Ok(()),
                Some("error") => {
                    return Err(Error::new(
                        503,
                        status["error"]
                            .as_str()
                            .unwrap_or("Tool installation failed"),
                    ))
                }
                _ => tokio::time::sleep(Duration::from_millis(200)).await,
            }
        }
    }
    async fn download(&self, provider: &str) -> Result<()> {
        let spec = self.spec(provider)?.clone();
        // TempDir removes partial files on network errors, timeout or cancellation.
        let temp = tempfile::Builder::new()
            .prefix(".install-")
            .tempdir_in(&self.root)?;
        let archive = temp.path().join("download");
        let mut output = tokio::fs::File::create(&archive).await?;
        let mut response = self
            .client
            .get(spec["url"].as_str().unwrap())
            .send()
            .await?
            .error_for_status()?;
        let expected = spec["size"].as_u64().unwrap();
        let mut count = 0;
        let mut hash = Sha256::new();
        while let Some(chunk) = response.chunk().await? {
            count += chunk.len() as u64;
            if count > expected {
                return Err(Error::new(502, "Download exceeds the expected size"));
            }
            hash.update(&chunk);
            output.write_all(&chunk).await?;
            self.update(provider, "downloading", count, None);
        }
        output.flush().await?;
        drop(output);
        if count != expected || hex::encode(hash.finalize()) != spec["sha256"].as_str().unwrap() {
            return Err(Error::new(502, "Download integrity check failed"));
        }
        self.update(provider, "installing", count, None);
        let destination = PathBuf::from(self.binary(provider));
        let receipt = self.root.join(format!("{provider}.json"));
        tokio::task::spawn_blocking(move || {
            install_verified(temp, &archive, &destination, &receipt, &spec)
        })
        .await
        .map_err(|_| Error::new(500, "Tool installation interrupted"))??;
        Ok(())
    }
}
fn install_verified(
    temp: tempfile::TempDir,
    archive: &Path,
    destination: &Path,
    receipt: &Path,
    spec: &Value,
) -> Result<()> {
    let staged = temp.path().join("executable");
    if let Some(member) = spec["member"].as_str() {
        let mut tar =
            tar::Archive::new(flate2::read::GzDecoder::new(std::fs::File::open(archive)?));
        let mut found = false;
        for entry in tar.entries()? {
            let mut entry = entry?;
            if entry.path()?.as_ref() != Path::new(member) {
                continue;
            }
            if found || !entry.header().entry_type().is_file() || entry.size() > MAX_BINARY {
                return Err(Error::new(502, "Invalid executable in tool archive"));
            }
            // Only copy the pinned regular member; never extract archive paths or links.
            let mut output = std::fs::File::create(&staged)?;
            std::io::copy(&mut entry, &mut output)?;
            output.sync_all()?;
            found = true;
        }
        if !found {
            return Err(Error::new(502, "Tool archive is missing its executable"));
        }
    } else {
        std::fs::rename(archive, &staged)?;
    }
    let mut file = std::fs::File::open(&staged)?;
    let mut magic = [0; 4];
    file.read_exact(&mut magic)?;
    if !matches!(
        magic,
        [0xcf, 0xfa, 0xed, 0xfe]
            | [0xfe, 0xed, 0xfa, 0xcf]
            | [0xca, 0xfe, 0xba, 0xbe]
            | [0xbe, 0xba, 0xfe, 0xca]
    ) {
        return Err(Error::new(502, "Downloaded tool is not a macOS executable"));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&staged, std::fs::Permissions::from_mode(0o755))?;
    }
    let metadata = temp.path().join("receipt");
    let mut f = std::fs::File::create(&metadata)?;
    f.write_all(serde_json::to_string(spec)?.as_bytes())?;
    f.sync_all()?;
    std::fs::rename(staged, destination)?;
    std::fs::rename(metadata, receipt)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{routing::get, Router};
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn macho() -> Vec<u8> {
        [vec![0xcf, 0xfa, 0xed, 0xfe], vec![7; 64]].concat()
    }
    fn spec(bytes: &[u8], member: Option<&str>) -> Value {
        json!({"version":"test","url":"https://example.invalid/tool","sha256":hex::encode(Sha256::digest(bytes)),"size":bytes.len(),"member":member})
    }
    fn archive(member: &str, symlink: bool) -> Vec<u8> {
        let mut tar = tar::Builder::new(flate2::write::GzEncoder::new(
            Vec::new(),
            flate2::Compression::default(),
        ));
        let bytes = macho();
        let mut header = tar::Header::new_gnu();
        header.set_mode(0o755);
        if symlink {
            header.set_entry_type(tar::EntryType::Symlink);
            header.set_size(0);
            header
                .set_link_name("/tmp/never-extract-this-link")
                .unwrap();
            header.set_cksum();
            tar.append_data(&mut header, member, std::io::empty())
                .unwrap();
        } else {
            header.set_size(bytes.len() as u64);
            header.set_cksum();
            tar.append_data(&mut header, member, bytes.as_slice())
                .unwrap();
        }
        tar.into_inner().unwrap().finish().unwrap()
    }
    #[tokio::test]
    async fn download_is_coalesced_cached_and_resumes_after_restart() {
        let count = Arc::new(AtomicUsize::new(0));
        let requests = count.clone();
        let bytes = macho();
        let response = bytes.clone();
        let router = Router::new().route(
            "/tool",
            get(move || {
                let count = requests.clone();
                let bytes = response.clone();
                async move {
                    count.fetch_add(1, Ordering::SeqCst);
                    tokio::time::sleep(Duration::from_millis(60)).await;
                    bytes
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/tool", listener.local_addr().unwrap());
        let server = tokio::spawn(async move {
            axum::serve(listener, router).await.unwrap();
        });
        let dir = tempfile::tempdir().unwrap();
        let mut s = spec(&bytes, None);
        s["url"] = json!(url);
        let mut manager = Installer::new(dir.path().into(), json!({"claude":s})).unwrap();
        // HTTP is allowed only in this in-process fixture, never in the production client.
        manager.client = reqwest::Client::new();
        let manager = Arc::new(manager);
        let (a, b) = tokio::join!(manager.ensure("claude"), manager.ensure("claude"));
        a.unwrap();
        b.unwrap();
        assert_eq!(count.load(Ordering::SeqCst), 1);
        assert_eq!(std::fs::read(manager.binary("claude")).unwrap(), bytes);
        let restarted = Arc::new(Installer::new(dir.path().into(), manager.specs.clone()).unwrap());
        restarted.ensure("claude").await.unwrap();
        assert_eq!(count.load(Ordering::SeqCst), 1);
        assert!(manager.start("../../escape").is_err());
        assert!(!std::fs::read_dir(dir.path()).unwrap().any(|f| f
            .unwrap()
            .file_name()
            .to_string_lossy()
            .starts_with(".install-")));
        server.abort();
    }
    #[tokio::test]
    async fn bad_download_never_installs_and_retry_succeeds() {
        for failure in ["checksum", "truncated", "oversized", "http"] {
            let attempts = Arc::new(AtomicUsize::new(0));
            let count = attempts.clone();
            let bytes = macho();
            let response = bytes.clone();
            let router = Router::new().route(
                "/tool",
                get(move || {
                    let n = count.fetch_add(1, Ordering::SeqCst);
                    let mut bytes = response.clone();
                    async move {
                        if n == 0 {
                            match failure {
                                "checksum" => bytes[5] ^= 1,
                                "truncated" => {
                                    bytes.pop();
                                }
                                "oversized" => bytes.push(0),
                                _ => {
                                    return (
                                        axum::http::StatusCode::SERVICE_UNAVAILABLE,
                                        Vec::new(),
                                    )
                                }
                            }
                        }
                        (axum::http::StatusCode::OK, bytes)
                    }
                }),
            );
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let mut s = spec(&bytes, None);
            s["url"] = json!(format!("http://{}/tool", listener.local_addr().unwrap()));
            let server = tokio::spawn(async move {
                axum::serve(listener, router).await.unwrap();
            });
            let dir = tempfile::tempdir().unwrap();
            let mut manager = Installer::new(dir.path().into(), json!({"claude":s})).unwrap();
            manager.client = reqwest::Client::new();
            let manager = Arc::new(manager);
            assert!(manager.ensure("claude").await.is_err(), "{failure}");
            assert!(!Path::new(&manager.binary("claude")).exists());
            assert_eq!(manager.status("claude").unwrap()["state"], "error");
            manager.ensure("claude").await.unwrap();
            assert_eq!(attempts.load(Ordering::SeqCst), 2);
            server.abort();
        }
    }
    #[tokio::test]
    async fn https_only_and_network_timeout_fail_cleanly() {
        let dir = tempfile::tempdir().unwrap();
        let mut s = spec(&macho(), None);
        s["url"] = json!("http://127.0.0.1:1/tool");
        let manager = Arc::new(Installer::new(dir.path().into(), json!({"claude":s})).unwrap());
        assert!(manager.ensure("claude").await.is_err());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let mut s = spec(&macho(), None);
        s["url"] = json!(format!("http://{}/tool", listener.local_addr().unwrap()));
        let server = tokio::spawn(async move {
            axum::serve(
                listener,
                Router::new().route(
                    "/tool",
                    get(|| async {
                        tokio::time::sleep(Duration::from_secs(5)).await;
                        macho()
                    }),
                ),
            )
            .await
            .unwrap();
        });
        let mut manager = Installer::new(dir.path().into(), json!({"claude":s})).unwrap();
        manager.client = reqwest::Client::builder()
            .timeout(Duration::from_millis(50))
            .build()
            .unwrap();
        assert!(Arc::new(manager).ensure("claude").await.is_err());
        server.abort();
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0);
    }
    #[test]
    fn archives_only_install_the_pinned_regular_executable() {
        for (member, link, succeeds) in [
            ("codex", false, true),
            ("other", false, false),
            ("codex", true, false),
        ] {
            let dir = tempfile::tempdir().unwrap();
            let temp = tempfile::tempdir_in(dir.path()).unwrap();
            let download = temp.path().join("download");
            let bytes = archive(member, link);
            std::fs::write(&download, &bytes).unwrap();
            let destination = dir.path().join("codex");
            std::fs::write(&destination, b"previous version").unwrap();
            let result = install_verified(
                temp,
                &download,
                &destination,
                &dir.path().join("receipt"),
                &spec(&bytes, Some("codex")),
            );
            assert_eq!(result.is_ok(), succeeds);
            assert_eq!(
                std::fs::read(destination).unwrap(),
                if succeeds {
                    macho()
                } else {
                    b"previous version".to_vec()
                }
            );
        }
    }
    #[test]
    fn manifest_has_pinned_https_sources_for_both_architectures() {
        let m: Value = serde_json::from_str(include_str!("provider-tools.json")).unwrap();
        for arch in ["aarch64", "x86_64"] {
            for p in ["claude", "codex", "antigravity"] {
                let s = &m[arch][p];
                let url = url::Url::parse(s["url"].as_str().unwrap()).unwrap();
                assert_eq!(url.scheme(), "https");
                assert!(matches!(
                    url.host_str(),
                    Some("github.com" | "downloads.claude.ai")
                ));
                assert_eq!(
                    hex::decode(s["sha256"].as_str().unwrap()).unwrap().len(),
                    32
                );
                assert!(s["size"].as_u64().unwrap() < MAX_BINARY);
            }
        }
    }
}
