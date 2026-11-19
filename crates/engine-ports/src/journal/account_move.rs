//! A task's move to another account, step by step, as the journal keeps it.
use alondra_domain_rules::{AgentId, InteractionId, TaskId};
use anyhow::Result;
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Step {
    Planned,
    HostStopped,
    Admitted,
    Done,
    Undone,
}

impl Step {
    pub fn key(self) -> &'static str {
        match self {
            Self::Planned => "planned",
            Self::HostStopped => "host_stopped",
            Self::Admitted => "admitted",
            Self::Done => "done",
            Self::Undone => "undone",
        }
    }
    pub fn parse(key: &str) -> Result<Self> {
        Ok(match key {
            "planned" => Self::Planned,
            "host_stopped" => Self::HostStopped,
            "admitted" => Self::Admitted,
            "done" => Self::Done,
            "undone" => Self::Undone,
            other => anyhow::bail!("unknown account move step {other}"),
        })
    }
    /// The move is over, done or undone: nothing of it is left to do.
    pub fn ended(self) -> bool {
        matches!(self, Self::Done | Self::Undone)
    }
}

/// What the plan of a move fixes: the account the task leaves and the one it goes on with, the
/// card of the limit it leaves, the execution that hit it and its Claude session, which the
/// execution on the other account resumes in the same folder.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Plan {
    pub from: alondra_core::accounts::AccountId,
    pub to: alondra_core::accounts::AccountId,
    pub card: InteractionId,
    pub node: AgentId,
    pub instance: String,
    pub session: String,
    pub cwd: String,
    /// Until when the task moves no more: the reset of the limit it left.
    pub until: i64,
}

/// One move as far as it went.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AccountMove {
    pub task: TaskId,
    pub number: i64,
    pub plan: Plan,
    pub step: Step,
    /// The node of the execution admitted on the other account.
    pub agent: Option<AgentId>,
    /// Why a move was undone.
    pub undone: Option<String>,
}
