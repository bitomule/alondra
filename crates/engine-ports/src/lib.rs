//! The ports through which the pieces of the engine call each other without depending on each
//! other, the values they speak in, and the registry that holds one implementation of each. Each
//! piece that implements a port gives it to the registry in its `wire`; the root calls every
//! `wire` when it puts the daemon together, and a piece that needs a port asks the registry.
//! It also keeps the file and network helpers the engine and the server share: the approval lease,
//! uploads and stored files, the Claude folder, the cutover marker, reports and retention.
pub mod accounts;
pub mod admission;
pub mod agent_host;
pub mod agent_workers;
pub mod api;
pub mod approval_lease;
pub mod claude_history;
pub mod conversation_file;
pub mod disk;
pub mod domain_api;
pub mod domain_cutover;
pub mod extensions;
pub mod files_retention;
pub mod gate;
pub mod github;
pub mod hooks;
pub mod hosts;
pub mod job_state;
pub mod kept_images;
pub mod journal;
pub mod launch;
pub mod local_network;
pub mod machine;
pub mod node_close;
pub mod orchestrator;
pub mod outbox;
pub mod sign_in_returns;
pub mod plugins;
pub mod processes;
pub mod reports;
pub mod schedule_clock;
pub mod schedule_routes;
pub mod sdk;
pub mod spec_reading;
pub mod spec_worktrees;
pub mod stored_files;
pub mod uploads;
pub mod usage;
pub mod watcher;
pub mod worktrees;
mod registry;
pub mod runtime;

pub use extensions::extension;
pub use registry::Registry;
pub use runtime::{AsRuntime, Runtime};

#[cfg(test)]
#[path = "../../test_support.rs"]
mod test_support;
