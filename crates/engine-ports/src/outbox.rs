//! A message waiting in the outbox for its recipient.
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct OutboxMessage {
    pub id: i64,
    pub recipient_kind: String,
    pub agent_id: Option<String>,
    pub task_id: Option<String>,
    pub interaction_id: Option<String>,
    pub message: String,
    pub state: String,
}

pub const ALONDRA: &str = "Alondra";

/// Who sent a message to a node and when, ahead of its text: the node goes by the newest
/// instruction of whoever directs it.
pub fn stamped(from: &str, at: alondra_core::Millis, text: &str) -> String {
    format!("[From {from} · {}] {text}", crate::usage::clock_text(at, alondra_core::now_millis(), false))
}

/// The author a node reads for a message that a command of this kind of actor wrote.
pub fn author_of(actor_kind: &str) -> String {
    match actor_kind {
        "user" => alondra_core::config::user_name(),
        "orchestrator" => "the orchestrator".into(),
        _ => ALONDRA.into(),
    }
}

/// What the Agent of a conflicting PR is told, whether the user asked or Alondra noticed.
pub fn conflicts_text(name: &str, pr: &alondra_domain_model::PullRequest, update: alondra_domain_model::BranchUpdate) -> String {
    format!(
        "{name} asks you to resolve the conflicts of {url} with {base}: {how}. When you finish, present the PR again with \
         present_result and its new head. If you cannot resolve them, ask {name}.",
        url = format!("https://{}/{}/pull/{}", pr.host, pr.repository, pr.number), base = pr.base, how = update.instruction(&pr.base)
    )
}
