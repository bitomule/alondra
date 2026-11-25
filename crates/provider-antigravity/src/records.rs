use alondra_foundation::MessageUuid;
use serde::{Deserialize, Serialize};

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct InputRecord {
    pub message_id: MessageUuid,
}
/// The conversation's identity on Alondra's side. The ACP session it runs in is the host's to keep
/// (`native-sessions/<session_id>` beside its state folder), since `agy` names it only once the
/// session opens.
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct SessionRecord {
    pub session_id: MessageUuid,
}

/// The conversation a conversation's metadata names: the one each of its hosts runs.
pub fn conversation_session(metadata: &[u8]) -> Option<String> {
    serde_json::from_slice::<SessionRecord>(metadata).ok().map(|record| record.session_id.as_str().to_owned())
}
