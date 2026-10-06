use crate::{
    config::{self, text},
    error::{Error, Result},
};
use base64::{engine::general_purpose::STANDARD, Engine};
use futures_util::StreamExt;
use serde_json::{json, Value};
use std::{
    net::{IpAddr, SocketAddr},
    time::Duration,
};
const MAX_FILE: usize = 20 * 1024 * 1024;
pub fn text_of(blocks: &[Value]) -> String {
    blocks
        .iter()
        .filter(|b| b["type"] == "text")
        .map(|b| text(b, "text"))
        .collect::<Vec<_>>()
        .join("\n")
}
pub fn public_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(ip) => {
            let b = ip.octets();
            !(b[0] == 0
                || b[0] == 10
                || b[0] == 127
                || b[0] >= 224
                || (b[0] == 100 && (64..=127).contains(&b[1]))
                || (b[0] == 169 && b[1] == 254)
                || (b[0] == 172 && (16..=31).contains(&b[1]))
                || (b[0] == 192 && (b[1] == 168 || (b[1] == 0 && (b[2] == 0 || b[2] == 2))))
                || (b[0] == 198 && ((18..=19).contains(&b[1]) || (b[1] == 51 && b[2] == 100)))
                || (b[0] == 203 && b[1] == 0 && b[2] == 113))
        }
        IpAddr::V6(ip) => {
            let s = ip.segments();
            s[0] & 0xe000 == 0x2000
                && !(s[0] == 0x2001 && (s[1] < 0x200 || s[1] == 0xdb8))
                && s[0] != 0x2002
        }
    }
}
async fn fetch_image(input: &str) -> Result<Value> {
    let mut url = url::Url::parse(input).map_err(|_| Error::new(400, "Invalid image URL"))?;
    for _ in 0..6 {
        if !["http", "https"].contains(&url.scheme())
            || !url.username().is_empty()
            || url.password().is_some()
        {
            return Err(Error::new(400, "Only public HTTP(S) images are accepted"));
        }
        let host = url
            .host_str()
            .ok_or_else(|| Error::new(400, "Missing host"))?
            .to_string();
        let port = url.port_or_known_default().unwrap_or(443);
        let addresses: Vec<SocketAddr> = tokio::time::timeout(
            Duration::from_secs(5),
            tokio::net::lookup_host((host.as_str(), port)),
        )
        .await
        .map_err(|_| Error::new(400, "DNS resolution timed out"))?
        .map_err(|_| Error::new(400, "Image host not found"))?
        .collect();
        if addresses.is_empty() || addresses.iter().any(|a| !public_ip(a.ip())) {
            return Err(Error::new(400, "The image targets a non-public address"));
        }
        let client = reqwest::Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(Duration::from_secs(20))
            .resolve_to_addrs(&host, &addresses)
            .build()?;
        let r = client.get(url.clone()).send().await?;
        if r.status().is_redirection() {
            let location = r
                .headers()
                .get("location")
                .and_then(|v| v.to_str().ok())
                .ok_or_else(|| Error::new(400, "Invalid redirect"))?;
            url = url
                .join(location)
                .map_err(|_| Error::new(400, "Invalid redirect"))?;
            continue;
        }
        if !r.status().is_success() {
            return Err(Error::new(
                400,
                format!("The remote image returned HTTP {}", r.status()),
            ));
        }
        let mt = r
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .split(';')
            .next()
            .unwrap_or("")
            .to_owned();
        if !mt.starts_with("image/") {
            return Err(Error::new(400, "The URL does not contain an image"));
        }
        if r.content_length().is_some_and(|n| n > MAX_FILE as u64) {
            return Err(Error::new(400, "Image is too large (maximum 20 MiB)"));
        }
        let mut bytes = Vec::new();
        let mut stream = r.bytes_stream();
        while let Some(b) = stream.next().await {
            let b = b?;
            if bytes.len() + b.len() > MAX_FILE {
                return Err(Error::new(400, "Image is too large (maximum 20 MiB)"));
            }
            bytes.extend_from_slice(&b);
        }
        return Ok(
            json!({"type":"image","source":{"type":"base64","media_type":mt,"data":STANDARD.encode(bytes)}}),
        );
    }
    Err(Error::new(400, "Too many image redirects"))
}
fn data_url(input: &str) -> Result<Value> {
    let (prefix, data) = input
        .split_once(",")
        .ok_or_else(|| Error::new(400, "Invalid data URL"))?;
    let mime = prefix
        .strip_prefix("data:")
        .and_then(|p| p.strip_suffix(";base64"))
        .ok_or_else(|| Error::new(400, "Expected a base64 data URL"))?;
    if data.len() > MAX_FILE * 4 / 3 + 4 {
        return Err(Error::new(400, "File is too large (maximum 20 MiB)"));
    }
    let raw = STANDARD
        .decode(data)
        .map_err(|_| Error::new(400, "Invalid base64"))?;
    if mime == "application/pdf" || mime.starts_with("image/") {
        Ok(
            json!({"type":if mime=="application/pdf"{"document"}else{"image"},"source":{"type":"base64","media_type":mime,"data":data}}),
        )
    } else if mime.starts_with("text/") || mime == "application/json" {
        Ok(json!({"type":"text","text":String::from_utf8_lossy(&raw)}))
    } else {
        Err(Error::new(400, format!("Unsupported file type: {mime}")))
    }
}
pub async fn blocks(content: &Value) -> Result<Vec<Value>> {
    if content.is_null() {
        return Ok(vec![]);
    }
    if let Some(s) = content.as_str() {
        return Ok(vec![json!({"type":"text","text":s})]);
    }
    let parts = content
        .as_array()
        .ok_or_else(|| Error::new(400, "Invalid message content"))?;
    let mut out = vec![];
    for p in parts {
        if let Some(s) = p.as_str() {
            out.push(json!({"type":"text","text":s}));
            continue;
        }
        if !p.is_object() {
            return Err(Error::new(400, "Each message part must be an object"));
        }
        out.push(match text(p, "type") {
            "text" | "input_text" | "output_text" => {
                if !p["text"].is_string() {
                    return Err(Error::new(400, "Text must be a string"));
                }
                json!({"type":"text","text":p["text"]})
            }
            "refusal" => json!({"type":"text","text":p["refusal"]}),
            "image_url" | "input_image" => {
                let url = if p["image_url"].is_object() {
                    text(&p["image_url"], "url")
                } else {
                    text(p, "image_url")
                };
                if url.starts_with("data:") {
                    data_url(url)?
                } else {
                    fetch_image(url).await?
                }
            }
            "file" | "input_file" => {
                let f = p.get("file").unwrap_or(p);
                let d = text(f, "file_data");
                if d.is_empty() {
                    return Err(Error::new(400, "Only inline file_data files are accepted"));
                }
                let d = if d.starts_with("data:") {
                    d.to_owned()
                } else {
                    format!("data:application/pdf;base64,{d}")
                };
                let mut b = data_url(&d)?;
                if let Some(name) = f.get("filename") {
                    b["title"] = name.clone();
                }
                b
            }
            "input_audio" | "audio" => return Err(Error::new(400, "Audio input is not supported")),
            _ => json!({"type":"text","text":p.to_string()}),
        });
    }
    Ok(out)
}
async fn pdf_command(binary: &str, args: &[String]) -> Result<Vec<u8>> {
    let mut c = tokio::process::Command::new(binary);
    c.args(args).env("LC_ALL", "C").kill_on_drop(true);
    let out = tokio::time::timeout(Duration::from_secs(30), c.output())
        .await
        .map_err(|_| Error::new(400, "PDF conversion timed out"))?
        .map_err(|_| {
            Error::new(
                503,
                "PDF conversion unavailable: install poppler-utils (included in Docker).",
            )
        })?;
    if !out.status.success() {
        return Err(Error::new(400, "Unreadable or protected PDF"));
    }
    Ok(out.stdout)
}
pub async fn expand_documents(blocks: Vec<Value>) -> Result<Vec<Value>> {
    let mut out = vec![];
    for b in blocks {
        if b["type"] != "document" {
            out.push(b);
            continue;
        }
        let raw = STANDARD
            .decode(text(&b["source"], "data"))
            .map_err(|_| Error::new(400, "Invalid PDF base64"))?;
        let tmp = tempfile::tempdir()?;
        let path = tmp.path().join("input.pdf");
        tokio::fs::write(&path, raw).await?;
        let path = path.to_string_lossy().to_string();
        let info = pdf_command("pdfinfo", std::slice::from_ref(&path)).await?;
        let info = String::from_utf8_lossy(&info);
        let pages = info
            .lines()
            .find_map(|l| {
                l.strip_prefix("Pages:")
                    .and_then(|s| s.trim().parse::<usize>().ok())
            })
            .ok_or_else(|| Error::new(400, "Cannot determine PDF page count"))?;
        if pages > config::number("REMOTE_PDF_MAX_PAGES", 500) as usize {
            return Err(Error::new(400, "PDF exceeds the page limit"));
        }
        let bytes = pdf_command("pdftotext", &["-layout".into(), path.clone(), "-".into()]).await?;
        let extracted = String::from_utf8_lossy(&bytes);
        let texts: Vec<&str> = extracted.split('\x0c').take(pages).collect();
        let mode = config::envs("REMOTE_PDF_MODE", "auto");
        let max_images = config::number("REMOTE_PDF_MAX_IMAGE_PAGES", 20) as usize;
        let max_chars = config::number("REMOTE_PDF_MAX_TEXT_CHARS", 400000) as usize;
        let mut rendered = vec![];
        let mut doc = format!("<document pages=\"{pages}\">\n");
        let mut used = 0;
        for i in 0..pages {
            let txt = texts.get(i).copied().unwrap_or("").trim();
            let render = rendered.len() / 2 < max_images
                && (mode == "images" || (mode == "auto" && txt.chars().count() < 40));
            let body = if txt.is_empty() {
                if render {
                    "[page without text: see the attached image]"
                } else {
                    "[page without text]"
                }
            } else {
                txt
            };
            let body = body
                .chars()
                .take(max_chars.saturating_sub(used))
                .collect::<String>();
            used += body.chars().count();
            doc += &format!("<page number=\"{}\">\n{}\n</page>\n", i + 1, body);
            if render {
                let prefix = tmp.path().join(format!("page-{i}"));
                pdf_command(
                    "pdftoppm",
                    &[
                        "-f".into(),
                        (i + 1).to_string(),
                        "-l".into(),
                        (i + 1).to_string(),
                        "-scale-to".into(),
                        "2048".into(),
                        "-singlefile".into(),
                        "-png".into(),
                        path.clone(),
                        prefix.to_string_lossy().into(),
                    ],
                )
                .await?;
                let png = tokio::fs::read(prefix.with_extension("png")).await?;
                rendered.push(json!({"type":"text","text":format!("PDF page {}:",i+1)}));
                rendered.push(json!({"type":"image","detail":"high","source":{"type":"base64","media_type":"image/png","data":STANDARD.encode(png)}}));
            }
        }
        doc += "</document>";
        out.push(json!({"type":"text","text":doc}));
        out.extend(rendered);
    }
    Ok(out)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn prevents_internal_fetches() {
        for ip in [
            "127.0.0.1",
            "10.1.2.3",
            "192.168.1.1",
            "100.100.100.200",
            "169.254.169.254",
            "::1",
            "::ffff:127.0.0.1",
            "fe80::1",
            "2001:db8::1",
        ] {
            assert!(!public_ip(ip.parse().unwrap()), "{ip}");
        }
        assert!(public_ip("8.8.8.8".parse().unwrap()));
    }
    #[test]
    fn validates_base64() {
        assert!(data_url("data:image/png;base64,not base64!").is_err());
        assert_eq!(
            data_url("data:text/plain;base64,aGk=").unwrap()["text"],
            "hi"
        );
    }
}
