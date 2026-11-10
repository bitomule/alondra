use alondra_core::{Millis, accounts::AccountKey, domain::InteractionId, model_selection::ModelSelection};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BatchSelection {
    pub account: AccountKey,
    #[serde(default)]
    pub account_status: Option<alondra_core::accounts::AccountStatus>,
    pub model: ModelSelection,
    pub policy_revision: String,
    pub reason: String,
    pub rejected_candidates: Vec<String>,
}

/// A node's turns an API error ended in a row, and the card raised once Alondra gave up.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Streak {
    pub failures: u8,
    pub first_at: Millis,
    pub last_at: Millis,
    pub card: Option<InteractionId>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RuleState {
    /// Chosen; Claude Code has not been seen writing it yet.
    Pending,
    Active,
    /// Claude Code never wrote it: it is not listed, and its card says so.
    Missing,
    Removed,
}
impl RuleState {
    pub fn as_str(self) -> &'static str {
        match self {
            RuleState::Pending => "pending",
            RuleState::Active => "active",
            RuleState::Missing => "missing",
            RuleState::Removed => "removed",
        }
    }
    pub fn parse(text: &str) -> Self {
        match text {
            "active" => RuleState::Active,
            "missing" => RuleState::Missing,
            "removed" => RuleState::Removed,
            _ => RuleState::Pending,
        }
    }
}

/// A rule «allow always» added on this Mac.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AllowedRule {
    pub id: i64,
    /// The main checkout's folder.
    pub repo: String,
    pub settings: String,
    /// As Claude Code matches it: `make build *`.
    pub rule: String,
    pub added_at: i64,
    pub task_id: Option<String>,
    pub interaction_id: String,
    pub machine: String,
    pub state: RuleState,
}
