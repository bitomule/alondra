//! A node's move from `claude --bg` to the Agent SDK, step by step, as the journal keeps it.
use alondra_domain_rules::{AgentId, TaskId};
use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::path::Path;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Step {
    Planned,
    BgStopped,
    SdkAdmitted,
    Done,
}

impl Step {
    pub fn key(self) -> &'static str {
        match self {
            Self::Planned => "planned",
            Self::BgStopped => "bg_stopped",
            Self::SdkAdmitted => "sdk_admitted",
            Self::Done => "done",
        }
    }
    pub fn parse(key: &str) -> Result<Self> {
        Ok(match key {
            "planned" => Self::Planned,
            "bg_stopped" => Self::BgStopped,
            "sdk_admitted" => Self::SdkAdmitted,
            "done" => Self::Done,
            other => anyhow::bail!("unknown SDK handover step {other}"),
        })
    }
}

/// A process named by its PID and the moment it started: a PID alone can be reused.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Process {
    pub pid: i32,
    pub started: (u64, u64),
}

/// What the plan of a node's move fixes: the session it resumes, where, the `claude --bg` process
/// that ran it, and the input that opens it through the SDK.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Plan {
    pub session: String,
    pub cwd: String,
    pub name: String,
    pub process: Option<Process>,
    pub input: String,
    /// The outbox message the input is, handed with the move.
    pub outbox: Option<i64>,
    /// The node was brought back (lost, stalled or paused) rather than found at rest.
    pub returning: bool,
}

/// One node's move as far as it went.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Handover {
    pub node: AgentId,
    pub task: TaskId,
    pub plan: Plan,
    pub step: Step,
    /// The `claude --bg` process seen exiting before the SDK took the session.
    pub exited: Option<Process>,
    /// The SDK node that took the task.
    pub agent: Option<AgentId>,
}

const COPY_SUFFIX: &str = "before-sdk-handover";

/// Where the copy of the database kept before the first move is.
pub fn copy_path(database: &Path) -> std::path::PathBuf {
    let mut name = database.file_name().unwrap_or_default().to_os_string();
    name.push(format!(".{COPY_SUFFIX}"));
    database.with_file_name(name)
}
