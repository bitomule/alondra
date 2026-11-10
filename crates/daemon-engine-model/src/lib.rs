//! Shared launch data and helpers used by the daemon's engine.
#[cfg(test)]
#[path = "../../test_support.rs"]
mod test_support;

use alondra_daemon_support::{disk, locks, prs, save};

mod account;
mod account_identity;
mod launch;
mod state;

#[cfg(any(test, feature = "test-hooks"))]
pub mod faults;
pub mod issues;
pub mod options;
pub mod reply;
pub mod retention;
pub mod settings_file;

pub use account::{account_of, local_day};
pub use account_identity::NativeAccountIdentity;
pub use launch::{Failed, LaunchBody, LaunchOptions, LaunchPlan, retain_selection_options};
pub use state::{AllowedRule, BatchSelection, RuleState, Streak};
