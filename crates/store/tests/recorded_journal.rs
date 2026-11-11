use alondra_domain::{ActivityView, Event, InputId, Refusal, RuntimeFact, TaskLifecycle, Trigger, TurnId, WaitReason};
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

/// 2026-11-05, t-flaky-daemon-engine-under-load: the user paused the node at revision 56 and Claude
/// went on by itself, three turns the end of a background command woke, until its host ended. That
/// journal, recorded before a pause stopped the host, loads as it was: the pause holds, what the
/// node was told waits, and the pause ends with the same execution taking it.
#[test]
fn the_journal_of_a_node_that_took_turns_of_its_own_while_paused_loads_and_its_pause_ends() {
    let directory = test_support::Directory::new();
    let connection = recorded(&directory, include_str!("../fixtures/user-pause-own-turns.sql"));
    let scope = AuthorityScope::new("6d5130723af5fb06c149dc2641fbef8c47650cb6d8bb6e52ab65b71b04ce8f00".into()).unwrap();
    let journal = read_journal(&connection, &scope, |_, event, _| Ok::<_, alondra_store::Error>(event.clone())).unwrap();
    let task = &journal[&TaskId::from("t-flaky-daemon-engine-under-load")];
    assert_eq!(task.lifecycle.revision(), 73);
    let own_turn = |event: &&Event| matches!(event, Event::Runtime { fact: RuntimeFact::TurnStarted { trigger: Trigger::OwnedContinuation(_), .. }, .. });
    let turns = |events: &[Event]| events.iter().filter(own_turn).count();
    assert_eq!(turns(&task.entries[56..]), 3, "the turns it took while paused");
    let card = InteractionId::from("node-pause-user-1793833237789-t-flaky-daemon-engine-under-load");
    assert_eq!(task.lifecycle.cards(), [(&card, WaitReason::Pause)], "the pause is all it waits on");
    assert_eq!(task.lifecycle.pending_inputs().len(), 1, "what it was told when paused waits for it");

    let ended = task.lifecycle.advanced(73, Event::EndPause { card }).unwrap();
    assert!(ended.cards().is_empty());
    assert_eq!(ended.execution(), task.lifecycle.execution(), "the same session takes it");
}

/// 2026-11-09, a task whose host ended with an input left to resume it: its next Claude took a
/// turn of its own, and the node rested at revision 65 with that input still in its inbox. The turn
/// Claude then started with it was refused as not the one expected, and so was its end: once the
/// pause ended the task read «Esperando a que Claude empiece» for hours, waiting for a turn Claude
/// had already taken. The turn and its end are taken as Claude reports them.
#[test]
fn the_turn_claude_starts_with_an_input_waiting_in_the_inbox_is_taken() {
    let directory = test_support::Directory::new();
    let connection = recorded(&directory, include_str!("../fixtures/turn-of-a-held-input.sql"));
    let scope = AuthorityScope::new("6d5130723af5fb06c149dc2641fbef8c47650cb6d8bb6e52ab65b71b04ce8f00".into()).unwrap();
    let journal = read_journal(&connection, &scope, |_, event, _| Ok::<_, alondra_store::Error>(event.clone())).unwrap();
    let task = &journal[&TaskId::from("t-stuck-start")];
    assert_eq!(task.lifecycle.revision(), 68);
    let mut lifecycle = TaskLifecycle::planned(TaskId::from("t-stuck-start"), InputId::new("input:t-stuck-start:225535").unwrap()).unwrap();
    for (revision, event) in task.entries[..65].iter().enumerate() {
        lifecycle.apply(revision as u64, event.clone()).unwrap();
    }
    let held = InputId::new("input:t-stuck-start:240390").unwrap();
    assert_eq!(lifecycle.activity(), ActivityView::Resting);
    assert_eq!(lifecycle.pending_inputs(), [&held], "the input left to resume the host waits in the inbox");

    let stamp = lifecycle.execution().unwrap().clone();
    let turn = TurnId::new("claude-message:6e9fc896-c9f5-4288-8e5c-69ab7e86c919").unwrap();
    let observed = |lifecycle: &TaskLifecycle, fact: RuntimeFact| Event::Runtime { stamp: stamp.clone(), cursor: lifecycle.cursor().unwrap() + 1, fact };
    let started = observed(&lifecycle, RuntimeFact::TurnStarted { turn: turn.clone(), trigger: Trigger::Input(held.clone()) });
    lifecycle.apply(65, started).unwrap();
    assert_eq!(lifecycle.activity(), ActivityView::Working);
    assert!(lifecycle.pending_inputs().is_empty(), "the turn took it");

    let card = InteractionId::from("stalled-t-stuck-start-1-claude-message-6e9fc896-c9f5-4288-8e5c-69ab7e86c919");
    let ended = observed(&lifecycle, RuntimeFact::TurnEnded { turn, unheard_card: card, wake: None, awaits: None, stalled: true });
    lifecycle.apply(66, ended).unwrap();
    assert_eq!(lifecycle.activity(), ActivityView::Resting, "nothing is left waiting for Claude to start");
}

