//! The engine's kernel: the domain store, its migration, the committed snapshot readers take, the
//! commit and its observers, capabilities, sessions, retention, the settings projection and the
//! capture of metrics. It names none of the subsystems above it: each keeps its state as an
//! extension of the runtime and hears of commits through the observers the engine registers.
#![cfg_attr(all(feature = "test-hooks", not(test)), allow(dead_code, unused_imports))]

#[cfg(any(test, feature = "test-hooks"))]
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
pub use alondra_engine_ports::extension;

pub mod domain_database_migration;
pub mod domain_runtime;
pub mod domain_sessions;
pub mod domain_store;
pub mod metrics_capture;
pub mod retention;
pub mod sdk_backend;
pub mod settings_projection;
