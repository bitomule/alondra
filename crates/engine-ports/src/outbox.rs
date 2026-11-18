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
