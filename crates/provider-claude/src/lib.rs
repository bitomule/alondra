#![forbid(unsafe_code)]
//! Private Claude worker transport behind committed application dispatch authority.
//!
//! Terminal observations still require the real Task/card application transaction;
//! the controller deliberately leaves those observations unacknowledged.
mod adapter;
mod batch;
mod elicitation;
mod ended;
mod normalizer;
mod records;
mod wire;

pub use adapter::{
    ClaudeConfiguration, ClaudeWorker, Found, WorkerProfile, answered_by_event, answered_control,
};
pub use elicitation::{elicitation_content, elicitation_fields};
pub use ended::{EndedJournal, Retained};
pub use batch::{BatchHost, HostEvent, McpServer, OrchestratorProfile};
pub use alondra_agent_protocol::{Execution, OrchestratorExecution, OrchestratorLane};
pub use wire::HostOwner;

#[cfg(test)]
mod controller_tests;
