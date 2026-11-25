//! The frames the hosts of a task wrote on this Mac, read where the daemon keeps them: each SDK
//! host of Claude under `sdk/`, each host of Codex under `codex/` and each of Antigravity under
//! `antigravity/`, one folder per host with its `outbox`. The outbox journals both directions: the commands the daemon sent the host and the
//! receipts and events the host answered with.

use serde_json::Value;
use std::io::Read;
use std::path::{Path, PathBuf};

const MAX_FRAME: usize = 1_048_576;
/// Where each provider's hosts keep their folders, under Alondra's home.
const PROVIDERS: &[(&str, &str)] = &[("claude", "sdk"), ("codex", "codex"), ("antigravity", "antigravity")];

/// One host of the task: its provider and its frames, in the order it wrote them.
#[derive(Debug, PartialEq)]
pub struct Host {
    pub provider: &'static str,
    pub frames: Vec<Value>,
}

/// The frames of an outbox: a 4-byte big-endian length, then that many bytes of JSON. A truncated
/// tail is how a host stops writing.
pub fn frames(path: &Path) -> std::io::Result<Vec<Value>> {
    let mut reader = std::io::BufReader::new(std::fs::File::open(path)?);
    let mut frames = Vec::new();
    loop {
        let mut header = [0u8; 4];
        if reader.read_exact(&mut header).is_err() {
            return Ok(frames);
        }
        let length = u32::from_be_bytes(header) as usize;
        if length == 0 || length > MAX_FRAME {
            return Ok(frames);
        }
        let mut payload = vec![0u8; length];
        if reader.read_exact(&mut payload).is_err() {
            return Ok(frames);
        }
        if let Ok(frame) = serde_json::from_slice(&payload) {
            frames.push(frame);
        }
    }
}

fn belongs_to(frames: &[Value], task: &str) -> bool {
    frames.iter().any(|frame| frame["body"]["execution"]["task_id"] == task)
}

/// Every host of `task` under `home`, the one that wrote last at the end.
pub fn hosts_of(home: &Path, task: &str) -> Vec<Host> {
    let mut found: Vec<(std::time::SystemTime, PathBuf, Host)> = Vec::new();
    for (provider, folder) in PROVIDERS {
        let Ok(entries) = std::fs::read_dir(home.join(folder)) else { continue };
        for outbox in entries.flatten().map(|entry| entry.path().join("outbox")) {
            let Ok(written) = outbox.metadata().and_then(|metadata| metadata.modified()) else { continue };
            let Ok(frames) = frames(&outbox) else { continue };
            if belongs_to(&frames, task) {
                found.push((written, outbox, Host { provider, frames }));
            }
        }
    }
    found.sort_by(|a, b| (a.0, &a.1).cmp(&(b.0, &b.1)));
    found.into_iter().map(|(_, _, host)| host).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn write(path: &Path, frames: &[Value]) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let mut bytes = Vec::new();
        for frame in frames {
            let payload = frame.to_string().into_bytes();
            bytes.extend((payload.len() as u32).to_be_bytes());
            bytes.extend(payload);
        }
        bytes.extend([0, 0, 0, 9, b'{']);
        std::fs::write(path, bytes).unwrap();
    }

    fn command(task: &str) -> Value {
        json!({"body": {"type": "command", "execution": {"task_id": task}, "action": {"type": "start", "input_id": "i", "prompt": "p"}}})
    }

    fn event(task: &str) -> Value {
        json!({"body": {"type": "event", "execution": {"task_id": task}, "observation": {"type": "sdk"}}})
    }

    #[test]
    fn the_hosts_of_a_task_are_read_for_claude_and_for_codex_with_what_the_daemon_sent_them() {
        let home = tempfile::tempdir().unwrap();
        write(&home.path().join("sdk/sdk-a/outbox"), &[command("t1"), event("t1")]);
        write(&home.path().join("sdk/o-b/outbox"), &[command("t2"), event("t2")]);
        write(&home.path().join("codex/sdk-c/outbox"), &[command("t3"), json!({"body": {"type": "acknowledge"}}), event("t3")]);
        let claude = hosts_of(home.path(), "t1");
        assert_eq!(claude, vec![Host { provider: "claude", frames: vec![command("t1"), event("t1")] }], "the truncated tail is where the host stopped");
        let codex = hosts_of(home.path(), "t3");
        assert_eq!(codex.len(), 1);
        assert_eq!(codex[0].provider, "codex");
        assert_eq!(codex[0].frames[0]["body"]["type"], "command");
        assert!(hosts_of(home.path(), "t9").is_empty());
    }
}
