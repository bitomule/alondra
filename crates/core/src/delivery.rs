//! Durable local handoff evidence. Handed is not proof of Claude execution.
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Channel {
    Terminal,
    Compact,
    HookStop,
    HookDecision,
    MenuDecision,
    NodeDiscard,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum State {
    Dispatching,
    Uncertain,
    Handed,
    Retryable,
    Discarded,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Attempt {
    pub id: u64,
    pub session_id: String,
    pub channel: Channel,
    pub message_ids: Vec<u64>,
    /// Terminal menu intent has no message payload and must never become inbox text.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub question_id: Option<u64>,
    pub state: State,
    pub version: u64,
    pub reason: Option<String>,
    pub created_at: i64,
    pub updated_at: i64,
    /// Explicit resolution provenance makes duplicate retry/discard requests idempotent.
    pub resolved_from_version: Option<u64>,
}
impl Attempt {
    pub fn blocks(&self) -> bool {
        matches!(self.state, State::Dispatching | State::Uncertain)
    }

    /// Nothing can reach or re-check a terminal that is gone, so this leaves David nothing to
    /// resolve. A node discard is about the node itself and keeps its own recovery.
    pub fn close_for_gone_node(&mut self) {
        if self.blocks() && self.channel != Channel::NodeDiscard {
            self.state = State::Discarded;
            self.reason = Some("node_gone".into());
        }
    }
}
