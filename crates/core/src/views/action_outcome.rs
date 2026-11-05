//! What the Mac answers when it cannot hand over an action's reply: the action is still running
//! (`in_progress`, asked again later gets the real reply) or nobody can tell whether it happened
//! (`outcome_unknown`, with the typed reason why).

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

pub const IN_PROGRESS: &str = "in_progress";
pub const OUTCOME_UNKNOWN: &str = "outcome_unknown";

/// Why the Mac cannot tell whether an action happened. A reason from a newer daemon reads as
/// `Other`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UnknownReason {
    /// Alondra stopped while the action ran.
    Interrupted,
    /// The action ran but its reply could not be kept.
    Unrecorded,
    /// The same action id came back with different content.
    Changed,
    /// The reply was kept too long ago and is gone.
    Expired,
    /// Alondra's data on the Mac was reset or restored since the action was sent.
    DataReset,
    /// The record of actions could not be read.
    Unavailable,
    /// The Mac has no record of the action.
    NotFound,
    /// The device's approval changed while the action ran.
    ApprovalChanged,
    /// The request was malformed.
    Invalid,
    #[serde(other)]
    Other,
}

impl UnknownReason {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Interrupted => "interrupted",
            Self::Unrecorded => "unrecorded",
            Self::Changed => "changed",
            Self::Expired => "expired",
            Self::DataReset => "data_reset",
            Self::Unavailable => "unavailable",
            Self::NotFound => "not_found",
            Self::ApprovalChanged => "approval_changed",
            Self::Invalid => "invalid",
            Self::Other => "other",
        }
    }
}

pub fn unknown_body(reason: UnknownReason) -> Value {
    json!({"error": OUTCOME_UNKNOWN, "kind": OUTCOME_UNKNOWN, "reason": reason})
}

pub fn in_progress_body() -> Value {
    json!({"error": IN_PROGRESS, "kind": IN_PROGRESS})
}

/// What a body that is not an action's reply says about it, decided on its `kind` and `reason`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Pending {
    InProgress,
    Unknown(UnknownReason),
}

impl Pending {
    pub fn of(body: &Value) -> Option<Self> {
        match body.get("kind").and_then(Value::as_str)? {
            IN_PROGRESS => Some(Self::InProgress),
            OUTCOME_UNKNOWN => Some(Self::Unknown(
                body.get("reason")
                    .and_then(|reason| serde_json::from_value(reason.clone()).ok())
                    .unwrap_or(UnknownReason::Other),
            )),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bodies_read_back_as_their_type_and_unknown_reasons_from_newer_daemons_read_as_other() {
        assert_eq!(Pending::of(&in_progress_body()), Some(Pending::InProgress));
        assert_eq!(
            Pending::of(&unknown_body(UnknownReason::Interrupted)),
            Some(Pending::Unknown(UnknownReason::Interrupted))
        );
        assert_eq!(unknown_body(UnknownReason::DataReset)["reason"], "data_reset");
        assert_eq!(
            Pending::of(&json!({"kind": "outcome_unknown", "reason": "from_the_future"})),
            Some(Pending::Unknown(UnknownReason::Other))
        );
        assert_eq!(
            Pending::of(&json!({"error": "outcome_unknown", "kind": "outcome_unknown"})),
            Some(Pending::Unknown(UnknownReason::Other))
        );
        assert_eq!(Pending::of(&json!({"error": "busy"})), None);
        for reason in [
            UnknownReason::Interrupted, UnknownReason::Unrecorded, UnknownReason::Changed,
            UnknownReason::Expired, UnknownReason::DataReset, UnknownReason::Unavailable,
            UnknownReason::NotFound, UnknownReason::ApprovalChanged, UnknownReason::Invalid,
        ] {
            assert_eq!(serde_json::to_value(reason).unwrap(), reason.as_str());
        }
    }
}
