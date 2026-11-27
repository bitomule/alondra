//! Shared launch data and helpers used by the daemon's engine.
#[cfg(test)]
#[path = "../../test_support.rs"]
mod test_support;

use alondra_foundation::save;

mod account;
mod account_identity;
mod launch;
mod state;

pub mod claude_trust;
pub mod faults;
pub use alondra_engine_ports::issues;
pub use alondra_engine_ports::options;
pub mod reply;
pub mod retention;
pub use alondra_engine_ports::settings_file;

pub use account::{account_of, local_day};
pub use account_identity::NativeAccountIdentity;
pub use launch::{Failed, LaunchBody, LaunchOptions, LaunchOptionsFile, LaunchPlan, WrittenFailure, retain_selection_options};
pub use state::{AllowedRule, BatchSelection, RuleState, Streak};
