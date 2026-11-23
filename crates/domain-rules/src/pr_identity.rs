//! Which pull request a record is, apart from where it lives. The provider's adapter reads the PR
//! and decides what its answer means; the domain only keeps an opaque key that never changes, an
//! opaque locator the adapter finds the PR with again, the current address and its aliases.
use crate::obligation_policy::Millis;
use alondra_foundation::PrId;
use serde::{Deserialize, Serialize};

/// The provider's stable name for one pull request: fixed once known, never compared by meaning.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ProviderPrKey(pub String);

/// What the adapter finds the pull request with when its address no longer leads to it. The
/// adapter may hand a new one on any read: the domain keeps the latest and gives it back.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ProviderLocator(pub String);

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum PrIdentity {
    /// Stored before identities were read, or registered and not read yet.
    #[default]
    Unverified,
    Verified { key: ProviderPrKey, locator: ProviderLocator },
    /// Alondra cannot tell which pull request this record is, so it never acts on it remotely.
    Unresolvable {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        key: Option<ProviderPrKey>,
        reason: UnresolvableReason,
        since: Millis,
    },
}

impl PrIdentity {
    pub fn is_unverified(&self) -> bool {
        matches!(self, Self::Unverified)
    }
    pub fn is_verified(&self) -> bool {
        matches!(self, Self::Verified { .. })
    }
    pub fn is_unresolvable(&self) -> bool {
        matches!(self, Self::Unresolvable { .. })
    }
    pub fn key(&self) -> Option<&ProviderPrKey> {
        match self {
            Self::Unverified => None,
            Self::Verified { key, .. } => Some(key),
            Self::Unresolvable { key, .. } => key.as_ref(),
        }
    }
    pub fn locator(&self) -> Option<&ProviderLocator> {
        match self {
            Self::Verified { locator, .. } => Some(locator),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UnresolvableReason {
    /// The address now leads to another pull request.
    AddressReused,
    /// The repository is gone or Alondra lost access to it.
    Gone,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum PrTracking {
    #[default]
    Tracked,
    /// Another record of the same pull request took everything this one had; kept as history.
    Superseded { by: PrId },
    /// The user removed it from Alondra without touching the pull request.
    Detached { at: Millis },
}

impl PrTracking {
    pub fn is_tracked(&self) -> bool {
        matches!(self, Self::Tracked)
    }
}

/// The definitive "not found" answers read since the last confirmation: how many counted, each at
/// least [`STRIKE_GAP`] after the one before, and when the last one counted was read.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PrMissing {
    pub strikes: u8,
    pub last: Millis,
}

/// Answers that count towards [`UnresolvableReason::Gone`].
pub const GONE_STRIKES: u8 = 3;
/// How far apart two answers must be read to count as two: a moved repository can answer "not
/// found" for a while right after the move.
pub const STRIKE_GAP: Millis = 60 * 60 * 1000;

/// How the adapter reached the pull request it answers about.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum PrRead {
    /// Through this address: the record's current one or one of its aliases.
    Address { repository: String },
    /// Through the record's locator, after a relocation was asked for.
    Locator,
}

/// What the adapter decided a read says about the record.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum PrAddressVerdict {
    /// The read reached this record's pull request, which now lives at `current_repository`.
    Confirmed { key: ProviderPrKey, locator: ProviderLocator, current_repository: String },
    /// The address leads to a different pull request.
    NamesAnotherPr,
    /// A definitive answer that nothing is there, or that Alondra has no access to it.
    NotFound,
    /// No answer: network, server error, rate limit, expired session or timeout.
    Unreachable,
}
