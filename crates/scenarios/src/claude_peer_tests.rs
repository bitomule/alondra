//! The peer is played against the daemon's side of the protocol written with the same codec and
//! the same proofs: if it speaks the real wire, a real owner reads it.

use super::*;
use alondra_agent_protocol::{Action, Attach, HostExpectation, PROTOCOL_VERSION, owner_proof, verify_host_proof};
use alondra_agent_runtime::owned_host::HostSpawn;
use serde_json::json;
use std::io::{Seek, SeekFrom, Write};

const SECRET_HEX: &str = "0101010101010101010101010101010101010101010101010101010101010101";
const INPUT: &str = "11111111-1111-4111-8111-111111111111";

fn fence() -> Value {
    json!({"profile_id": "profile", "account_id": "account", "data_epoch": "epoch", "host_id": "host"})
}

fn execution() -> Value {
    json!({"task_id": "t1", "agent_id": "agent-t1", "conversation_id": "22222222-2222-4222-8222-222222222222", "process_instance_id": "instance", "generation": 1})
}

fn transcript(prompt: &str) -> Transcript {
    let header = json!({"type": "transcript_start", "provider": "claude", "protocol_version": 1, "sdk_version": "0.3.300", "model": "model-a", "captured_at": "2026-11-15", "scenario": "peer", "origin": "written"});
    let offer = json!({"type": "expect_outbound", "frame": {"protocol_version": 1, "body": {"type": "command", "action": {"type": "offer", "input_id": INPUT, "prompt": prompt}}}});
    let admitted = json!({"type": "emit_inbound", "frame": {"protocol_version": 1, "body": {"type": "receipt", "outcome": {"status": "accepted", "phase": "applied"}}}});
    let exited = json!({"type": "emit_inbound", "frame": {"protocol_version": 1, "body": {"type": "event", "observation": {"type": "process_exited", "exit_code": 0, "signal": null}}}});
    Transcript::parse(&format!("{header}\n{offer}\n{admitted}\n{exited}\n")).unwrap()
}

struct Started {
    _directory: tempfile::TempDir,
    seen: Arc<Mutex<Seen>>,
    child: Child,
    socket: PathBuf,
}

