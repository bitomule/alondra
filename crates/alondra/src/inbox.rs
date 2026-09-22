use std::io::Read;
use std::time::Duration;

use anyhow::Result;
use serde_json::Value;

use crate::client::Client;
use crate::mcp::urlencode;

const WAIT_SECS: u64 = 3600;

pub fn run() -> Result<i32> {
    let mut payload = String::new();
    std::io::stdin().read_to_string(&mut payload)?;
    let session = serde_json::from_str::<Value>(&payload)
        .ok()
        .and_then(|v| v.get("session_id").and_then(Value::as_str).map(str::to_string));
    let Some(session) = session else { return Ok(0) };
    let client = Client::new(Duration::from_secs(WAIT_SECS + 60));
    let path = format!("/v1/inbox/{}?wait={WAIT_SECS}", urlencode(&session));
    loop {
        match client.get_text(&path) {
            Ok((200, text)) if !text.trim().is_empty() => {
                eprintln!("{text}");
                return Ok(2);
            }
            Ok((_, text)) if text.trim() == "superseded" => return Ok(0),
            Ok(_) => continue,
            Err(_) => std::thread::sleep(Duration::from_secs(15)),
        }
    }
}
