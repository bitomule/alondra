use alondra_domain::{ActivityView, Event, InputId, Refusal, WaitReason};
use alondra_foundation::{InteractionId, TaskId};
use alondra_store::{AuthorityScope, LifecycleTransaction, read_journal};
use rusqlite::{Connection, TransactionBehavior};
#[path = "../../test_support.rs"]
mod test_support;

/// 2026-11-03, t-deps-done-still-waiting: the journal the daemon recorded while its SDK host lost its
/// connection on every second input, with the doubt and resend events of each host that took its
/// place. It was recorded before the rule for a doubtful input of a host Alondra ended changed, and
/// loads as it did: the same doubts, the same settled resends.
fn recorded(directory: &test_support::Directory, fixture: &str) -> Connection {
    std::fs::create_dir_all(&**directory).unwrap();
    let mut connection = Connection::open(directory.join("domain.db")).unwrap();
    connection.execute_batch("PRAGMA foreign_keys=ON;").unwrap();
    connection.execute_batch(include_str!("../../alondrad/src/domain_store/schema.sql")).unwrap();
    let mut installing = connection.transaction_with_behavior(TransactionBehavior::Immediate).unwrap();
    LifecycleTransaction::install(&mut installing).unwrap();
    installing.commit().unwrap();
    connection.execute_batch(&format!("BEGIN;\n{fixture}\nCOMMIT;")).unwrap();
    connection
}

#[test]
fn the_journal_recorded_while_a_host_kept_losing_its_connection_loads_as_it_did() {
    let directory = test_support::Directory::new();
    let connection = recorded(&directory, include_str!("../fixtures/sdk-host-disconnect-loop.sql"));
    let scope = AuthorityScope::new("6d5130723af5fb06c149dc2641fbef8c47650cb6d8bb6e52ab65b71b04ce8f00".into()).unwrap();
    let journal = read_journal(&connection, &scope, |_, event, _| Ok::<_, alondra_store::Error>(format!("{event:?}"))).unwrap();
    let task = &journal[&TaskId::from("t-deps-done-still-waiting")];
    assert_eq!(task.entries.len(), 132);
    assert_eq!(task.lifecycle.revision(), 132);
    let doubted_at = |revision: usize| task.entries[..revision].iter().filter(|event| event.starts_with("DoubtHandoff")).count();
    let settled_at = |revision: usize| task.entries[..revision].iter().filter(|event| event.starts_with("SettleHandoff")).count();
    assert_eq!((doubted_at(97), settled_at(97)), (2, 0), "both inputs of the first host were in doubt");
    assert_eq!((doubted_at(99), settled_at(99)), (2, 2), "each doubt was settled by a resend");
    assert_eq!(doubted_at(132), 10);
    assert_eq!(settled_at(132), 10);
}

/// 2026-11-04, t-oss-9-export: the gate held the node's next turn while the disk was nearly full;
/// the turn it ran ended, then its host did, with nothing told. The journal loads as it was
/// recorded, still paused, and the pause cannot end with nothing new to tell: the gate tried that
/// 376 times. It ends by telling the node to carry on, which the daemon now records, and the same
/// execution takes that input.
#[test]
fn the_journal_of_a_node_that_rested_through_a_disk_pause_ends_its_pause_by_telling_it_to_carry_on() {
    let directory = test_support::Directory::new();
    let connection = recorded(&directory, include_str!("../fixtures/disk-pause-resting-node.sql"));
    let scope = AuthorityScope::new("6d5130723af5fb06c149dc2641fbef8c47650cb6d8bb6e52ab65b71b04ce8f00".into()).unwrap();
    let journal = read_journal(&connection, &scope, |_, event, _| Ok::<_, alondra_store::Error>(event.clone())).unwrap();
    let task = &journal[&TaskId::from("t-oss-9-export")];
    assert_eq!(task.lifecycle.revision(), 76);
    let card = InteractionId::from("node-pause-disk-1793753433759-t-oss-9-export");
    assert_eq!(task.lifecycle.cards(), [(&card, WaitReason::Pause)], "the disk's pause is all it waits on");
    assert_eq!(task.lifecycle.activity(), ActivityView::Resting);
    assert!(task.lifecycle.pending_inputs().is_empty());
    assert_eq!(task.lifecycle.advanced(76, Event::EndPause { card: card.clone() }).unwrap_err(), Refusal::InvalidState,
        "nothing waits for the resting node");

    let carry_on = InputId::new("input:t-oss-9-export:192012").unwrap();
    let resumed = task.lifecycle.advanced(76, Event::Resume { card, input: carry_on.clone() }).unwrap();
    assert!(resumed.cards().is_empty());
    assert_eq!(resumed.pending_inputs(), [&carry_on]);
    assert_eq!(resumed.execution(), task.lifecycle.execution(), "the same session takes it");
}
