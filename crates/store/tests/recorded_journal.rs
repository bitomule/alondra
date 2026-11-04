use alondra_foundation::TaskId;
use alondra_store::{AuthorityScope, LifecycleTransaction, read_journal};
use rusqlite::{Connection, TransactionBehavior};
#[path = "../../test_support.rs"]
mod test_support;

/// 2026-11-03, t-deps-done-still-waiting: the journal the daemon recorded while its SDK host lost its
/// connection on every second input, with the doubt and resend events of each host that took its
/// place. It was recorded before the rule for a doubtful input of a host Alondra ended changed, and
/// loads as it did: the same doubts, the same settled resends.
#[test]
fn the_journal_recorded_while_a_host_kept_losing_its_connection_loads_as_it_did() {
    let directory = test_support::Directory::new();
    std::fs::create_dir_all(&*directory).unwrap();
    let connection = Connection::open(directory.join("domain.db")).unwrap();
    connection.execute_batch("PRAGMA foreign_keys=ON;").unwrap();
    connection.execute_batch(include_str!("../../alondrad/src/domain_store/schema.sql")).unwrap();
    let mut connection = connection;
    let mut installing = connection.transaction_with_behavior(TransactionBehavior::Immediate).unwrap();
    LifecycleTransaction::install(&mut installing).unwrap();
    installing.commit().unwrap();
    connection.execute_batch(&format!("BEGIN;\n{}\nCOMMIT;", include_str!("../fixtures/sdk-host-disconnect-loop.sql"))).unwrap();
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
