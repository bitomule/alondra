//! How the engine's fixtures open a runtime: as the test said on its own thread, and a runtime
//! given no gate or hosts says so instead of letting everything through.
use super::*;
use crate::gate::GateOf as _;
use crate::hosts::HostsOf as _;
use std::sync::atomic::{AtomicUsize, Ordering};

static MARKED_OPENS: AtomicUsize = AtomicUsize::new(0);

fn marked(home: &Path, database: &Path, files: &dyn JournalFiles) -> Result<DomainRuntime> {
    MARKED_OPENS.fetch_add(1, Ordering::SeqCst);
    engine_alone(home, database, files)
}

fn home() -> crate::test_support::Directory {
    let home = crate::test_support::Directory::new();
    std::fs::create_dir_all(&home).unwrap();
    home
}

#[test]
fn a_test_opens_the_engine_as_it_said_on_its_own_thread_only() {
    let said = home();
    open_with(marked);
    drop(open(&said).unwrap());
    let other = home();
    std::thread::scope(|scope| scope.spawn(|| drop(open(&other).unwrap())).join().unwrap());
    assert_eq!(MARKED_OPENS.load(Ordering::SeqCst), 1, "another thread opens as it said, not as this one did");
}

#[test]
#[should_panic(expected = "the daemon gives the runtime its admission gate")]
fn a_runtime_given_no_gate_says_so_instead_of_letting_everything_through() {
    let home = home();
    let runtime = open_engine_at(&home, &crate::domain_store::active_path(&home), &alondra_store_sqlite::Sqlite(&alondra_domain_rules::Rules)).unwrap();
    drop(runtime.gate());
}

#[test]
#[should_panic(expected = "the daemon gives the runtime the owner of its SDK hosts")]
fn a_runtime_given_no_owner_of_its_hosts_says_so_instead_of_running_none() {
    let home = home();
    let runtime = open_engine_at(&home, &crate::domain_store::active_path(&home), &alondra_store_sqlite::Sqlite(&alondra_domain_rules::Rules)).unwrap();
    drop(runtime.hosts());
}
