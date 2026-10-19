//! What follows a turn a Claude API error ended. Claude Code reports it with its `StopFailure`
//! hook, whose `error_type` says which error (code.claude.com/docs/en/hooks). By then Claude Code
//! has already retried what it could: an error that arrives after the response began, such as
//! "Connection lost mid-response", is never retried and needs a "continue"
//! (code.claude.com/docs/en/errors). The hook cannot make Claude go on, so Alondra sends the node
//! back to its task through the gate, a growing wait apart.
use crate::model::Millis;
use serde::{Deserialize, Serialize};

/// The waits before each of the times Alondra sends a node back by itself after an API error.
pub const API_RETRIES: [Millis; 3] = [30_000, 2 * 60_000, 10 * 60_000];
/// While the API looks down for every node, one of them probes it this often; no card is raised.
pub const OUTAGE_PROBE: Millis = 2 * 60_000;
/// Past its retries, with nothing proving the API works for other nodes, a node tries again this
/// often and still raises no card.
pub const QUIET_RETRY: Millis = 10 * 60_000;
/// A node the model kept answering this long after its last API error fails anew, not again.
pub const FRESH_TURN: Millis = 10 * 60_000;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ApiErrorKind {
    /// The API failed for a while: trying again later helps.
    Transient,
    /// The usage limit: it goes on when the limit resets.
    Usage,
    /// Trying again cannot help until the user fixes something (login, billing, the model).
    NeedsUser,
}

/// The kind of every `error_type` Claude Code's `StopFailure` hook reports; one it adds later is
/// taken as transient.
pub fn classify(error_type: &str) -> ApiErrorKind {
    match error_type {
        "rate_limit" => ApiErrorKind::Usage,
        "authentication_failed" | "oauth_org_not_allowed" | "account_on_hold" | "billing_error" | "cloud_credential_error"
        | "invalid_request" | "model_not_found" => ApiErrorKind::NeedsUser,
        _ => ApiErrorKind::Transient,
    }
}

/// A node stopped by an API error, waiting to be sent back to its task at `at`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApiRetry {
    pub error: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    /// How many turns in a row the error ended.
    pub attempt: u8,
    pub at: Millis,
    /// The API looked down for every node: it waits without counting toward a card.
    #[serde(default)]
    pub outage: bool,
}

/// What Alondra knows of the API when a node's turn fails.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ApiHealth {
    /// Another node got answers from the model since this one began failing.
    Proven,
    Unknown,
    /// Several nodes failed and none got an answer since.
    Down,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AfterApiFailure {
    Retry(ApiRetry),
    /// The usage limit: the node waits for it to reset.
    WaitUsage,
    /// Nothing Alondra does helps: a card asks the user.
    Ask,
}

/// `attempt` counts this failure among those in a row. A transient error is retried after each
/// wait of `API_RETRIES`; past them a card asks the user only when other nodes prove the API
/// works, else the node keeps waiting for the API to come back.
pub fn after_api_failure(error: &str, detail: Option<&str>, attempt: u8, health: ApiHealth, now: Millis) -> AfterApiFailure {
    let retry = |wait: Millis, outage: bool| AfterApiFailure::Retry(ApiRetry {
        error: error.to_owned(), detail: detail.map(str::to_owned), attempt, at: now + wait, outage,
    });
    match classify(error) {
        ApiErrorKind::Usage => AfterApiFailure::WaitUsage,
        ApiErrorKind::NeedsUser => AfterApiFailure::Ask,
        ApiErrorKind::Transient => {
            let scheduled = API_RETRIES.get(usize::from(attempt.max(1)) - 1).copied();
            match (health, scheduled) {
                (ApiHealth::Down, _) => retry(OUTAGE_PROBE, true),
                (_, Some(wait)) => retry(wait, false),
                (ApiHealth::Proven, None) => AfterApiFailure::Ask,
                (ApiHealth::Unknown, None) => retry(QUIET_RETRY, true),
            }
        }
    }
}

/// The Claude API error an SDK turn ended with, once Claude Code's own retries were spent: its
/// category in `SDKAssistantMessage.error`, the HTTP status of the result and, for a rejected
/// usage limit, when it resets in Unix seconds.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SdkApiFailure {
    pub error: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<u16>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resets_at: Option<u64>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SdkFailureKind {
    /// Claude Code's sign-in expired or was revoked: only the user signing in again helps.
    SignedOut,
    /// The usage limit: it goes on when the limit resets.
    Usage,
    /// The API failed past Claude Code's retries: it goes on when the API answers again.
    Outage,
    /// Something only the user can fix, and not by signing in: billing, the organization, the model.
    NeedsUser,
}

/// The kind of an SDK turn's API error. A 403 is labelled `authentication_failed` too, but signing
/// in again does not help with it. An answer cut at the output limit is no failure of the API: the
/// turn ends like any other. A category Claude Code adds later is taken as an outage.
pub fn classify_sdk(error: &str, status: Option<u16>) -> Option<SdkFailureKind> {
    Some(match error {
        "max_output_tokens" => return None,
        "authentication_failed" if status == Some(403) => SdkFailureKind::NeedsUser,
        "authentication_failed" => SdkFailureKind::SignedOut,
        "rate_limit" => SdkFailureKind::Usage,
        "oauth_org_not_allowed" | "account_on_hold" | "verification_required" | "billing_error" | "cloud_credential_error"
        | "invalid_request" | "model_not_found" => SdkFailureKind::NeedsUser,
        _ => SdkFailureKind::Outage,
    })
}

/// The categories `api_refusal` names.
pub const REFUSALS: [&str; 8] = [
    "authentication_failed", "oauth_org_not_allowed", "account_on_hold", "verification_required", "billing_error",
    "cloud_credential_error", "model_not_found", "invalid_request",
];

/// What a refused request needs, for a card that names it.
pub fn api_refusal(error: &str) -> &'static str {
    match error {
        "authentication_failed" => "the Claude account refuses this request",
        "oauth_org_not_allowed" => "the Claude organization does not allow Claude Code",
        "account_on_hold" => "the Claude account is on hold",
        "verification_required" => "the Claude account needs verifying",
        "billing_error" => "the Claude account has a billing problem",
        "cloud_credential_error" => "Claude Code's cloud credentials fail",
        "model_not_found" => "Claude Code's model is not available",
        "invalid_request" => "the Claude API refuses Claude Code's request",
        _ => "the Claude API refuses its request",
    }
}
