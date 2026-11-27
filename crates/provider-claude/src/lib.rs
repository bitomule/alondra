#![forbid(unsafe_code)]
//! Private Claude worker transport behind committed application dispatch authority.
//!
//! Terminal observations still require the real Task/card application transaction;
//! the controller deliberately leaves those observations unacknowledged.
mod adapter;
mod agent;
mod batch;
mod context;
mod mcp;
mod ended;
mod normalizer;
mod records;
mod session_worktrees;
mod wire;

pub use adapter::{
    ClaudeWorker, Found, answered_by_event, answered_control, native_exited,
};
pub use context::{Cached, ContextEvent, context_event, sdk_message};
pub use mcp::{mcp_traits, remove_arguments, settings_file, still_configured};
pub use ended::{EndedJournal, Retained};
pub use agent::{wire, Claude};
pub use batch::{BatchHost, Batches, HostEvent, McpServer, OrchestratorProfile, PendingHostEvent};
pub use alondra_agent_protocol::{Execution, OrchestratorExecution, OrchestratorLane};
pub use wire::HostOwner;
pub use records::conversation_session;
pub use session_worktrees::SubagentNotes;
pub use normalizer::background_left;

#[cfg(test)]
mod controller_tests;
#[cfg(test)]
mod conformance_tests;
#[cfg(test)]
use alondra_agent_protocol as archive_protocol;
#[cfg(test)]
mod archive_tests;
