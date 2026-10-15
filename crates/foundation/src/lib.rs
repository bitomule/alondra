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
