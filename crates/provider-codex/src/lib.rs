//! Durable Codex execution through the application-owned transaction and retained host.
mod adapter;
mod agent;
mod batch;
mod context;
mod ended;
mod normalizer;
mod records;
mod session_worktrees;
mod wire;

#[cfg(test)]
#[path = "../../test_support.rs"]
mod test_support;
#[cfg(test)]
mod conformance_tests;
#[cfg(all(test, any(feature = "host_fixture", feature = "host_model")))]
mod host_cases_tests;

pub use adapter::{CodexWorker, Found, answered_by_event, answered_control, native_exited};
pub use batch::{BatchHost, HostEvent, PendingHostEvent, McpServer, OrchestratorProfile};
pub use context::{ContextEvent, SessionFailure, context_event, native_message, account_refusal, session_failure};
pub use ended::{EndedJournal, Retained};
pub use agent::{wire, Codex};
pub use normalizer::{background_left,live_scope,blocked_policy,LiveScope};
pub use records::conversation_session;
pub use session_worktrees::NoSessionWorktrees;
pub use wire::HostOwner;
pub use alondra_codex_protocol::{Execution, OrchestratorExecution, OrchestratorLane};
#[cfg(test)]
use alondra_codex_protocol as archive_protocol;
#[cfg(test)]
#[path = "../../provider-claude/src/archive_tests.rs"]
mod archive_tests;
