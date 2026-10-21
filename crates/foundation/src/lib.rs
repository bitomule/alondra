mod execution;
mod mcp;
pub use execution::*;
pub use mcp::*;

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
