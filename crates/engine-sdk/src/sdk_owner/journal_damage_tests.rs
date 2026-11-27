//! A host whose journal a full disk damaged: its write of an event landed but failed, so it wrote
//! the next one under the same cursor. The takeover of its task keeps what came before the damage
//! and starts a host that resumes the same session, whether it runs at the daemon's start, when a
//! pause ends or when the user tries again.
use std::io::Write as _;
use super::*;

fn runtime(domain: &Arc<DomainRuntime>) -> Arc<dyn alondra_engine_ports::Runtime> {
    domain.clone()
}

/// What the host wrote once the disk filled (27 November): an event whose write landed but failed,
/// so the host never counted it, and its exit under the same cursor.
fn repeat_a_cursor(folder: &Path, session: &str) {
    use alondra_codex_protocol::{Body, Event, Frame, FramedReader, NativeCorrelation, Observation, SdkEventKind, encode};
    let outbox = folder.join("outbox");
    let mut reader = FramedReader::new(std::fs::File::open(&outbox).unwrap());
    let mut last = None;
    while let Some(frame) = reader.read().unwrap() {
        if let Body::Event(event) = frame.body {
            last = Some(event);
        }
    }
    let last = last.expect("the host journaled what its Claude did");
    let status = Observation::Sdk {
        kind: SdkEventKind::Status,
        native: NativeCorrelation::default(),
        payload: serde_json::json!({"type": "system", "subtype": "status", "status": null, "session_id": session}),
        failures: vec![],
    };
    let landed = Event { cursor: last.cursor + 1, observation: status, ..last.clone() };
    let exited = Event { cursor: last.cursor + 1, observation: Observation::ProcessExited { exit_code: Some(143), signal: None }, ..last };
    let mut file = std::fs::OpenOptions::new().append(true).open(&outbox).unwrap();
    for event in [landed, exited] {
        file.write_all(&encode(&Frame::new(Body::Event(event))).unwrap()).unwrap();
    }
}

/// The node works, its host dies with a journal whose last cursor is written twice, and the daemon
/// goes down with it. With `disk_pause`, the disk's floor paused the node first.
fn damaged(slice: &Slice, disk_pause: bool) -> (String, AgentId) {
    let mut driver = slice.launch_with("consume-and-hold");
    slice.until_working(&mut driver);
    let session = slice.starts()[0]["session"].as_str().unwrap().to_owned();
    let agent = driver.agent().clone();
    if disk_pause {
        assert!(slice.domain.gate().hold_back(&agent.0, alondra_engine_ports::admission::Reason::Disk, alondra_core::now_millis()));
    }
    let folder = slice.backend.state_root.join(&agent.0);
    slice.kill_host_and_daemon(driver);
    repeat_a_cursor(&folder, &session);
    (session, agent)
}

/// Claude is told `told` in the same session it ran before.
async fn resumes(slice: &Slice, restarted: &Arc<DomainRuntime>, session: &str, told: &str) {
    let mut changed = restarted.subscribe();
    changed.wait_for(|_| slice.hosts.prompts().iter().any(|prompt| prompt.starts_with(told))).await.unwrap();
    restarted.stop_sdk_owners();
    let starts = slice.starts();
    assert_eq!(starts.last().unwrap()["resume"].as_str(), Some(session), "the same session is resumed: {starts:?}");
}

#[cfg(not(feature = "real-hosts"))]
// 27 November: the disk filled while two nodes worked, and the floor paused them. Each host's
// journal ended with a cursor written twice, so every takeover failed with InvalidObservation:
// quietly at the daemon's start while the pause held, and with the card to try again once the pause
// ended. The upgrade at 18:59 changed nothing, and no host ever took the tasks again.
#[tokio::test(flavor = "multi_thread")]
async fn a_node_the_disk_paused_whose_journal_repeats_a_cursor_resumes_its_session_once_the_pause_ends() {
    let slice = Slice::new();
    let (session, agent) = damaged(&slice, true);
    let restarted = slice.restart();
    restarted.enable_sdk(slice.backend.clone());
    restarted.admission().ignore_mac();
    assert!(restarted.admission().paused(&*restarted, &agent.0).is_some(), "the node is still paused after the restart");
    adopt_all(&runtime(&restarted), &slice.backend);
    crate::admission::pass_at(&*restarted, &slice._home, alondra_core::now_millis()).await.unwrap();
    restarted.join_retakes();
    assert!(restarted.admission().paused(&*restarted, &agent.0).is_none(), "the disk has room: the pause ended");
    assert!(open_card(&restarted, HOST_LOST_PREFIX).is_none(), "no card asks the user to try again: {:?}", open_card(&restarted, HOST_LOST_PREFIX).and_then(|card| card.card.content));
    assert!(restarted.owns_sdk_host(&agent), "a host took the execution over");
    resumes(&slice, &restarted, &session, "Alondra held your next turn while the disk was nearly full. There is room again").await;
}

#[cfg(not(feature = "real-hosts"))]
// The same journal, on a daemon whose takeover failed already and put the card: «Try again» takes
// the execution over with a host that resumes the same session, and the card closes. Before, every
// «Try again» failed as the first takeover had, and put the card back.
#[tokio::test(flavor = "multi_thread")]
async fn try_again_on_a_node_whose_journal_repeats_a_cursor_resumes_its_session() {
    let slice = Slice::new();
    let (session, agent) = damaged(&slice, false);
    let refuse = |refused: bool| std::fs::set_permissions(&slice.backend.state_root, std::fs::Permissions::from_mode(if refused { 0o500 } else { 0o700 })).unwrap();
    refuse(true);
    let restarted = slice.restart();
    restarted.enable_sdk(slice.backend.clone());
    adopt_all(&runtime(&restarted), &slice.backend);
    let card = open_card(&restarted, HOST_LOST_PREFIX).expect("no new host could start: the task has a card");
    refuse(false);
    answer(&restarted, &card.id, RETAKE);
    restarted.join_retakes();
    assert!(restarted.owns_sdk_host(&agent), "«Try again» took the execution over");
    assert!(open_card(&restarted, HOST_LOST_PREFIX).is_none(), "the card closed");
    resumes(&slice, &restarted, &session, "Your session was cut off when its host stopped").await;
}
