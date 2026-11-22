#[macro_use]
pub mod log;
pub mod artifact_digest;
pub mod changes;
pub mod clock;
pub mod daemon_log;
pub mod durability;
pub mod exit;
pub mod locks;
pub mod runtime_mutex;
pub mod save;
mod execution;
mod mcp;
mod poll;
pub use execution::*;
pub use mcp::*;
pub use poll::readable;

/// Whether this process runs under `bazel test`, which sets `BAZEL_TEST` (launchd never does): what
/// only tests need, such as a Claude home of their own, is decided here at run time.
pub fn under_test() -> bool {
    static UNDER_TEST: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *UNDER_TEST.get_or_init(|| std::env::var_os("BAZEL_TEST").is_some())
}

#[cfg(test)]
#[path = "../../test_support.rs"]
mod test_support;

use serde::{Deserialize, Serialize};
use std::fmt;

macro_rules! ids { ($($name:ident),*) => {$ (
    #[derive(Clone,Debug,Default,PartialEq,Eq,PartialOrd,Ord,Hash,Serialize,Deserialize)]
    #[serde(transparent)] pub struct $name(pub String);
    impl From<&str> for $name { fn from(v:&str)->Self {Self(v.into())} }
    impl From<String> for $name { fn from(v:String)->Self {Self(v)} }
    impl fmt::Display for $name {fn fmt(&self,f:&mut fmt::Formatter<'_>)->fmt::Result {self.0.fmt(f)}}
)*}; }
ids!(
    TaskId,
    AgentId,
    InteractionId,
    DocumentId,
    DocumentRevisionId,
    PrId,
    RequestId,
    AuthorizationId,
    CommandId,
    DataEpoch,
    ScheduleId,
    ScheduleRunId
);
pub type Revision = u64;
pub const MAX_REVISION: Revision = 9_007_199_254_740_991;

pub type Millis = i64;

/// `bytes` random bytes from the system, in hex.
pub fn random_hex(bytes: usize) -> String {
    use std::io::Read;
    let mut buf = vec![0u8; bytes];
    if let Ok(mut f) = std::fs::File::open("/dev/urandom") {
        let _ = f.read_exact(&mut buf);
    }
    buf.iter().map(|b| format!("{b:02x}")).collect()
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Lang {
    En,
    Es,
}

/// A canonical UUID-valued message identity for adapters that use UUIDs.
/// Business input identities remain separate.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(transparent)]
pub struct MessageUuid(String);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct InvalidMessageUuid;
impl fmt::Display for InvalidMessageUuid {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("message UUID must be hexadecimal 8-4-4-4-12")
    }
}
impl std::error::Error for InvalidMessageUuid {}

impl MessageUuid {
    pub fn new(value: impl Into<String>) -> Result<Self, InvalidMessageUuid> {
        let value = value.into();
        if value.len() != 36
            || !value.bytes().enumerate().all(|(index, byte)| {
                if matches!(index, 8 | 13 | 18 | 23) {
                    byte == b'-'
                } else {
                    byte.is_ascii_hexdigit()
                }
            })
        {
            return Err(InvalidMessageUuid);
        }
        Ok(Self(value.to_ascii_lowercase()))
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
}
impl fmt::Display for MessageUuid {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}
impl<'de> Deserialize<'de> for MessageUuid {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Self::new(String::deserialize(deserializer)?).map_err(serde::de::Error::custom)
    }
}
