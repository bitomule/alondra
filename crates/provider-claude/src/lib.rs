#![forbid(unsafe_code)]
//! Private Claude worker transport behind committed application dispatch authority.
//!
//! Terminal observations still require the real Task/card application transaction;
//! the controller deliberately leaves those observations unacknowledged.
mod adapter;
mod batch;
mod normalizer;
mod records;
mod wire;

pub use adapter::{ClaudeConfiguration, ClaudeWorker, WorkerProfile};
pub use batch::{BatchHost, HostEvent, McpServer, OrchestratorProfile};
pub use alondra_agent_protocol::{Execution, OrchestratorExecution, OrchestratorLane};
pub use wire::HostOwner;

#[cfg(test)]
mod controller_tests;
