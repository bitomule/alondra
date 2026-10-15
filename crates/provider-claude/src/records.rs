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
}
