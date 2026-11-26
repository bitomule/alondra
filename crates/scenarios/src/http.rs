//! The phone of a scenario, a paired device that reads the views and sends the actions to the
//! daemon's own listener on loopback, signing every request as the apps do; and the node's
//! calls of its tools, which reach the same listener with the session's capability.

use alondrad::assemble::Assembled;
use base64::Engine;
use p256::ecdsa::signature::Signer;
use serde_json::Value;
use std::net::SocketAddr;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

/// The secret the phone's keys derive from: the same in every scenario, so every run signs alike.
pub const PHONE_SECRET: [u8; 32] = [9u8; 32];

pub struct Phone {
    addr: SocketAddr,
    key: p256::ecdsa::SigningKey,
    device: String,
    sent: std::cell::Cell<i64>,
}

pub struct Response {
    pub status: u16,
    pub body: String,
}

impl Response {
    pub fn json(&self) -> Value {
        serde_json::from_str(&self.body).unwrap_or(Value::Null)
    }
}

impl Phone {
    pub fn pair(daemon: &Assembled, addr: SocketAddr) -> Result<Self, String> {
        let key = p256::ecdsa::SigningKey::from_slice(&PHONE_SECRET).map_err(|error| error.to_string())?;
        let code = daemon.auth.new_pairing_code();
        let public = base64::engine::general_purpose::STANDARD.encode(key.verifying_key().to_sec1_bytes());
        let device = daemon.auth.pair(&code, "Scenario phone", &public)?;
        Ok(Self { addr, key, device: device.id, sent: std::cell::Cell::new(0) })
    }

    pub fn device(&self) -> &str {
        &self.device
    }

    pub async fn shared_tui_feed_work(&self, daemon: &Assembled) -> Result<(u64, usize), String> {
        let first_timestamp = alondra_core::now_millis() + self.sent.get();
        self.sent.set(self.sent.get() + 32);
        daemon.shared_tui_feed_work_for_test(&PHONE_SECRET, self.device(), first_timestamp).await
    }

    pub async fn get(&self, path: &str) -> std::io::Result<Response> {
        self.send("GET", path, None).await
    }

    pub async fn post(&self, path: &str, body: &Value) -> std::io::Result<Response> {
        self.send("POST", path, Some(body)).await
    }

    async fn send(&self, method: &str, path: &str, body: Option<&Value>) -> std::io::Result<Response> {
        let bytes = body.map(|body| serde_json::to_vec(body).expect("a json value renders")).unwrap_or_default();
        self.sent.set(self.sent.get() + 1);
        let timestamp = alondra_core::now_millis() + self.sent.get();
        let message = alondra_daemon_server::auth::signed_message(method, path, &timestamp.to_string(), &bytes);
        let signature: p256::ecdsa::Signature = self.key.sign(message.as_bytes());
        let signature = base64::engine::general_purpose::STANDARD.encode(signature.to_der().as_bytes());
        let mut request = format!(
            "{method} {path} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\nx-alondra-device: {}\r\nx-alondra-timestamp: {timestamp}\r\nx-alondra-signature: {signature}\r\n",
            self.device
        );
        if body.is_some() {
            request.push_str(&format!("content-type: application/json\r\ncontent-length: {}\r\n", bytes.len()));
        }
        request.push_str("\r\n");
        let mut stream = tokio::net::TcpStream::connect(self.addr).await?;
        stream.write_all(request.as_bytes()).await?;
        stream.write_all(&bytes).await?;
        let mut raw = Vec::new();
        stream.read_to_end(&mut raw).await?;
        parse(&raw)
    }
}

/// A call of a node's tools as `alondra mcp-node` makes it: plain HTTP to the daemon's loopback
/// listener, with the capability the daemon issued to the node's session as its bearer.
pub async fn node_post(addr: SocketAddr, capability: &str, path: &str, body: &Value) -> std::io::Result<Response> {
    let bytes = serde_json::to_vec(body).expect("a json value renders");
    let request = format!(
        "POST {path} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\nauthorization: Bearer {capability}\r\ncontent-type: application/json\r\ncontent-length: {}\r\n\r\n",
        bytes.len()
    );
    let mut stream = tokio::net::TcpStream::connect(addr).await?;
    stream.write_all(request.as_bytes()).await?;
    stream.write_all(&bytes).await?;
    let mut raw = Vec::new();
    stream.read_to_end(&mut raw).await?;
    parse(&raw)
}

pub fn parse(raw: &[u8]) -> std::io::Result<Response> {
    let malformed = || std::io::Error::other("the daemon answered something that is not HTTP");
    let split = raw.windows(4).position(|window| window == b"\r\n\r\n").ok_or_else(malformed)?;
    let head = String::from_utf8_lossy(&raw[..split]).to_string();
    let status = head.split_whitespace().nth(1).and_then(|code| code.parse().ok()).ok_or_else(malformed)?;
    let body = &raw[split + 4..];
    let chunked = head
        .lines()
        .filter_map(|line| line.split_once(':'))
        .any(|(name, value)| name.trim().eq_ignore_ascii_case("transfer-encoding") && value.trim().eq_ignore_ascii_case("chunked"));
    let body = if chunked { unchunk(body).ok_or_else(malformed)? } else { body.to_vec() };
    Ok(Response { status, body: String::from_utf8_lossy(&body).to_string() })
}

fn unchunk(mut body: &[u8]) -> Option<Vec<u8>> {
    let mut out = Vec::new();
    loop {
        let end = body.windows(2).position(|window| window == b"\r\n")?;
        let size = usize::from_str_radix(std::str::from_utf8(&body[..end]).ok()?.trim(), 16).ok()?;
        body = &body[end + 2..];
        if size == 0 {
            return Some(out);
        }
        out.extend_from_slice(body.get(..size)?);
        body = body.get(size + 2..)?;
    }
}
