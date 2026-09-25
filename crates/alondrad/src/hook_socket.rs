//! Local hook transport. Claude Code starts `alondra hook`; only this daemon writes state.

use std::os::unix::fs::{FileTypeExt, PermissionsExt};
use std::path::Path;
use std::sync::Arc;

use anyhow::{Context, Result, bail};
use axum::body::to_bytes;
use axum::extract::{Json, State};
use serde::Deserialize;
use serde_json::Value;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{UnixListener, UnixStream};

use crate::hub::Hub;

const MAX_HOOK_BYTES: u64 = 25 * 1024 * 1024;

#[derive(Deserialize)]
struct Envelope {
    launched: bool,
    payload: Value,
}

pub async fn bind(home: &Path) -> Result<UnixListener> {
    let path = home.join("hooks.sock");
    if let Ok(meta) = std::fs::symlink_metadata(&path) {
        if !meta.file_type().is_socket() {
            bail!("{} exists but is not a Unix socket", path.display());
        }
        match std::os::unix::net::UnixStream::connect(&path) {
            Ok(_) => bail!("another daemon already owns {}", path.display()),
            Err(e) if e.kind() == std::io::ErrorKind::ConnectionRefused => {}
            Err(e) => return Err(e).with_context(|| format!("checking {}", path.display())),
        }
        std::fs::remove_file(&path).with_context(|| format!("removing stale {}", path.display()))?;
    }
    let listener = UnixListener::bind(&path).with_context(|| format!("binding {}", path.display()))?;
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))?;
    Ok(listener)
}

pub async fn serve(listener: UnixListener, hub: Arc<Hub>) {
    loop {
        match listener.accept().await {
            Ok((stream, _)) => {
                let hub = hub.clone();
                tokio::spawn(async move {
                    if let Err(e) = handle(stream, hub).await {
                        log!("local hook failed: {e}");
                    }
                });
            }
            Err(e) => log!("local hook accept failed: {e}"),
        }
    }
}

