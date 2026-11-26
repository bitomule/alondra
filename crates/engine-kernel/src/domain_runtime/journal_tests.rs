//! The runtime reaches its journal only through the port it is given.
use super::*;
use crate::domain_store::decisions::SdkDecisions;
use std::os::unix::fs::DirBuilderExt;
use std::sync::atomic::{AtomicUsize, Ordering};

/// The SQLite journal, counting what the runtime opens through it.
#[derive(Default)]
struct Counted {
    writers: AtomicUsize,
    readers: AtomicUsize,
}

impl JournalFiles for Counted {
    fn open(&self, database: &Path, decisions: Option<Arc<dyn SdkDecisions>>) -> Result<Box<dyn Journal>> {
        self.writers.fetch_add(1, Ordering::SeqCst);
        alondra_store_sqlite::Sqlite(&alondra_domain_rules::Rules).open(database, decisions)
    }
    fn open_readonly(&self, database: &Path) -> Result<Box<dyn Journal>> {
        self.readers.fetch_add(1, Ordering::SeqCst);
        alondra_store_sqlite::Sqlite(&alondra_domain_rules::Rules).open_readonly(database)
    }
    fn migrate_and_fence(&self, home: &Path) -> Result<(bool, Option<Box<dyn std::any::Any + Send>>)> {
        alondra_store_sqlite::Sqlite(&alondra_domain_rules::Rules).migrate_and_fence(home)
    }
    fn validate_preflight_source(&self, home: &Path) -> Result<()> {
        alondra_store_sqlite::Sqlite(&alondra_domain_rules::Rules).validate_preflight_source(home)
    }
    fn preflight_staging_files(&self, home: &Path) -> Result<Vec<PathBuf>> {
        alondra_store_sqlite::Sqlite(&alondra_domain_rules::Rules).preflight_staging_files(home)
    }
    fn migration_marker(&self) -> &'static str {
        alondra_store_sqlite::Sqlite(&alondra_domain_rules::Rules).migration_marker()
    }
    fn remove_recovery_copies(&self, home: &Path, now: std::time::SystemTime) -> Result<()> {
        alondra_store_sqlite::Sqlite(&alondra_domain_rules::Rules).remove_recovery_copies(home, now)
    }
}

#[test]
fn the_runtime_opens_its_writer_and_its_reader_through_the_journal_it_is_given() {
    let home = crate::test_support::Directory::new();
    std::fs::DirBuilder::new().mode(0o700).create(&home).unwrap();
    let journal = Counted::default();
    let runtime = DomainRuntime::open_observed(&home, &crate::domain_store::active_path(&home), &journal, None, vec![]).unwrap();
    assert_eq!((journal.writers.load(Ordering::SeqCst), journal.readers.load(Ordering::SeqCst)), (1, 1));
    let user = Actor::User { user_id: "device:phone".into() };
    let state = runtime.state().unwrap();
    let envelope = CommandEnvelope {
        protocol_version: PROTOCOL_VERSION, data_epoch: state.data_epoch.clone(), command_id: "request".into(), expected_revision: state.revision,
        command: Command::CreateRequest { request_id: "r-1".into(), body: "Through the port".into(), auto_merge: false, maintenance: false, message: false },
    };
    runtime.execute(&user, &envelope, None, || Ok(())).unwrap();
    assert!(runtime.live_read().request_times().unwrap().contains_key(&RequestId::from("r-1")), "the reader it opened reads the commit");
}
