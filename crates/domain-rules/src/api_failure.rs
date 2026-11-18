//! What follows a turn a provider API error ended. Each provider's adapter reads its own errors
//! and hands over a `Verdict`; nothing here reads a provider's error text or category. Claude Code
//! reports a failure with its `StopFailure` hook too (code.claude.com/docs/en/hooks). By then Claude Code
//! has already retried what it could: an error that arrives after the response began, such as
//! "Connection lost mid-response", is never retried and needs a "continue"
//! (code.claude.com/docs/en/errors). The hook cannot make Claude go on, so Alondra sends the node
//! back to its task through the gate, a growing wait apart.
use alondra_core::model::Millis;
pub use alondra_foundation::{RefusalReason, RequestProblem, Verdict as ProviderVerdict};
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

/// `attempt` counts this failure among those in a row. A failure of the API is retried after
/// each wait of `API_RETRIES`; past them a card asks the user only when other nodes prove the API
/// works, else the node keeps waiting for the API to come back.
pub fn after_api_failure(error: &str, detail: Option<&str>, verdict: &ProviderVerdict, attempt: u8, health: ApiHealth, now: Millis) -> AfterApiFailure {
    let retry = |wait: Millis, outage: bool| AfterApiFailure::Retry(ApiRetry {
        error: error.to_owned(), detail: detail.map(str::to_owned), attempt, at: now + wait, outage,
    });
    match verdict {
        ProviderVerdict::Usage { .. } => AfterApiFailure::WaitUsage,
        ProviderVerdict::SessionLost | ProviderVerdict::Refused { .. } | ProviderVerdict::RequestProblem { .. } => AfterApiFailure::Ask,
        ProviderVerdict::Outage | ProviderVerdict::Doubtful | ProviderVerdict::None => {
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

/// The provider API error an SDK turn ended with, once the provider's own retries were spent:
/// its native category and status for the log, when a rejected usage limit resets in Unix
/// seconds, and the verdict the provider's adapter read in it. Alondra decides on the verdict
/// alone; a failure recorded before verdicts existed is taken as an outage.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SdkApiFailure {
    pub error: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<u16>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resets_at: Option<u64>,
    #[serde(default)]
    pub verdict: ProviderVerdict,
    /// When the daemon observed it, for the provider's health.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub at: Option<Millis>,
}

impl SdkApiFailure {
    pub fn new(error: impl Into<String>, status: Option<u16>, verdict: ProviderVerdict) -> Self {
        let resets_at = match verdict { ProviderVerdict::Usage { resets_at } => resets_at, _ => None };
        Self { error: error.into(), status, resets_at, verdict, at: None }
    }
    pub fn at(mut self, at: Millis) -> Self {
        self.at = Some(at);
        self
    }
}

/// Whether a provider answers on this Mac. Down holds the provider's work silently: no card, no
/// push; the outage probe tries it again every `OUTAGE_PROBE`, and the first answer brings it back.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProviderHealth {
    pub provider: alondra_core::accounts::Provider,
    pub state: HealthState,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum HealthState {
    Healthy,
    Down { since: Millis, #[serde(default, skip_serializing_if = "Option::is_none")] last_probe: Option<Millis>, next_probe: Millis },
}

impl HealthState {
    /// Down since `at`, or since it already was, with its next probe `OUTAGE_PROBE` after `at`.
    pub fn down(previous: Option<&HealthState>, at: Millis) -> Self {
        let since = match previous { Some(Self::Down { since, .. }) => *since, _ => at };
        let last_probe = matches!(previous, Some(Self::Down { .. })).then_some(at);
        Self::Down { since, last_probe, next_probe: at + OUTAGE_PROBE }
    }
    pub fn down_since(&self) -> Option<Millis> {
        match self { Self::Down { since, .. } => Some(*since), Self::Healthy => None }
    }
    /// Whether new work stays off the provider at `now`: it is down and its probe is not due.
    pub fn holds_new_work(&self, now: Millis) -> bool {
        matches!(self, Self::Down { next_probe, .. } if now < *next_probe)
    }
}
