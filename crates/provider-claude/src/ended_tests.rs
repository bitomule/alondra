use super::*;
use alondra_agent_protocol::{
    Action, Command, Event, Frame, NativeCorrelation, Observation, Receipt, SdkEventKind, TaskExecution, encode,
};
use alondra_foundation::{AgentId, CommandId, DataEpoch, MessageUuid, TaskId};
use std::io::Write;

#[path = "../../test_support.rs"]
mod test_support;

fn fence() -> Fence {
    Fence { profile_id: "alondra".into(), account_id: "native-login".into(), data_epoch: DataEpoch::from("epoch"), host_id: "alondra-sdk".into() }
}
fn execution(generation: u64) -> Execution {
    Execution::Task(TaskExecution {
        task_id: TaskId::from("task"),
        agent_id: AgentId::from("node"),
        conversation_id: "conversation".into(),
        process_instance_id: "instance".into(),
        generation,
    })
}
fn command(id: &str, prompt: &str) -> Vec<u8> {
    encode(&Frame::new(Body::Command(Command {
        fence: fence(),
        execution: execution(1),
        command_id: CommandId::from(id),
        deadline_ms: 1,
        action: Action::Offer { input_id: MessageUuid::new("10000000-0000-4000-8000-000000000001").unwrap(), prompt: prompt.into() },
    })))
    .unwrap()
}
fn receipt(command: &[u8], outcome: ReceiptOutcome) -> Vec<u8> {
    let received = FramedReader::new(std::io::Cursor::new(command)).read_received().unwrap().unwrap();
    let Body::Command(sent) = &received.frame().body else { unreachable!() };
    encode(&Frame::new(Body::Receipt(Receipt {
        fence: fence(),
        execution: execution(1),
        command_id: sent.command_id.clone(),
        command_sha256: received.digest().into(),
        outcome,
    })))
    .unwrap()
}
fn admitted() -> ReceiptOutcome {
    ReceiptOutcome::Accepted { phase: ReceiptPhase::Admitted }
}
fn event(cursor: u64, generation: u64) -> Vec<u8> {
    encode(&Frame::new(Body::Event(Event {
        fence: fence(),
        execution: execution(generation),
        cursor,
        observation: Observation::Sdk {
            kind: SdkEventKind::Status,
            native: NativeCorrelation::default(),
            payload: serde_json::json!({"type": "system", "subtype": "status", "session_id": "10000000-0000-4000-8000-000000000009"}),
            failures: vec![],
        },
    })))
    .unwrap()
}
fn outbox(directory: &Path, frames: &[&[u8]]) {
    let mut file = std::fs::File::create(directory.join("outbox")).unwrap();
    for frame in frames {
        file.write_all(frame).unwrap();
    }
}
fn directory() -> test_support::Directory {
    let directory = test_support::Directory::new();
    std::fs::create_dir(&*directory).unwrap();
    directory
}
fn read(directory: &Path, committed: u64) -> Result<EndedJournal, AdapterError> {
    EndedJournal::read(directory, &fence(), &execution(1), "host-instance", committed)
}

#[test]
fn each_command_is_answered_by_the_last_durable_receipt_the_host_kept() {
    let directory = directory();
    let (offered, applied, refused, admitted_only, absent) =
        (command("offered", "a"), command("applied", "b"), command("refused", "c"), command("admitted", "d"), command("absent", "e"));
    outbox(&directory, &[
        &offered,
        &receipt(&offered, admitted()),
        &receipt(&offered, ReceiptOutcome::Accepted { phase: ReceiptPhase::Offered }),
        &applied,
        &receipt(&applied, admitted()),
        &receipt(&applied, ReceiptOutcome::Accepted { phase: ReceiptPhase::Applied }),
        &refused,
        &receipt(&refused, admitted()),
        &receipt(&refused, ReceiptOutcome::Rejected { code: "single_turn".into(), message: "one turn".into() }),
        &admitted_only,
        &receipt(&admitted_only, admitted()),
    ]);
    let journal = read(&directory, 0).unwrap();
    assert_eq!(journal.retained(&offered).unwrap(), Retained::Offered);
    assert_eq!(journal.retained(&applied).unwrap(), Retained::Offered);
    assert_eq!(journal.retained(&refused).unwrap(), Retained::Refused);
    assert_eq!(journal.retained(&admitted_only).unwrap(), Retained::AdmittedOnly, "the action may have run: its outcome is unknown");
    assert_eq!(journal.retained(&absent).unwrap(), Retained::Absent, "the host never took it");
    assert!(
        journal.retained(&command("offered", "other bytes")).is_err(),
        "the same command id with other bytes is not the command the host took"
    );
}

#[test]
fn without_trustworthy_storage_a_command_the_host_does_not_show_stays_uncertain() {
    let directory = directory();
    let taken = command("taken", "a");
    outbox(&directory, &[&taken, &receipt(&taken, ReceiptOutcome::Accepted { phase: ReceiptPhase::Offered })]);
    std::fs::write(directory.join("uncertain"), b"Technical evidence storage uncertain\n").unwrap();
    let journal = read(&directory, 0).unwrap();
    assert_eq!(journal.retained(&taken).unwrap(), Retained::Offered, "a durable receipt is still evidence");
    assert_eq!(journal.retained(&command("lost", "b")).unwrap(), Retained::AdmittedOnly);
}