/// What anyone reads of an aggregate.
fn seen(task: &TaskLifecycle) -> String {
    format!(
        "{:?} {:?} {:?} {:?} {:?} {:?} {:?} {:?} {:?} {:?} {:?} {:?} {:?}",
        task.phase(), task.activity(), task.resource(), task.cursor(), task.turn(), task.turn_stop(), task.pending_inputs(),
        task.cards(), task.live(), task.dependencies(), task.awaits(), task.takeover(), task.execution(),
    )
}

/// Every journal recorded before the provider's facts were never refused loads, revision by
/// revision, into the same aggregate as it did then: the new rules only take what the old ones
/// refused. Each journal's states are kept as a digest of what anyone reads of them at every
/// revision.
#[test]
fn the_recorded_journals_replay_unchanged() {
    let scope = AuthorityScope::new("6d5130723af5fb06c149dc2641fbef8c47650cb6d8bb6e52ab65b71b04ce8f00".into()).unwrap();
    let fixtures = [
        ("disk-pause-resting-node", include_str!("../fixtures/disk-pause-resting-node.sql"), 0x3c0f_0cfd_d5ef_ffe6u64),
        ("sdk-host-disconnect-loop", include_str!("../fixtures/sdk-host-disconnect-loop.sql"), 0xa7aa_3324_b4f2_20e1),
        ("turn-of-a-held-input", include_str!("../fixtures/turn-of-a-held-input.sql"), 0xb1cc_075e_5480_cfc1),
        ("user-pause-own-turns", include_str!("../fixtures/user-pause-own-turns.sql"), 0x9d93_0cad_71cb_b999),
    ];
    let digest = |text: &str, mut hash: u64| {
        for byte in text.bytes() {
            hash = (hash ^ u64::from(byte)).wrapping_mul(0x0100_0000_01b3);
        }
        hash
    };
    let mut changed = Vec::new();
    for (name, fixture, expected) in fixtures {
        let directory = test_support::Directory::new();
        let connection = recorded(&directory, fixture);
        let journal = read_journal(&connection, &scope, |before, event, _| Ok::<_, alondra_store::Error>(format!("{} {event:?}", seen(before)))).unwrap();
        let mut replayed = 0xcbf2_9ce4_8422_2325;
        for (task, read) in &journal {
            replayed = digest(&format!("{task}"), replayed);
            for entry in &read.entries {
                replayed = digest(entry, replayed);
            }
            replayed = digest(&seen(&read.lifecycle), replayed);
        }
        if replayed != expected {
            changed.push(format!("{name}: {replayed:#018x}"));
        }
    }
    assert!(changed.is_empty(), "replayed differently: {}", changed.join(", "));
}
