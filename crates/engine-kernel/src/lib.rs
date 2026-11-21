//! The engine's kernel: the domain store, its migration, the committed snapshot readers take, the
//! commit and its observers, capabilities, sessions, retention, the settings projection and the
//! capture of metrics. It names none of the subsystems above it: each keeps its state as an
//! extension of the runtime and hears of commits through the observers the engine registers.

#[cfg(test)]
#[path = "../../test_support.rs"]
pub mod test_support;

#[macro_use]
extern crate alondra_foundation;

use alondra_daemon_support::{
    approval_lease,
    auth,
    blocking_work,
    domain_authority,
    domain_content,
    domain_cutover,
    files_retention,
    hook_text,
    locks,
    runtime_mutex,
    save,
    service_files,
};

pub use alondra_daemon_engine_model::options;
pub use alondra_engine_ports::{Runtime, extension};

pub mod accounts_file;
pub mod bg_sessions;
pub mod domain_runtime;
pub mod domain_sessions;
pub mod domain_store;
pub mod launches;
pub mod metrics_capture;
pub mod retention;
pub mod sdk_backend;
pub mod settings_projection;
pub mod start_failures;
pub mod untaken;

/// The journal the tests of the engine open their runtimes on: a test of a crate above the kernel
/// sets it once, before its first runtime.
pub static TEST_JOURNAL: std::sync::OnceLock<&'static dyn alondra_engine_ports::journal::JournalFiles> = std::sync::OnceLock::new();

pub fn test_journal() -> &'static dyn alondra_engine_ports::journal::JournalFiles {
    #[cfg(test)]
    TEST_JOURNAL.get_or_init(|| &alondra_store_sqlite::Sqlite);
    *TEST_JOURNAL.get().expect("the test sets alondra_engine_kernel::TEST_JOURNAL to the journal its runtimes open on")
}