#[test]
fn a_frame_cut_short_ends_the_journal_and_proves_nothing_beyond_it() {
    let directory = directory();
    let taken = command("taken", "a");
    let offered = receipt(&taken, ReceiptOutcome::Accepted { phase: ReceiptPhase::Offered });
    outbox(&directory, &[&taken, &receipt(&taken, admitted()), &offered[..offered.len() - 3]]);
    let journal = read(&directory, 0).unwrap();
    assert_eq!(journal.retained(&taken).unwrap(), Retained::AdmittedOnly, "an offer whose receipt never became durable stays uncertain");
    let original = std::fs::read(directory.join("outbox")).unwrap();
    assert_eq!(read(&directory, 0).unwrap().retained(&taken).unwrap(), Retained::AdmittedOnly);
    assert_eq!(std::fs::read(directory.join("outbox")).unwrap(), original, "recovery never truncates the evidence");
}

#[test]
fn corrupt_tail_preserves_valid_prefix_and_never_proves_an_absent_command_was_rejected() {
    let directory = directory();
    let taken = command("taken", "a");
    let offered = receipt(&taken, ReceiptOutcome::Accepted { phase: ReceiptPhase::Offered });
    let first = event(1, 1);
    outbox(&directory, &[&taken, &receipt(&taken, admitted()), &offered, &first, b"\0\0\0\x01{", &event(2, 1)]);
    let original = std::fs::read(directory.join("outbox")).unwrap();
    for committed in [0, 1] {
        let journal = read(&directory, committed).unwrap();
        assert_eq!(journal.retained(&taken).unwrap(), Retained::Offered);
        assert_eq!(journal.retained(&command("unseen", "b")).unwrap(), Retained::AdmittedOnly);
        assert_eq!(journal.last_cursor(), 1, "no event beyond the corrupt frame is invented");
        assert_eq!(journal.events().len(), usize::from(committed == 0));
        if committed == 0 { assert_eq!(journal.events()[0].payload, first[4..]); }
    }
    assert_eq!(std::fs::read(directory.join("outbox")).unwrap(), original);
}

#[test]
fn only_the_events_after_the_committed_head_come_back_with_their_exact_bytes() {
    let directory = directory();
    let (first, second, third) = (event(1, 1), event(2, 1), event(3, 1));
    outbox(&directory, &[&first, &second, &third]);
    let journal = read(&directory, 1).unwrap();
    let cursors: Vec<u64> = journal.events().iter().map(|event| event.cursor).collect();
    assert_eq!(cursors, [2, 3]);
    assert_eq!(journal.events()[0].payload, second[4..]);
    assert_eq!(journal.last_cursor(), 3);
    assert_eq!(read(&directory, 3).unwrap().last_cursor(), 3, "nothing new: the next host continues from the head");
}

#[test]
fn a_frame_of_another_execution_refuses_the_whole_journal() {
    let directory = directory();
    outbox(&directory, &[&event(1, 1), &event(2, 2)]);
    assert!(read(&directory, 0).is_err());
}

#[test]
fn a_host_that_never_wrote_its_journal_took_nothing() {
    let directory = directory();
    let journal = read(&directory, 4).unwrap();
    assert_eq!(journal.retained(&command("any", "a")).unwrap(), Retained::Absent);
    assert_eq!(journal.last_cursor(), 4);
}

/// What a host journal compacted after its ACK at cursor 5 holds: the owner grant and that ACK,
/// the commands still unresolved and the events after it.
#[test]
fn a_compacted_journal_starts_after_the_acknowledged_events_and_keeps_the_unresolved_commands() {
    let directory = directory();
    let attached = encode(&Frame::new(Body::Attached(alondra_agent_protocol::Attached {
        fence: fence(), execution: execution(1), host_instance_id: "host-instance".into(), owner_generation: 1, proof: "0".repeat(64),
    })))
    .unwrap();
    let acknowledged = encode(&Frame::new(Body::Acknowledge(alondra_agent_protocol::Acknowledge {
        fence: fence(), execution: execution(1), host_instance_id: "host-instance".into(), owner_generation: 1, cursor: 5,
    })))
    .unwrap();
    let held = command("held", "a");
    outbox(&directory, &[&attached, &acknowledged, &held, &receipt(&held, admitted()), &event(6, 1), &event(7, 1)]);
    let journal = read(&directory, 5).unwrap();
    assert_eq!(journal.events().iter().map(|event| event.cursor).collect::<Vec<_>>(), [6, 7]);
    assert_eq!(journal.last_cursor(), 7);
    assert_eq!(journal.retained(&held).unwrap(), Retained::AdmittedOnly);
    let ahead = read(&directory, 6).unwrap();
    assert_eq!(ahead.events().iter().map(|event| event.cursor).collect::<Vec<_>>(), [7], "what the store already has is left out");
}
