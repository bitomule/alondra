//! The MCP servers the native Claude has besides Alondra's, from `fakes/mcp-servers.json`
//! (`[{"name": "linear", "url": "https://mcp.linear.app/mcp"}]`). Each needs a sign-in until the
//! user signs in once, which every session shares, as Claude's keychain is
//! (`fakes/mcp-signed-in/<name>`); a running session sees it only once it connects the server again.
//! A sign-in listens on loopback as Claude's own listener does: its page sends the browser straight
//! back to `/callback` with a code, and the return signs the server in.
use std::{
    collections::BTreeSet,
    fs,
    io::{BufRead, BufReader, Write},
    net::TcpListener,
    path::PathBuf,
    sync::{Arc, Mutex},
};

use serde::Deserialize;
use serde_json::{Value, json};

use super::Fakes;
use crate::sandbox::{hex, random};

#[derive(Deserialize, Clone)]
struct Server {
    name: String,
    url: String,
}

pub struct Servers {
    servers: Vec<Server>,
    signed_in: PathBuf,
    connected: Mutex<BTreeSet<String>>,
}

impl Servers {
    pub fn load(fakes: &Fakes) -> Arc<Self> {
        let servers: Vec<Server> = fs::read(fakes.dir.join("mcp-servers.json")).ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok()).unwrap_or_default();
        let signed_in = fakes.dir.join("mcp-signed-in");
        let connected = servers.iter().filter(|server| signed_in.join(&server.name).exists()).map(|server| server.name.clone()).collect();
        Arc::new(Self { servers, signed_in, connected: Mutex::new(connected) })
    }

    fn connected(&self, name: &str) -> bool {
        self.connected.lock().is_ok_and(|connected| connected.contains(name))
    }

    /// How every server is, as `mcp_status` and init list them.
    pub fn statuses(&self) -> Vec<Value> {
        let mut listed = vec![json!({"name": "plugin:alondra:alondra", "status": "connected"})];
        for server in &self.servers {
            let status = if self.connected(&server.name) { "connected" } else { "needs-auth" };
            listed.push(json!({"name": server.name, "status": status, "scope": "user", "config": {"type": "http", "url": server.url}}));
        }
        listed
    }

    /// Connects `name` again: it connects once the user has signed in to it.
    pub fn reconnect(&self, name: &str) -> std::result::Result<(), String> {
        if !self.servers.iter().any(|server| server.name == name) {
            return Err(format!("Server not found: {name}"));
        }
        if !self.signed_in.join(name).exists() {
            return Err("Server status: needs-auth".into());
        }
        if let Ok(mut connected) = self.connected.lock() {
            connected.insert(name.to_owned());
        }
        Ok(())
    }

    /// Opens a sign-in to `name`, listening on loopback for the browser's return.
    pub fn authenticate(self: &Arc<Self>, name: &str) -> std::result::Result<Value, String> {
        if !self.servers.iter().any(|server| server.name == name) {
            return Err(format!("Server not found: {name}"));
        }
        let listener = TcpListener::bind("127.0.0.1:0").map_err(|error| error.to_string())?;
        let port = listener.local_addr().map_err(|error| error.to_string())?.port();
        let state = hex(&random::<8>().map_err(|error| error.to_string())?);
        let (servers, server, expected) = (self.clone(), name.to_owned(), state.clone());
        std::thread::spawn(move || servers.listen(&listener, &server, &expected));
        Ok(json!({
            "authUrl": format!("http://localhost:{port}/authorize?state={state}"),
            "requiresUserAction": true, "callbackExpected": true, "redirectScheme": "localhost",
            "state": state, "callbackPort": port,
        }))
    }

    fn listen(&self, listener: &TcpListener, name: &str, state: &str) {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { continue };
            let mut reader = BufReader::new(&stream);
            let mut line = String::new();
            if reader.read_line(&mut line).is_err() || line.is_empty() {
                continue;
            }
            let mut header = String::new();
            while reader.read_line(&mut header).is_ok_and(|read| read > 0) && !header.trim().is_empty() {
                header.clear();
            }
            let target = line.split_whitespace().nth(1).unwrap_or_default().to_owned();
            if target.starts_with("/authorize") {
                let back = format!("/callback?code=sandbox-{}&state={state}", hex(&random::<8>().unwrap_or_default()));
                let _ = write!(stream, "HTTP/1.1 302 Found\r\nLocation: {back}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
                continue;
            }
            let ours = target.starts_with("/callback?") && target.contains(&format!("state={state}")) && target.contains("code=");
            if !ours {
                let _ = write!(stream, "HTTP/1.1 400 Bad Request\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
                continue;
            }
            let signed = fs::create_dir_all(&self.signed_in).and_then(|()| fs::write(self.signed_in.join(name), b"signed in\n"));
            if signed.is_ok() && let Ok(mut connected) = self.connected.lock() {
                connected.insert(name.to_owned());
            }
            let page = format!("Signed in to {name}. You can close this page.");
            let _ = write!(stream, "HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{page}", page.len());
            return;
        }
    }
}
