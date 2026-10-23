//! Claude adapter private daemon ↔ SDK host wire contract.
//! This is not the provider-neutral execution port and grants no domain authority.
mod codec;
mod correlation;
mod json;
mod ownership;
mod schema;

pub use codec::{FramedReader, FramedWriter, ReceivedFrame, command_digest, encode};
pub use correlation::{CommandRegistry, CommandStatus, EventSequence, SequenceStatus};
pub use schema::*;
pub use ownership::{host_proof, owner_proof, verify_host_proof, verify_owner_proof};

/// A frame a host of the previous release would no longer read the same way (a changed or removed
/// field, a changed meaning) takes a new version, and the daemon keeps speaking the old one to the
/// hosts it adopts. A new action is no such change: it names the capability that carries it.
pub const PROTOCOL_VERSION: u16 = 1;
pub const SDK_VERSION: &str = "0.3.300";
/// The SDK releases whose messages this daemon reads: a live host of another release that runs
/// one of them is adopted, and one that runs another is replaced.
pub const ADOPTABLE_SDK_VERSIONS: &[&str] = &[SDK_VERSION];
/// The oldest release whose live host this daemon adopts. A release that fixes a host's bug no
/// node should keep running until its next rest raises it to itself.
pub const OLDEST_ADOPTABLE_HOST_RELEASE: &str = "0.3.5";

/// Why this daemon does not adopt a live host of `host_release` that runs SDK `sdk_version`, or
/// `None` when it does. The protocol versions and capabilities the host announces are checked when
/// the owner attaches.
pub fn refuses_adoption(host_release: &str, sdk_version: &str) -> Option<String> {
    fn release(text: &str) -> Option<(u64, u64, u64)> {
        let mut parts = text.split('.').map(|part| part.parse::<u64>().ok());
        let release = (parts.next()??, parts.next()??, parts.next()??);
        parts.next().is_none().then_some(release)
    }
    if !ADOPTABLE_SDK_VERSIONS.contains(&sdk_version) {
        return Some(format!("it runs Agent SDK {sdk_version}, which this release does not read"));
    }
    match (release(host_release), release(OLDEST_ADOPTABLE_HOST_RELEASE)) {
        (Some(host), Some(oldest)) if host >= oldest => None,
        _ => Some(format!("it runs release {host_release}, and this release adopts hosts from {OLDEST_ADOPTABLE_HOST_RELEASE} on")),
    }
}
pub const MAX_FRAME_BYTES: usize = 1_048_576;
pub const MAX_FRAME_DEPTH: usize = 127;
pub const MAX_TRACKED_COMMANDS: usize = 256;
pub const MAX_SAFE_INTEGER: u64 = 9_007_199_254_740_991;

#[derive(Debug)]
pub enum Error {
    Io(std::io::Error),
    Json(serde_json::Error),
    Poisoned,
    TruncatedHeader,
    TruncatedPayload,
    FrameTooLarge,
    FrameTooDeep,
    Invalid(&'static str),
    UnsupportedVersion,
    FenceMismatch,
    ExecutionMismatch,
    MissingCapability,
    HostMismatch,
    CommandConflict,
    CommandCapacity,
    UnknownCommand,
    ReceiptConflict,
    CursorGap,
    CursorConflict,
    StaleCursor,
    UnsupportedSdk,
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Payloads can contain private tool inputs; error display never includes them.
        match self {
            Self::Io(_) => f.write_str("host transport I/O failed"),
            Self::Json(_) => f.write_str("invalid host protocol JSON"),
            other => write!(f, "host protocol rejected: {other:?}"),
        }
    }
}
impl std::error::Error for Error {}