async fn handle(mut stream: UnixStream, hub: Arc<Hub>) -> Result<()> {
    let len = stream.read_u32().await? as u64;
    if len > MAX_HOOK_BYTES {
        bail!("hook input exceeds {MAX_HOOK_BYTES} bytes");
    }
    let mut input = vec![0; len as usize];
    stream.read_exact(&mut input).await?;
    let envelope: Envelope = serde_json::from_slice(&input).context("invalid local hook envelope")?;
    let mut headers = axum::http::HeaderMap::new();
    if envelope.launched {
        headers.insert(alondra_core::LAUNCHED_HEADER, axum::http::HeaderValue::from_static("1"));
    }
    // A held permission can wait for days. If Claude exits, dropping the hook future also
    // drops its HoldGuard, which orphans the question and settles its task blocker.
    let response = tokio::select! {
        response = crate::http::hook(State(hub.clone()), headers, Json(envelope.payload)) => response,
        result = stream.read_u8() => {
            log!("hook client disconnected while awaiting a decision: {result:?}");
            return Ok(());
        }
    };
    let held = response.headers().get("x-alondra-hold-id").and_then(|v| v.to_str().ok()).and_then(|v| v.parse::<u64>().ok());
    let status = response.status().as_u16();
    let bytes = match to_bytes(response.into_body(), MAX_HOOK_BYTES as usize).await {
        Ok(bytes) => bytes,
        Err(error) => {
            if let Some(id) = held {
                hub.append(alondra_core::Event::QuestionClosed {
                    id,
                    status: alondra_core::QuestionStatus::Orphaned,
                    note: Some("hook reply could not be encoded".into()),
                })?;
            }
            return Err(error.into());
        }
    };
    let sent = async {
        stream.write_all(&status.to_be_bytes()).await?;
        stream.write_all(&bytes).await?;
        stream.shutdown().await?;
        anyhow::ensure!(stream.read_u8().await? == 6, "hook reply was not acknowledged");
        Ok::<(), anyhow::Error>(())
    }.await;
    if let Some(id) = held {
        let event = if sent.is_ok() {
            alondra_core::Event::QuestionDelivered { id }
        } else {
            alondra_core::Event::QuestionClosed { id, status: alondra_core::QuestionStatus::Orphaned, note: None }
        };
        hub.append(event)?;
    }
    sent?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use alondra_core::config::Config;
    use alondra_core::{QuestionStatus, tasks::TaskState};

    #[tokio::test]
    async fn second_daemon_cannot_replace_a_live_hook_socket() {
        let home = std::env::temp_dir().join(format!("alondra-hook-owner-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&home);
        std::fs::create_dir_all(&home).unwrap();
        let first = bind(&home).await.unwrap();
        assert!(bind(&home).await.unwrap_err().to_string().contains("already owns"));
        assert!(UnixStream::connect(home.join("hooks.sock")).await.is_ok());
        drop(first);
        std::fs::remove_dir_all(home).unwrap();
    }

    #[tokio::test]
    async fn local_hook_reaches_the_single_writer_and_returns_claude_json() {
        let home = std::env::temp_dir().join(format!("alondra-hook-socket-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&home);
        let config = Config::resolve(home.clone(), &Default::default(), |_| None, || Some("David".into()));
        let hub = Arc::new(Hub::open(config, false).unwrap());
        let listener = bind(&home).await.unwrap();
        let mut client = UnixStream::connect(home.join("hooks.sock")).await.unwrap();
        let (server, _) = listener.accept().await.unwrap();
        let received = tokio::spawn(handle(server, hub.clone()));
        let body = br#"{"launched":true,"payload":{"session_id":"socket-node","hook_event_name":"SessionStart"}}"#;
        client.write_u32(body.len() as u32).await.unwrap();
        client.write_all(body).await.unwrap();
        let mut response = Vec::new();
        client.read_to_end(&mut response).await.unwrap();
        client.write_u8(6).await.unwrap();
        received.await.unwrap().unwrap();
        assert_eq!(u16::from_be_bytes([response[0], response[1]]), 200);
        assert_eq!(&response[2..], b"{}");
        assert!(hub.read(|v| v.nodes.contains_key("socket-node")));
        drop(client);
        drop(listener);
        drop(hub);
        std::fs::remove_dir_all(home).unwrap();
    }

    #[tokio::test]
    async fn disconnected_held_hook_orphans_its_question_and_unblocks_the_task() {
        let home = std::env::temp_dir().join(format!("alondra-disconnected-hook-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&home);
        let sid = "12345678-disconnect";
        let job = home.join("claude/jobs/12345678");
        std::fs::create_dir_all(&job).unwrap();
        std::fs::write(job.join("state.json"), format!("{{\"sessionId\":\"{sid}\"}}" )).unwrap();
        let config = Config::resolve(home.clone(), &Default::default(), |_| None, || Some("David".into()));
        let hub = Arc::new(Hub::open(config, true).unwrap().with_claude_home(home.join("claude")));
        let listener = bind(&home).await.unwrap();
        let mut client = UnixStream::connect(home.join("hooks.sock")).await.unwrap();
        let (server, _) = listener.accept().await.unwrap();
        let received = tokio::spawn(handle(server, hub.clone()));
        let body = format!("{{\"launched\":true,\"payload\":{{\"session_id\":\"{sid}\",\"hook_event_name\":\"PermissionRequest\",\"tool_name\":\"Bash\",\"tool_use_id\":\"tool-1\",\"tool_input\":{{\"command\":\"echo hi\"}}}}}}");
        client.write_u32(body.len() as u32).await.unwrap();
        client.write_all(body.as_bytes()).await.unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(2), async {
            while hub.holds_count() == 0 { tokio::task::yield_now().await; }
        }).await.unwrap();
        let id = hub.read(|v| *v.questions.keys().next().unwrap());
        assert_eq!(hub.read(|v| v.board.tasks.values().next().unwrap().state), TaskState::NeedsYou);
        drop(client);
        received.await.unwrap().unwrap();
        assert_eq!(hub.holds_count(), 0);
        assert_eq!(hub.read(|v| v.questions[&id].status), QuestionStatus::Orphaned);
        assert_ne!(hub.read(|v| v.board.tasks.values().next().unwrap().state), TaskState::NeedsYou);
        drop(listener);
        drop(hub);
        std::fs::remove_dir_all(home).unwrap();
    }
}
