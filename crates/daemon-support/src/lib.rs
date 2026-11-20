#[cfg(test)]
#[path = "../../test_support.rs"]
mod test_support;

#[macro_use]
extern crate alondra_foundation;

pub use alondra_foundation::{exit, locks, log, runtime_mutex, save};
pub mod approval_lease;
pub mod auth;
pub mod blocking_work;
pub mod card_language;
pub mod classify;
pub mod device_usage;
pub mod domain_authority;
pub mod domain_content;
pub mod domain_cutover;
pub mod files_retention;
pub mod hook_text;
pub mod job_state;
pub mod local_network;
pub mod node_cost;
pub mod open_files;
pub mod pr_heads;
pub mod pr_ids;
pub mod push;
pub mod relaunch;
pub mod reports;
pub mod result_files;
pub mod revoked_devices;
#[cfg(target_os = "macos")]
pub mod schedule_clock;
pub mod schedules;
pub mod scoped_reads;
pub mod service_files;
pub mod start_guard;
pub mod updates;
pub mod uploads;
pub mod watcher;
pub mod workflow_state;
