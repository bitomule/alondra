//! What a provider's API failure says about its account and its provider. Each provider's adapter
//! reads its own errors into these; the rest of Alondra decides on them alone. The provider
//! contract hands them over; they sit here so the domain, below the contract, can keep them.
use serde::{Deserialize, Serialize};

/// What a provider failure says about its account and its provider.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Verdict {
    /// The account's sign-in expired or was revoked.
    SessionLost,
    /// The provider fails for everyone: its work waits and goes on by itself.
    Outage,
    /// The provider refuses the account itself.
    Refused { reason: RefusalReason },
    /// The usage limit; it resets at `resets_at`, in Unix seconds, when the provider said.
    Usage { resets_at: Option<u64> },
    /// An authentication error that says neither: Alondra checks before believing it.
    Doubtful,
    /// The request or the task is wrong, not the account.
    RequestProblem { problem: RequestProblem },
    /// No failure of the provider: an answer cut at the output limit.
    None,
}
impl Default for Verdict {
    /// What an error nobody classified is taken as: an outage, which waits and goes on by itself.
    fn default() -> Self {
        Self::Outage
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RefusalReason {
    AccountOnHold,
    OrgNotAllowed,
    Billing,
    VerificationRequired,
    PermissionDenied,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RequestProblem {
    CloudCredentials,
    InvalidRequest,
    ModelNotFound,
    BadRequest,
    ContextWindowExceeded,
}
