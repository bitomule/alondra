use super::archive_protocol::*;
use std::path::Path;

#[path = "../../test_support.rs"]
mod test_support;

fn command() -> Frame {
    Frame::new(Body::Command(Command {
        fence: Fence { profile_id: "profile".into(), account_id: "account".into(), data_epoch: "epoch".into(), host_id: "host".into() },
        execution: Execution::Task(TaskExecution {
            task_id: "task".into(), agent_id: "node".into(), conversation_id: "conversation".into(),
            process_instance_id: "instance".into(), generation: 1,
        }),
        command_id: "dispatch".into(), deadline_ms: 1,
        action: Action::Offer { input_id: alondra_foundation::MessageUuid::new("00000000-0000-4000-8000-000000000001").unwrap(), prompt: "Update the package".into() },
    }))
}
fn receipt(frame: &Frame, outcome: ReceiptOutcome) -> Vec<u8> {
    let Body::Command(command) = &frame.body else { unreachable!() };
    encode(&Frame::new(Body::Receipt(Receipt {
        fence: command.fence.clone(), execution: command.execution.clone(), command_id: command.command_id.clone(),
        command_sha256: command_digest(command).unwrap(), outcome,
    }))).unwrap()
}
fn offered() -> ReceiptOutcome { ReceiptOutcome::Accepted { phase: ReceiptPhase::Offered } }
fn fixture(frames: &[&[u8]]) -> test_support::Directory {
    let root = test_support::Directory::new();
    let archive = root.join("session.ended-original");
    std::fs::create_dir_all(&archive).unwrap();
    std::fs::write(archive.join("host.lock"), b"verified native session").unwrap();
    std::fs::write(archive.join("outbox"), frames.concat()).unwrap();
    root
}
fn query(root: &Path, frame: &Frame) -> Result<Vec<ArchivedReceipt>, Error> {
    let Body::Command(command) = &frame.body else { unreachable!() };
    archived_receipts(&root.join("session"), &command.fence, &command.execution, &[&encode(frame).unwrap()], |bytes| {
        if bytes != b"verified native session" { return Err(Error::Invalid("native session")); }
        Ok("original-host".into())
    })
}

#[test]
fn an_archived_offered_receipt_is_found_when_the_current_host_only_attached_without_any_write() {
    let frame = command();
    let bytes = encode(&frame).unwrap();
    let offered = receipt(&frame, offered());
    let root = fixture(&[&bytes, &offered]);
    let current = root.join("session");
    std::fs::create_dir(&current).unwrap();
    std::fs::write(current.join("host.lock"), b"current host remains untouched").unwrap();
    let Body::Command(command) = &frame.body else { unreachable!() };
    let attached = encode(&Frame::new(Body::Attached(Attached {
        fence: command.fence.clone(), execution: command.execution.clone(), host_instance_id: "current-host".into(),
        owner_generation: 45, proof: "0".repeat(64),
    }))).unwrap();
    std::fs::write(current.join("outbox"), &attached).unwrap();
    let proofs = query(&root, &frame).unwrap();
    assert_eq!(proofs.len(), 1);
    assert_eq!(proofs[0].command, bytes);
    assert_eq!(proofs[0].outcome, self::offered());
    assert_eq!(proofs[0].receipt, offered[4..]);
    assert_eq!(std::fs::read(current.join("outbox")).unwrap(), attached);
    assert_eq!(std::fs::read(current.join("host.lock")).unwrap(), b"current host remains untouched");
    assert_eq!(std::fs::read(root.join("session.ended-original/outbox")).unwrap(), [&bytes[..], &offered[..]].concat());
    assert_eq!(std::fs::read_dir(&*root).unwrap().count(), 2);
    assert_eq!(query(&root, &frame).unwrap()[0].receipt, proofs[0].receipt);
}

#[test]
fn absent_admitted_only_and_incomplete_archives_prove_no_handoff() {
    let frame = command();
    let bytes = encode(&frame).unwrap();
    let admission = receipt(&frame, ReceiptOutcome::Accepted { phase: ReceiptPhase::Admitted });
    let offered = receipt(&frame, offered());
    for frames in [
        vec![], vec![&bytes[..]], vec![&bytes[..], &admission[..]],
        vec![&bytes[..], &offered[..offered.len()-1]],
        vec![&bytes[..], &offered[..], &[0, 1][..]],
    ] {
        let root = fixture(&frames);
        assert!(query(&root, &frame).unwrap().is_empty());
    }
    let root = fixture(&[&bytes, &offered]);
    std::fs::write(root.join("session.ended-original/uncertain"), b"storage failed").unwrap();
    assert!(query(&root, &frame).unwrap().is_empty());
}

#[test]
fn conflicting_archived_receipts_keep_both_definitive_outcomes() {
    let frame = command();
    let bytes = encode(&frame).unwrap();
    let offered = receipt(&frame, offered());
    let rejected = receipt(&frame, ReceiptOutcome::Rejected { code: "refused".into(), message: "Not offered".into() });
    let root = fixture(&[&bytes, &offered, &offered, &rejected]);
    let proofs = query(&root, &frame).unwrap();
    assert_eq!(proofs.len(), 2, "a contradiction is never replaced by the last receipt");
    assert_eq!(proofs[0].outcome, self::offered());
    assert!(matches!(proofs[1].outcome, ReceiptOutcome::Rejected { .. }));
}

#[test]
fn archived_receipts_require_the_exact_command_digest_scope_and_native_session() {
    let frame = command();
    let bytes = encode(&frame).unwrap();
    let offered = receipt(&frame, offered());
    for field in ["profile", "account", "epoch", "host", "task", "node", "conversation", "instance", "generation", "prompt"] {
        let mut changed = frame.clone();
        let Body::Command(command) = &mut changed.body else { unreachable!() };
        let Execution::Task(execution) = &mut command.execution else { unreachable!() };
        match field {
            "profile" => command.fence.profile_id = "other".into(),
            "account" => command.fence.account_id = "other".into(),
            "epoch" => command.fence.data_epoch = "other".into(),
            "host" => command.fence.host_id = "other".into(),
            "task" => execution.task_id = "other".into(),
            "node" => execution.agent_id = "other".into(),
            "conversation" => execution.conversation_id = "other".into(),
            "instance" => execution.process_instance_id = "other".into(),
            "generation" => execution.generation = 2,
            "prompt" => if let Action::Offer { prompt, .. } = &mut command.action { *prompt = "Different command".into(); },
            _ => unreachable!(),
        }
        let root = fixture(&[&encode(&changed).unwrap(), &offered]);
        assert!(query(&root, &frame).is_err(), "{field}");
    }
    let root = fixture(&[&bytes, &offered]);
    std::fs::write(root.join("session.ended-original/host.lock"), b"different native session").unwrap();
    assert!(query(&root, &frame).is_err());
    let mut changed: Frame = serde_json::from_slice(&offered[4..]).unwrap();
    let Body::Receipt(receipt) = &mut changed.body else { unreachable!() };
    receipt.command_sha256 = "0".repeat(64);
    let root = fixture(&[&bytes, &encode(&changed).unwrap()]);
    assert!(query(&root, &frame).is_err());
    let root = fixture(&[&offered]);
    assert!(query(&root, &frame).is_err(), "a receipt without its recorded command is incomplete evidence");
}
