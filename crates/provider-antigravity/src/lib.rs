//! Durable Antigravity execution through the application-owned transaction and retained host:
//! the daemon side of `alondra-antigravity-host`, which drives `agy acp`.
mod adapter;
mod context;
mod decisions;
mod effort;
mod ended;
mod normalizer;
mod records;
mod session_worktrees;
mod wire;

#[cfg(test)]
mod conformance_tests;

pub use adapter::{AntigravityConfiguration, AntigravityWorker, Found, NativeLiveness, WorkerProfile, native_exited};
pub use context::{SessionFailure, account_refusal, failure_message, native_message, session_failure};
pub use decisions::{chosen_option, is_question};
pub use effort::{LevelRefused, effort_of, level_for};
pub use ended::{EndedJournal, Retained};
pub use normalizer::blocked_policy;
pub use records::conversation_session;
pub use session_worktrees::NoSessionWorktrees;
pub use wire::HostOwner;
pub use alondra_antigravity_protocol::Execution;
#[cfg(test)]
use alondra_antigravity_protocol as archive_protocol;
#[cfg(test)]
#[path = "../../provider-claude/src/archive_tests.rs"]
mod archive_tests;
