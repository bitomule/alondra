//! A task's move to another model or account through the provider, as the journal keeps it.
use alondra_core::accounts::AccountKey;
use alondra_core::model_selection::{ModelSelection, TaskSelection};
use alondra_domain_rules::{AgentId, TaskId};
use anyhow::Result;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Source {
    pub agent: AgentId,
    pub instance: String,
    pub conversation: String,
    pub generation: u64,
    pub account: AccountKey,
    pub configuration: String,
    pub binding: String,
    pub driver: String,
    pub turn_model: bool,
    pub directory: String,
    pub host_directory: std::path::PathBuf,
    pub selection: Option<TaskSelection>,
    pub disallowed_tools: Vec<String>,
}

/// What a selection change does: change the model of the running session, move the task to
/// another account, where a new node takes it over from its record, or start a new session with
/// the same model and account that carries on from that record and the conversation before it,
/// once resuming the conversation would write it whole to the prompt cache again. A stage hands a
/// staged task to a new execution of its next role, on the account and model of that role, from
/// what Alondra writes for it instead of the conversation, once the one it leaves rests.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all="snake_case")]
pub enum Kind { #[default] Model, Move, Refresh, Stage }

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Plan {
    pub id: String,
    #[serde(default)]
    pub kind: Kind,
    pub task: TaskId,
    pub expected_selection: String,
    pub source: Source,
    pub target_account: AccountKey,
    pub target_model: ModelSelection,
    pub intent: TaskSelection,
    pub reason: String,
    /// The file that holds the conversation of the session a refresh leaves, which the new session
    /// reads only when it needs to.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub conversation_file: Option<std::path::PathBuf>,
    /// The role a stage's new execution takes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<alondra_domain_rules::ExecutionRole>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all="snake_case")]
/// `StartRefused` follows a move that ended before its target started, `Done` with `deferred_start`,
/// once that start is refused for a reason that trying again does not change: no pass tries it
/// again until the user asks to, which puts it back at `Done`.
pub enum Phase { Requested, Draining, SourceFenced, ContextReady, TargetAdmitted, TargetReady, Consumed, Done, Aborted, Uncertain, StartRefused }

impl Phase {
    pub fn key(self) -> &'static str { match self {
        Self::Requested=>"requested",Self::Draining=>"draining",Self::SourceFenced=>"source_fenced",Self::ContextReady=>"context_ready",Self::TargetAdmitted=>"target_admitted",Self::TargetReady=>"target_ready",Self::Consumed=>"consumed",Self::Done=>"done",Self::Aborted=>"aborted",Self::Uncertain=>"uncertain",Self::StartRefused=>"start_refused",
    }}
    pub fn parse(value:&str)->Result<Self>{Ok(match value {
        "requested"=>Self::Requested,"draining"=>Self::Draining,"source_fenced"=>Self::SourceFenced,"context_ready"=>Self::ContextReady,"target_admitted"=>Self::TargetAdmitted,"target_ready"=>Self::TargetReady,"consumed"=>Self::Consumed,"done"=>Self::Done,"aborted"=>Self::Aborted,"uncertain"=>Self::Uncertain,"start_refused"=>Self::StartRefused,_=>anyhow::bail!("unknown provider transition phase"),
    })}
}

#[derive(Clone, Debug)]
pub struct Transition { pub plan:Plan,pub phase:Phase,pub facts:serde_json::Value }

/// The prepared context a move keeps: its facts at `ContextReady`, or under `context` once it
/// ended waiting for its target to start.
pub fn prepared_facts(change:Transition)->serde_json::Value {
    if matches!(change.phase,Phase::Done|Phase::StartRefused){change.facts["context"].clone()}else{change.facts}
}
