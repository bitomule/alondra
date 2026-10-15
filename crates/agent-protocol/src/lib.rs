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

pub const PROTOCOL_VERSION: u16 = 1;
pub const SDK_VERSION: &str = "0.3.300";
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
