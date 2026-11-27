use super::*;
use alondra_codex_protocol::{Event, Frame, NativeCorrelation, Observation, SdkEventKind, TaskExecution, encode};
use alondra_foundation::{AgentId, DataEpoch, TaskId};
use std::io::Write;

#[path = "../../test_support.rs"]
mod test_support;

fn fence() -> Fence {
    Fence { profile_id: "alondra".into(), account_id: "native-login".into(), data_epoch: DataEpoch::from("epoch"), host_id: "alondra-sdk".into() }
}
fn execution() -> Execution {
    Execution::Task(TaskExecution {
        task_id: TaskId::from("task"),
        agent_id: AgentId::from("node"),
        conversation_id: "conversation".into(),
        process_instance_id: "instance".into(),
        generation: 1,
    })
}
fn event(cursor: u64, observation: Observation) -> Vec<u8> {
    encode(&Frame::new(Body::Event(Event { fence: fence(), execution: execution(), cursor, observation }))).unwrap()
}
fn status(cursor: u64) -> Vec<u8> {
    event(cursor, Observation::Sdk { kind: SdkEventKind::Status, native: NativeCorrelation::default(), payload: serde_json::json!({"type": "status"}), failures: vec![] })
}
fn exited(cursor: u64) -> Vec<u8> {
    event(cursor, Observation::ProcessExited { exit_code: Some(143), signal: None })
}
fn read(frames: &[&[u8]], committed: u64) -> EndedJournal {
    let directory = test_support::Directory::new();
    std::fs::create_dir(&*directory).unwrap();
    let mut file = std::fs::File::create(directory.join("outbox")).unwrap();
    for frame in frames {
        file.write_all(frame).unwrap();
    }
    EndedJournal::read(&directory, &fence(), &execution(), "host-instance", committed).expect("a broken cursor is damage, not another host's journal")
}

/// 27 November: a full disk failed the host's write of an event whose bytes landed, and the host
/// wrote its exit under the same cursor. What came before the break is evidence, nothing after it.
#[test]
fn a_cursor_written_twice_or_skipped_ends_the_journal_there() {
    let (first, second) = (status(1), status(2));
    for frames in [vec![&first[..], &second, &exited(2), &status(3)], vec![&first[..], &second, &status(4)]] {
        let journal = read(&frames, 0);
        assert_eq!(journal.events().iter().map(|event| event.cursor).collect::<Vec<_>>(), [1, 2]);
        assert_eq!(journal.events()[1].payload, second[4..]);
        assert_eq!(journal.last_cursor(), 2);
        assert!(journal.uncertain, "past the break nothing proves a command was never taken");
    }
}
