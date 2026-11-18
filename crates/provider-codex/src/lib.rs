//! Durable Codex execution through the application-owned transaction and retained host.
mod adapter;
mod batch;
mod context;
mod elicitation;
mod failure;
mod ended;
mod normalizer;
mod records;
mod wire;

#[cfg(test)]
#[path = "../../test_support.rs"]
mod test_support;
#[cfg(test)]
mod conformance_tests;

pub use adapter::{CodexConfiguration, CodexWorker, Found, NativeLiveness, WorkerProfile, answered_by_event, answered_control, native_exited};
pub use batch::{BatchHost, HostEvent, PendingHostEvent, McpServer, OrchestratorProfile};
pub use context::{ContextEvent, SessionFailure, context_event, native_message, account_refusal, session_failure};
pub use ended::{EndedJournal, Retained};
pub use failure::{classify_failure, provider_failure};
pub use normalizer::{background_left,live_scope,blocked_policy,LiveScope};
pub use records::conversation_session;
pub use wire::HostOwner;
pub use alondra_codex_protocol::{Execution, OrchestratorExecution, OrchestratorLane};
#[cfg(test)]
use alondra_codex_protocol as archive_protocol;
#[cfg(test)]
#[path = "../../provider-claude/src/archive_tests.rs"]
mod archive_tests;
#[cfg(test)]
mod failure_tests;
