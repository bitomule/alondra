#![forbid(unsafe_code)]
//! Private Claude worker transport behind committed application dispatch authority.
//!
//! Terminal observations still require the real Task/card application transaction;
//! the controller deliberately leaves those observations unacknowledged.
mod adapter;
mod batch;
mod context;
mod elicitation;
mod mcp;
mod ended;
mod normalizer;
mod records;
mod wire;

pub use adapter::{
    ClaudeConfiguration, ClaudeWorker, Found, WorkerProfile, answered_by_event, answered_control, native_exited,
};
pub use context::{ContextEvent, context_event, sdk_message};
pub use mcp::{mcp_traits, remove_arguments, settings_file, still_configured};
pub use elicitation::{elicitation_content, elicitation_fields};
pub use ended::{EndedJournal, Retained};
pub use batch::{BatchHost, PendingHostEvent, HostEvent, McpServer, OrchestratorProfile};
pub use alondra_agent_protocol::{Execution, OrchestratorExecution, OrchestratorLane};
pub use wire::HostOwner;
pub use records::conversation_session;
pub use normalizer::background_left;

#[cfg(test)]
mod controller_tests;
