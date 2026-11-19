//! How the card of a decision Claude asks for is built, which the journal does not know.
use alondra_domain_rules::{DecisionPrompt, Hold, McpServerOrigin};
use alondra_provider_contract::DecisionKind;
use serde_json::Value;
use std::path::Path;

/// What the domain is told of a decision Claude asks for through a task's SDK execution: the call
/// it is about and the card the user would answer, built exactly as a held hook's card.
pub struct SdkDecision {
    pub hold: Hold,
    pub tool: Option<String>,
    pub input: Value,
    pub mcp_server: Option<McpServerOrigin>,
    pub prompt: DecisionPrompt,
}

/// How the card of a decision Claude asks for is built, which the store does not know.
pub trait SdkDecisions: Send + Sync {
    fn decide(&self, task_title: &str, kind: DecisionKind, payload: &Value, worktree: Option<&str>, worktrees: &[String], home: &Path) -> SdkDecision;
}
