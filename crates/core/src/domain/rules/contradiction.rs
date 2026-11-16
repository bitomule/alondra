//! What the provider reported and Alondra could not apply is a fact of the task's lifecycle, never
//! a line in a log. When it disagrees with what the lifecycle expects, the lifecycle takes the
//! recovery its count of contradictions in a row gives: it ends the open turn and wakes the node,
//! then a host that resumes the session takes over, then the orchestrator gets the case. Nothing
//! here measures time and nothing reaches the user.
use super::*;
use alondra_domain::{Contradiction, Evidence, Recovery};
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UnappliedEvidence {
    TurnEnd,
    Transport,
    Unreadable,
}
impl From<UnappliedEvidence> for Evidence {
    fn from(evidence: UnappliedEvidence) -> Self {
        match evidence {
            UnappliedEvidence::TurnEnd => Self::TurnEnd,
            UnappliedEvidence::Transport => Self::Transport,
            UnappliedEvidence::Unreadable => Self::Unreadable,
        }
    }
}
impl From<Evidence> for UnappliedEvidence {
    fn from(evidence: Evidence) -> Self {
        match evidence {
            Evidence::TurnEnd => Self::TurnEnd,
            Evidence::Transport => Self::Transport,
            Evidence::Unreadable => Self::Unreadable,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContradictionRecord {
    TurnEndUnapplied,
    HostLost,
}
impl From<ContradictionRecord> for Contradiction {
    fn from(record: ContradictionRecord) -> Self {
        match record {
            ContradictionRecord::TurnEndUnapplied => Self::TurnEndUnapplied,
            ContradictionRecord::HostLost => Self::HostLost,
        }
    }
}
impl From<Contradiction> for ContradictionRecord {
    fn from(contradiction: Contradiction) -> Self {
        match contradiction {
            Contradiction::TurnEndUnapplied => Self::TurnEndUnapplied,
            Contradiction::HostLost => Self::HostLost,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RecoveryRecord {
    CloseTurn,
    ReplaceHost,
    OpenCase,
}
impl From<RecoveryRecord> for Recovery {
    fn from(record: RecoveryRecord) -> Self {
        match record {
            RecoveryRecord::CloseTurn => Self::CloseTurn,
            RecoveryRecord::ReplaceHost => Self::ReplaceHost,
            RecoveryRecord::OpenCase => Self::OpenCase,
        }
    }
}
impl From<Recovery> for RecoveryRecord {
    fn from(recovery: Recovery) -> Self {
        match recovery {
            Recovery::CloseTurn => Self::CloseTurn,
            Recovery::ReplaceHost => Self::ReplaceHost,
            Recovery::OpenCase => Self::OpenCase,
        }
    }
}

impl DomainState {
    /// The provider of `task`'s execution `generation` reported `evidence` that Alondra could not
    /// apply: the lifecycle records it, and takes its recovery when it contradicts what it expects.
    pub(in crate::domain) fn record_unapplied_evidence(&mut self, task: &TaskId, generation: u64, evidence: UnappliedEvidence,
        effects: &mut Vec<DomainEffect>, lifecycles: &mut Lifecycles) -> Result<()> {
        self.sdk_execution(task, generation, lifecycles)?;
        match self.lifecycle(lifecycles, task)?.contradiction(evidence.into()) {
            Some(contradiction) => self.contradicted(task, generation, contradiction, effects, lifecycles),
            None => self.decide(lifecycles, task, LifecycleRecord::EvidenceUnapplied { evidence }),
        }
    }

    /// The lifecycle of `task` disagrees with its host or provider as `contradiction` says, and
    /// takes the recovery its count of contradictions in a row gives.
    pub(in crate::domain) fn contradicted(&mut self, task: &TaskId, generation: u64, contradiction: Contradiction,
        effects: &mut Vec<DomainEffect>, lifecycles: &mut Lifecycles) -> Result<()> {
        let lifecycle = self.lifecycle(lifecycles, task)?;
        let recovery = lifecycle.recovery(contradiction);
        let turn = lifecycle.turn().map(|turn| turn.as_str().to_owned());
        self.decide(lifecycles, task, LifecycleRecord::Contradicted { contradiction: contradiction.into(), recovery: recovery.into() })?;
        let Some(turn) = turn.filter(|_| recovery.ends_turn()) else { return Ok(()) };
        let cause = EmptyTurn::Contradicted(contradiction);
        self.finish_observed_turn(task, generation, &turn, None, true, Some(cause), recovery == Recovery::CloseTurn, effects, lifecycles)
    }
}
