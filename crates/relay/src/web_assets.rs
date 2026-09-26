//! Reviewed static build only: explicit pages and one flat assets directory.
//! No request path is ever joined to a filesystem path. No SPA fallback for APIs.
use crate::Server;
use anyhow::{Context, Result, ensure};
use axum::{
    body::Bytes,
    extract::State,
    http::{Method, StatusCode, Uri, header},
    response::{IntoResponse, Response},
};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use sha2::{Digest, Sha256};
use std::{collections::HashMap, path::Path, sync::Arc};

const PAGES: &[&str] = &[
    "",
    "privacy",
    "install",
    "account",
    "admin",
    "invite",
    "login-error",
    "account-closed",
];
const MAX_FILE: u64 = 8 * 1024 * 1024;
struct Asset {
    bytes: Bytes,
    mime: &'static str,
    csp: Option<String>,
}
pub struct WebAssets {
    files: HashMap<String, Asset>,
}
impl WebAssets {
    pub fn from_env(enabled: bool) -> Result<Option<Self>> {
        let path = match std::env::var("ALONDRA_WEB_ASSET_DIR") {
            Ok(path) => path,
            Err(std::env::VarError::NotPresent) => return Ok(None),
            Err(error) => return Err(error.into()),
        };
        ensure!(
            enabled,
            "web assets require configured browser sessions and account management"
        );
        Ok(Some(Self::load(Path::new(&path))?))
    }
    fn load(root: &Path) -> Result<Self> {
        ensure!(
            root.symlink_metadata()?.is_dir(),
            "web root must be a real directory"
        );
        let mut files = HashMap::new();
        let mut total = 0usize;
        for page in PAGES {
            let directory = root.join(page);
            ensure!(
                directory.symlink_metadata()?.is_dir(),
                "web page must be a real directory"
            );
            let bytes = read_file(&directory.join("index.html"))?;
            total += bytes.len();
            let csp = policy(std::str::from_utf8(&bytes)?)?;
            files.insert(
                format!("/{page}"),
                Asset {
                    bytes,
                    mime: "text/html; charset=utf-8",
                    csp: Some(csp),
                },
            );
        }
        let assets = root.join("assets");
        ensure!(
            assets.symlink_metadata()?.is_dir(),
            "assets must be a real directory"
        );
        for entry in std::fs::read_dir(assets)? {
            ensure!(files.len() < 512, "too many web assets");
            let path = entry?.path();
            let name = path
                .file_name()
                .and_then(|name| name.to_str())
                .context("invalid asset name")?;
            ensure!(
                name.bytes()
                    .all(|c| c.is_ascii_alphanumeric() || b"-_.".contains(&c)),
                "invalid asset name"
            );
            let mime = match path.extension().and_then(|ext| ext.to_str()) {
                Some("js") => "text/javascript; charset=utf-8",
                Some("css") => "text/css; charset=utf-8",
                Some("svg") => "image/svg+xml",
                Some("png") => "image/png",
                Some("woff2") => "font/woff2",
                _ => anyhow::bail!("unexpected web build asset"),
            };
            let bytes = read_file(&path)?;
            total += bytes.len();
            ensure!(total <= 64 * 1024 * 1024, "web build too large");
            files.insert(
                format!("/assets/{name}"),
                Asset {
                    bytes,
                    mime,
                    csp: None,
                },
            );
        }
        Ok(Self { files })
    }
    fn response(&self, method: &Method, path: &str) -> Response {
        if method != Method::GET && method != Method::HEAD {
            return StatusCode::METHOD_NOT_ALLOWED.into_response();
        }
        // Accept one optional trailing slash, never percent-decoding or traversal.
        let path = if path.len() > 1 {
            path.strip_suffix('/').unwrap_or(path)
        } else {
            path
        };
        let Some(asset) = self.files.get(path) else {
            return StatusCode::NOT_FOUND.into_response();
        };
        let mut response = if method == Method::HEAD {
            Bytes::new()
        } else {
            asset.bytes.clone()
        }
        .into_response();
        let headers = response.headers_mut();
        headers.insert(header::CONTENT_TYPE, asset.mime.parse().unwrap());
        headers.insert(
            header::CONTENT_LENGTH,
            asset.bytes.len().to_string().parse().unwrap(),
        );
        headers.insert(header::CACHE_CONTROL, "no-store".parse().unwrap());
        headers.insert("x-content-type-options", "nosniff".parse().unwrap());
        headers.insert("referrer-policy", "no-referrer".parse().unwrap());
        headers.insert("x-frame-options", "DENY".parse().unwrap());
        headers.insert(
            "permissions-policy",
            "camera=(), microphone=(), geolocation=()".parse().unwrap(),
        );
        if let Some(csp) = &asset.csp {
            headers.insert("content-security-policy", csp.parse().unwrap());
        }
        response
    }
}
fn read_file(path: &Path) -> Result<Bytes> {
    let metadata = path.symlink_metadata()?;
    ensure!(
        metadata.is_file() && metadata.len() <= MAX_FILE,
        "invalid or oversized static asset"
    );
    let bytes = std::fs::read(path)?;
    ensure!(bytes.len() <= MAX_FILE as usize, "oversized static asset");
    Ok(bytes.into())
}
fn policy(html: &str) -> Result<String> {
    // Input is compiler-generated HTML from our deployment, never user HTML.
    // Hash exact script bodies, so hydration works without unsafe-inline/eval.
    let mut hashes = Vec::new();
    let mut remainder = html;
    while let Some(start) = remainder.find("<script") {
        remainder = &remainder[start + 7..];
        ensure!(
            remainder.starts_with('>') || remainder.starts_with(char::is_whitespace),
            "unexpected script markup"
        );
        let end = remainder.find('>').context("unterminated script tag")?;
        remainder = &remainder[end + 1..];
        let close = remainder
            .find("</script>")
            .context("unterminated script body")?;
        hashes.push(format!(
            "'sha256-{}'",
            STANDARD.encode(Sha256::digest(&remainder.as_bytes()[..close]))
        ));
        remainder = &remainder[close + 9..];
    }
    hashes.sort();
    hashes.dedup();
    let result = format!(
        "default-src 'none'; script-src 'self' {}; style-src 'self' 'unsafe-inline'; img-src 'self' data:; font-src 'self'; connect-src 'self'; form-action 'self'; base-uri 'none'; object-src 'none'; frame-ancestors 'none'",
        hashes.join(" ")
    );
    ensure!(result.len() <= 16_384, "static CSP too large");
    Ok(result)
}
pub async fn serve(State(server): State<Arc<Server>>, method: Method, uri: Uri) -> Response {
    match &server.web_assets {
        Some(assets) => assets.response(&method, uri.path()),
        None => StatusCode::NOT_FOUND.into_response(),
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn exact_script_hashes_and_no_api_fallback() {
        let csp = policy(
            "<script type=\"module\">hello()</script><script src=\"/assets/main.js\"></script>",
        )
        .unwrap();
        assert!(csp.contains(&STANDARD.encode(Sha256::digest(b"hello()"))));
        assert!(!csp.contains("script-src 'self' 'unsafe-inline'"));
        assert!(policy("<script>unterminated").is_err());
        let site = WebAssets {
            files: HashMap::from([(
                "/".into(),
                Asset {
                    bytes: Bytes::from_static(b"home"),
                    mime: "text/html",
                    csp: Some(csp),
                },
            )]),
        };
        for path in [
            "/v1/missing",
            "/assets/../index.html",
            "/%2e%2e/index.html",
            "/auth/missing",
        ] {
            assert_eq!(
                site.response(&Method::GET, path).status(),
                StatusCode::NOT_FOUND
            );
        }
        assert_eq!(
            site.response(&Method::POST, "/").status(),
            StatusCode::METHOD_NOT_ALLOWED
        );
        let head = site.response(&Method::HEAD, "/");
        assert_eq!(head.headers()[header::CONTENT_LENGTH], "4");
    }
}
