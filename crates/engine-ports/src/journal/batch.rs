//! What the journal keeps of the orchestrator's batches: their effects, reports and the ones left unfinished.
use super::DeliveryEvidence;
use alondra_domain_model::DomainError;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BatchReceiptRef {
    pub actor: String,
    pub command_id: String,
    pub data_epoch: String,
    pub digest: Vec<u8>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BatchEffectRef {
    pub run: String,
    pub attempt: u64,
    pub call: String,
    pub command_id: String,
    pub receipt_ref: BatchReceiptRef,
    pub kind: String,
    pub revision: u64,
    pub changed: bool,
}

pub struct BatchCommand<'a> {
    pub run: &'a str,
    pub attempt: u64,
    pub call: &'a str,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct BatchReport {
    pub kind: String,
    pub input_consumed: bool,
    pub turn_completed: bool,
    pub native_turn: Option<String>,
    pub failure: Option<String>,
    pub public_result: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stop: Option<alondra_domain_model::orchestrator_batch::BatchStop>,
}

pub struct UnfinishedBatch {
    pub run: String,
    pub ended: bool,
    pub decision: Option<String>,
    pub attempts: Vec<DeliveryEvidence>,
}

/// What only the orchestrator's tools answer, before they change anything.
pub const TOOL_REFUSALS: &[&str] = &[
    "node_paused", "task_closed", "model_is_binding", "task_moving", "usage_exhausted", "disk_low",
    "account_unavailable", "model_unavailable", "unsupported_capability", "workspace_untrusted", "main_checkout",
    "session_unreachable", "native_session_required", "selection_transition_required", "turn_task_limit", "provider_unavailable",
];

/// The domain's rules refuse before they commit, so such a call is settled and nothing is left to
/// reconcile, except `delivery_uncertain`. Any code not known here leaves its effects unknown.
pub fn refused_before_any_effect(code: Option<&str>) -> bool {
    code.is_some_and(|code| code != "delivery_uncertain" && (alondra_domain_model::INTERNAL_CODES.contains(&code) || TOOL_REFUSALS.contains(&code)))
}

fn error_code(error: Option<&str>) -> Option<String> {
    error.and_then(|error| serde_json::from_str::<DomainError>(error).ok()).map(|error| error.code)
}

/// An operation that is neither done nor refused before it changed anything.
pub fn is_unconfirmed(operation: &(String, String, String, Option<String>, Option<String>)) -> bool {
    match operation.2.as_str() {
        "succeeded" => false,
        "uncertain" => !refused_before_any_effect(error_code(operation.4.as_deref()).as_deref()),
        _ => true,
    }
}
