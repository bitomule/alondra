use alondra_foundation::MessageUuid;
use serde::{Deserialize, Serialize};

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct InputRecord {
    pub message_id: MessageUuid,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct SessionRecord {
    pub session_id: MessageUuid,
    /// The native session existed before this conversation (a `codex --bg` node's, handed to the
    /// SDK): every host of the conversation resumes it rather than starting it.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub continued: bool,
}

/// The native Codex session a conversation's metadata names: the one each of its hosts runs.
pub fn conversation_session(metadata: &[u8]) -> Option<String> {
    serde_json::from_slice::<SessionRecord>(metadata).ok().map(|record| record.session_id.as_str().to_owned())
}