impl Drop for Started {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn start(prompt: &str) -> Started {
    let directory = tempfile::Builder::new().prefix("pe-").tempdir().unwrap();
    let socket = directory.path().canonicalize().unwrap().join("s");
    let bootstrap = json!({
        "socket_path": socket, "host_instance_id": "host-instance", "control_secret": SECRET_HEX,
        "fence": fence(), "execution": execution(),
        "manifest": {"host_release": "0.3.45", "host_sha256": "a".repeat(64), "executable_sha256": "b".repeat(64), "sdk_version": "0.3.300"},
    });
    let mut file = tempfile::tempfile().unwrap();
    file.write_all(bootstrap.to_string().as_bytes()).unwrap();
    file.seek(SeekFrom::Start(0)).unwrap();
    let launcher = PeerLauncher::new(transcript(prompt));
    let child = launcher
        .spawn(HostSpawn {
            executable: std::path::Path::new("/bin/sleep"),
            artifact: std::path::Path::new("/bin/sleep"),
            state_dir: directory.path(),
            environment: &Default::default(),
            confinement: None,
            stdin: file,
        })
        .unwrap();
    Started { _directory: directory, seen: launcher.seen.clone(), child, socket }
}

/// The owner's side of the attach: the host's hello, its challenge, the owner's proof and the
/// host's answer, checked as the daemon checks them.
fn attach(started: &Started) -> (FramedReader<UnixStream>, FramedWriter<UnixStream>) {
    let stream = UnixStream::connect(&started.socket).unwrap();
    let mut writer = FramedWriter::new(stream.try_clone().unwrap());
    let mut reader = FramedReader::new(stream);
    let fence: Fence = serde_json::from_value(fence()).unwrap();
    let Body::Hello(hello) = reader.read().unwrap().unwrap().body else { panic!("the host speaks first, with its hello") };
    hello
        .negotiate(&HostExpectation {
            fence: &fence,
            host_release: "0.3.45",
            host_sha256: &"a".repeat(64),
            sdk_version: "0.3.300",
            required_capabilities: &["streaming_input"],
        })
        .unwrap();
    let Body::Challenge(challenge) = reader.read().unwrap().unwrap().body else { panic!("then its challenge") };
    let secret = decode_secret(SECRET_HEX).unwrap();
    let mut attach = Attach {
        fence: challenge.fence,
        execution: challenge.execution,
        host_instance_id: challenge.host_instance_id,
        nonce: challenge.nonce,
        owner_generation: 1,
        host_release: "0.3.45".into(),
        host_sha256: "a".repeat(64),
        executable_sha256: challenge.executable_sha256,
        sdk_version: "0.3.300".into(),
        proof: String::new(),
    };
    attach.proof = owner_proof(&secret, &attach).unwrap();
    writer.write(&Frame::new(Body::Attach(attach.clone()))).unwrap();
    let Body::Attached(attached) = reader.read().unwrap().unwrap().body else { panic!("then the proof of the host") };
    verify_host_proof(&secret, &attach, &attached).unwrap();
    (reader, writer)
}

fn offer(prompt: &str) -> Command {
    serde_json::from_value(json!({
        "fence": fence(), "execution": execution(), "command_id": "33333333-3333-4333-8333-333333333333", "deadline_ms": 1,
        "action": {"type": "offer", "input_id": INPUT, "prompt": prompt},
    }))
    .unwrap()
}

#[test]
fn the_peer_answers_a_command_the_transcript_expected_with_the_frames_that_follow_it() {
    let started = start("«user text 1»");
    let (mut reader, mut writer) = attach(&started);
    let command = offer("«user text 1»");
    writer.write(&Frame::new(Body::Command(command.clone()))).unwrap();
    let Body::Receipt(receipt) = reader.read().unwrap().unwrap().body else { panic!("a receipt answers the command") };
    assert_eq!(receipt.command_sha256, command_digest(&command).unwrap());
    let Body::Event(event) = reader.read().unwrap().unwrap().body else { panic!("then the event of the transcript") };
    assert_eq!(event.cursor, 1);
    assert!(started.seen.lock().unwrap().failure.is_none());
    assert!(matches!(command.action, Action::Offer { .. }) && PROTOCOL_VERSION == 1);
}

#[test]
fn a_command_the_transcript_did_not_expect_is_a_mismatch_with_both_frames() {
    let started = start("«user text 1»");
    let (mut reader, mut writer) = attach(&started);
    writer.write(&Frame::new(Body::Command(offer("«user text 2»")))).unwrap();
    assert!(reader.read().map(|frame| frame.is_none()).unwrap_or(true), "the peer stops answering");
    match started.seen.lock().unwrap().failure.clone() {
        Some(PeerError::Mismatch { expected: Some(expected), received }) => {
            assert!(expected.to_string().contains("«user text 1»") && received.to_string().contains("«user text 2»"));
        }
        other => panic!("expected a mismatch, found {other:?}"),
    }
}

#[test]
fn the_prompt_of_an_offer_only_has_to_contain_what_the_transcript_names() {
    let frame = |prompt: &str, input: &str| json!({"protocol_version": 1, "body": {"type": "command", "action": {"type": "offer", "input_id": input, "prompt": prompt}}});
    let expected = frame("Add the CSV export", "");
    assert!(is_expected(&expected, &frame("Resumed. Add the CSV export, then stop.", "input-9")));
    assert!(!is_expected(&expected, &frame("Something else", "input-9")));
}

#[test]
fn what_the_daemon_assigns_replaces_the_name_a_transcript_gives_it() {
    let mut payload = json!({"params": {"inputId": "«input»", "items": ["«input»", "«conversation»"]}});
    name_assigned(&mut payload, "«input»", &json!("input-9"));
    name_assigned(&mut payload, "«conversation»", &json!("conversation-3"));
    assert_eq!(payload, json!({"params": {"inputId": "input-9", "items": ["input-9", "conversation-3"]}}));
}
