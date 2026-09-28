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

/// Serves hooks until `drain` turns true, then stops accepting and drops every hook still
/// waiting, which releases a held question exactly as a disconnecting Claude would.
pub async fn serve(listener: UnixListener, hub: Arc<Hub>, mut drain: tokio::sync::watch::Receiver<bool>) {
    let mut hooks = tokio::task::JoinSet::new();
    loop {
        tokio::select! {
            accepted = listener.accept() => match accepted {
                Ok((stream, _)) => {
                    let hub = hub.clone();
                    hooks.spawn(async move {
                        if let Err(e) = handle(stream, hub).await {
                            log!("local hook failed: {e}");
                        }
                    });
                }
                Err(e) => log!("local hook accept failed: {e}"),
            },
            Some(_) = hooks.join_next(), if !hooks.is_empty() => (),
            _ = drain.wait_for(|draining| *draining) => break,
        }
    }
    drop(listener);
    if !hooks.is_empty() {
        log!("releasing {} hooks still waiting before restart", hooks.len());
    }
    hooks.abort_all();
    while hooks.join_next().await.is_some() {}
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
        response = crate::http::hook_with_ack(State(hub.clone()), headers, Json(envelope.payload),true) => response,
        result = stream.read_u8() => {
            log!("hook client disconnected while awaiting a decision: {result:?}");
            return Ok(());
        }
    };
    let delivery = response.headers().get("x-alondra-delivery-attempt").and_then(|v|v.to_str().ok()).and_then(|v|v.parse::<u64>().ok());
    let held = response.headers().get("x-alondra-hold-id").and_then(|v| v.to_str().ok()).and_then(|v| v.parse::<u64>().ok());
    let status = response.status().as_u16();
    let bytes = match to_bytes(response.into_body(), MAX_HOOK_BYTES as usize).await {
        Ok(bytes) => bytes,
        Err(error) => {
            if let Some(id)=delivery { crate::delivery::finish(&hub,id,crate::delivery::Outcome::Uncertain("hook_encode_failed"))?; }
            if let Some(id) = held.filter(|_| delivery.is_none()) {
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
        anyhow::ensure!(tokio::time::timeout(std::time::Duration::from_secs(5),stream.read_u8()).await?? == 6, "hook reply was not acknowledged");
        Ok::<(), anyhow::Error>(())
    }.await;
    if let Some(id)=delivery {
        crate::delivery::finish(&hub,id,if sent.is_ok() { crate::delivery::Outcome::Handed }
            else { crate::delivery::Outcome::Uncertain("hook_ack_missing") })?;
    }
    if let Some(id) = held.filter(|_| delivery.is_none()) {
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
    async fn stop_delivery_waits_for_stdout_ack_and_disconnect_is_uncertain() {
        for ack in [Some(6u8),Some(5u8),None] {
            let home=std::env::temp_dir().join(format!("delivery-hook-{}",crate::auth::random_hex(6)));
            let config=Config::resolve(home.clone(),&Default::default(),|_|None,||Some("Test".into()));
            let hub=Arc::new(Hub::open(config,false).unwrap());
            hub.append(alondra_core::Event::Hook(alondra_core::HookEvent {session_id:"interactive".into(),event:"UserPromptSubmit".into(),..Default::default()})).unwrap();
            for id in [1,2] { hub.append(alondra_core::Event::MessageQueued(alondra_core::Message {id,session_id:"interactive".into(),from:alondra_core::MessageFrom::User,text:format!("instruction {id}"),answers_question:None,created_at:1,handed_at:None,delivered_at:None,attachments:vec![],about:None})).unwrap(); }
            let (mut client,server)=UnixStream::pair().unwrap();
            let handler=tokio::spawn(handle(server,hub.clone()));
            let body=br#"{"launched":true,"payload":{"session_id":"interactive","hook_event_name":"Stop"}}"#;
            client.write_u32(body.len() as u32).await.unwrap();client.write_all(body).await.unwrap();
            let mut bytes=vec![];client.read_to_end(&mut bytes).await.unwrap();
            let response:Value=serde_json::from_slice(&bytes[2..]).unwrap();
            assert!(response["hookSpecificOutput"]["additionalContext"].as_str().unwrap().contains("instruction 2"));
            assert!(hub.read(|v|v.messages.values().all(|m|m.handed_at.is_none())));
            assert!(hub.read(|v|v.delivery_blocked("interactive")));
            if let Some(ack)=ack {client.write_u8(ack).await.unwrap();} drop(client);
            let result=handler.await.unwrap();
            let expected=if ack==Some(6) {alondra_core::delivery::State::Handed} else {alondra_core::delivery::State::Uncertain};
            assert_eq!(result.is_ok(),ack==Some(6));
            assert!(hub.read(|v|v.delivery_attempts.values().all(|a|a.state==expected)));
            assert!(hub.read(|v|v.messages.values().all(|m|m.handed_at.is_some()==(ack==Some(6)) && m.delivered_at.is_none())));
            drop(hub);std::fs::remove_dir_all(home).unwrap();
        }
    }

    #[tokio::test]
    async fn stop_http_without_receipt_exposes_uncertainty_and_does_not_replay() {
        let home=std::env::temp_dir().join(format!("delivery-http-hook-{}",crate::auth::random_hex(6)));
        let config=Config::resolve(home.clone(),&Default::default(),|_|None,||Some("Test".into()));
        let hub=Arc::new(Hub::open(config,false).unwrap());
        hub.append(alondra_core::Event::Hook(alondra_core::HookEvent {session_id:"s".into(),event:"UserPromptSubmit".into(),..Default::default()})).unwrap();
        hub.append(alondra_core::Event::MessageQueued(alondra_core::Message {id:1,session_id:"s".into(),from:alondra_core::MessageFrom::User,text:"once".into(),answers_question:None,created_at:1,handed_at:None,delivered_at:None,attachments:vec![],about:None})).unwrap();
        let mut headers=axum::http::HeaderMap::new();headers.insert(alondra_core::LAUNCHED_HEADER,"1".parse().unwrap());
        let payload=serde_json::json!({"session_id":"s","hook_event_name":"Stop"});
        let response=crate::http::hook(State(hub.clone()),headers.clone(),Json(payload.clone())).await;
        assert!(response.headers().contains_key("x-alondra-delivery-attempt"));
        assert!(hub.read(|v|v.delivery_attempts.values().all(|a|a.state==alondra_core::delivery::State::Uncertain)));
        assert!(hub.read(|v|v.messages[&1].handed_at.is_none()));
        let again=crate::http::hook(State(hub.clone()),headers,Json(payload)).await;
        assert!(!again.headers().contains_key("x-alondra-delivery-attempt"));
        drop(hub);std::fs::remove_dir_all(home).unwrap();
    }

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
    async fn held_answer_durable_handoff_requires_socket_ack() {
        for acknowledged in [true, false] {
            let home = std::env::temp_dir().join(format!("alondra-answer-ack-{}", crate::auth::random_hex(6)));
            let sid = "12345678-held-ack";
            let job = home.join("claude/jobs/12345678");
            std::fs::create_dir_all(&job).unwrap();
            std::fs::write(job.join("state.json"), format!("{{\"sessionId\":\"{sid}\"}}")).unwrap();
            let config = Config::resolve(home.clone(), &Default::default(), |_| None, || Some("Test".into()));
            let hub = Arc::new(Hub::open(config, true).unwrap().with_claude_home(home.join("claude")));
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let address = listener.local_addr().unwrap();
            let app = crate::http::router(hub.clone(), Arc::new(crate::auth::Auth::open(&home).unwrap()), Arc::new(crate::push::Push::open(&home).unwrap()));
            let server = tokio::spawn(async move { axum::serve(listener, app.into_make_service_with_connect_info::<std::net::SocketAddr>()).await.unwrap(); });
            let (mut client, socket) = UnixStream::pair().unwrap();
            let handler = tokio::spawn(handle(socket, hub.clone()));
            let payload = serde_json::json!({"launched":true,"payload":{"session_id":sid,"hook_event_name":"PermissionRequest","tool_name":"Bash","tool_use_id":"held-ack","tool_input":{"command":"echo synthetic"}}});
            let bytes = serde_json::to_vec(&payload).unwrap();
            client.write_u32(bytes.len() as u32).await.unwrap();
            client.write_all(&bytes).await.unwrap();
            tokio::time::timeout(std::time::Duration::from_secs(2), async {
                while hub.holds_count() == 0 { tokio::task::yield_now().await; }
            }).await.unwrap();
            let id = hub.read(|v| *v.questions.keys().next().unwrap());
            let response = reqwest::Client::new().post(format!("http://{address}/v1/questions/{id}/answer"))
                .json(&serde_json::json!({"text":"Allow","option":"Allow"})).send().await.unwrap();
            assert!(response.status().is_success());
            let mut received = vec![];
            client.read_to_end(&mut received).await.unwrap();
            assert_eq!(u16::from_be_bytes([received[0], received[1]]), 200);
            let attempt = hub.read(|v| v.delivery_attempts.values().find(|a| a.channel == alondra_core::delivery::Channel::HookDecision).unwrap().id);
            assert_eq!(hub.read(|v| v.delivery_attempts[&attempt].state), alondra_core::delivery::State::Dispatching);
            assert!(hub.read(|v| v.messages.values().all(|m| m.handed_at.is_none())));
            if acknowledged { client.write_u8(6).await.unwrap(); }
            drop(client);
            let result = handler.await.unwrap();
            assert_eq!(result.is_ok(), acknowledged);
            let expected = if acknowledged { alondra_core::delivery::State::Handed } else { alondra_core::delivery::State::Uncertain };
            assert_eq!(hub.read(|v| v.delivery_attempts[&attempt].state), expected);
            assert_eq!(hub.read(|v| v.questions[&id].status), if acknowledged { QuestionStatus::Delivered } else { QuestionStatus::Answered });
            assert_eq!(hub.read(|v| v.delivery_blocked(sid)), !acknowledged);
            server.abort();
            let _ = server.await;
            drop(hub);
            std::fs::remove_dir_all(home).unwrap();
        }
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
    #[tokio::test]
    async fn draining_stops_accepting_and_releases_a_held_hook_like_a_disconnect() {
        let home = std::env::temp_dir().join(format!("alondra-drain-hook-{}", crate::auth::random_hex(6)));
        let sid = "12345678-drain";
        let job = home.join("claude/jobs/12345678");
        std::fs::create_dir_all(&job).unwrap();
        std::fs::write(job.join("state.json"), format!("{{\"sessionId\":\"{sid}\"}}")).unwrap();
        let config = Config::resolve(home.clone(), &Default::default(), |_| None, || Some("Test".into()));
        let hub = Arc::new(Hub::open(config, true).unwrap().with_claude_home(home.join("claude")));
        let listener = bind(&home).await.unwrap();
        let (drain, draining) = tokio::sync::watch::channel(false);
        let served = tokio::spawn(serve(listener, hub.clone(), draining));
        let mut client = UnixStream::connect(home.join("hooks.sock")).await.unwrap();
        let body = format!("{{\"launched\":true,\"payload\":{{\"session_id\":\"{sid}\",\"hook_event_name\":\"PermissionRequest\",\"tool_name\":\"Bash\",\"tool_use_id\":\"tool-1\",\"tool_input\":{{\"command\":\"echo hi\"}}}}}}");
        client.write_u32(body.len() as u32).await.unwrap();
        client.write_all(body.as_bytes()).await.unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            while hub.holds_count() == 0 { tokio::task::yield_now().await; }
        }).await.unwrap();
        let id = hub.read(|v| *v.questions.keys().next().unwrap());
        assert_eq!(hub.read(|v| v.board.tasks.values().next().unwrap().state), TaskState::NeedsYou);
        drain.send(true).unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(5), served).await.unwrap().unwrap();
        assert_eq!(hub.holds_count(), 0);
        assert_eq!(hub.read(|v| v.questions[&id].status), QuestionStatus::Orphaned);
        assert_ne!(hub.read(|v| v.board.tasks.values().next().unwrap().state), TaskState::NeedsYou);
        let mut rest = vec![];
        assert_eq!(client.read_to_end(&mut rest).await.unwrap(), 0, "the hook gets no decision, as on disconnect");
        assert!(UnixStream::connect(home.join("hooks.sock")).await.is_err(), "no new hook is accepted");
        drop(hub);
        std::fs::remove_dir_all(home).unwrap();
    }
}
