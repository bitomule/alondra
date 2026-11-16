//! A node is stuck when what its lifecycle expects and what its host or provider reported
//! disagree. Each disagreement is a typed fact the lifecycle checks against its own state, and
//! the recovery it takes is decided by how many came in a row with no progress between them,
//! never by time.
use super::*;

/// What the provider reported and the lifecycle could not apply.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Evidence {
    TurnEnd,
    /// The native process, or the connection to it, was lost.
    Transport,
    /// What the provider's normalizer could not read.
    Unreadable,
}

/// A disagreement between the lifecycle and its host or provider.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Contradiction {
    /// The provider ended a turn the lifecycle could not end, and its turn stays open.
    TurnEndUnapplied,
    /// The execution runs work and no host can be taken over for it.
    HostLost,
}

/// What Alondra does about a contradiction.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Recovery {
    /// The open turn ends as a turn that explains nothing, which hands the node back its turn.
    CloseTurn,
    /// A host that resumes the same session takes the place of the current one.
    ReplaceHost,
    /// The open turn ends and the orchestrator gets the case.
    OpenCase,
}

/// How many recoveries in a row, with no progress between them, come before the case.
pub const RECOVERIES: u8 = 2;

impl Contradiction {
    fn natural(self) -> Recovery {
        match self {
            Self::TurnEndUnapplied => Recovery::CloseTurn,
            Self::HostLost => Recovery::ReplaceHost,
        }
    }
}

impl Recovery {
    /// Whether the recovery ends the open turn itself, with the turn end recorded right after it.
    pub fn ends_turn(self) -> bool {
        matches!(self, Self::CloseTurn | Self::OpenCase)
    }
}

impl TaskLifecycle {
    /// How many contradictions came in a row with no progress between them.
    pub fn recoveries(&self) -> u8 {
        self.recoveries
    }

    /// Whether the last recovery asked for a host that resumes the session, and the current host
    /// has not ended since.
    pub fn replaces_host(&self) -> bool {
        self.replacing
    }

    /// The contradiction `evidence` shows against what the lifecycle expects, if any.
    pub fn contradiction(&self, evidence: Evidence) -> Option<Contradiction> {
        let execution = self.session()?;
        let turn_open = execution.resource == ResourceEvidence::Available && matches!(execution.activity, Activity::Turn { .. });
        (evidence == Evidence::TurnEnd && turn_open && !matches!(self.state, State::Finishing(_))).then_some(Contradiction::TurnEndUnapplied)
    }

    /// The recovery the next `contradiction` takes: its own the first time, then a new host, then
    /// the orchestrator's case.
    pub fn recovery(&self, contradiction: Contradiction) -> Recovery {
        match self.recoveries {
            0 => contradiction.natural(),
            count if count < RECOVERIES => Recovery::ReplaceHost,
            _ => Recovery::OpenCase,
        }
    }

    /// Whether the lifecycle is in the state `contradiction` disagrees with.
    pub fn contradicts(&self, contradiction: Contradiction) -> bool {
        match contradiction {
            Contradiction::TurnEndUnapplied => self.contradiction(Evidence::TurnEnd).is_some(),
            Contradiction::HostLost => !matches!(self.state, State::Finishing(_)) && self.session().is_some_and(|execution| !execution.activity.rests()),
        }
    }

    pub(super) fn contradicted(&mut self, contradiction: Contradiction, recovery: Recovery) -> Result<(), Refusal> {
        if !self.contradicts(contradiction) || recovery.ends_turn() && contradiction != Contradiction::TurnEndUnapplied {
            return Err(Refusal::InvalidState);
        }
        if recovery != self.recovery(contradiction) {
            return Err(Refusal::CorrelationMismatch);
        }
        self.recoveries = self.recoveries.saturating_add(1);
        self.replacing = recovery == Recovery::ReplaceHost;
        Ok(())
    }

    pub(super) fn evidence_unapplied(&self) -> Result<(), Refusal> {
        self.session().map(drop).ok_or(Refusal::InvalidState)
    }
}

/// Whether `event` leaves the count of recoveries as it was: a contradiction and the turn end its
/// recovery records, and what neither the node's own result nor anyone told the task. A turn the
/// provider ended, a question, work of the node's own, an input, a card or a delivery is progress.
pub(super) fn keeps_recoveries(event: &Event, ending: bool) -> bool {
    match event {
        Event::Contradicted { .. } | Event::EvidenceUnapplied { .. } => true,
        Event::Runtime { fact, .. } => match fact {
            RuntimeFact::TurnEnded { .. } => ending,
            RuntimeFact::Ask { .. }
            | RuntimeFact::WorkStarted { .. }
            | RuntimeFact::WorkEnded { .. }
            | RuntimeFact::ContinuationConsumed { .. }
            | RuntimeFact::CallbackSettled { .. }
            | RuntimeFact::AnsweredByPolicy { .. } => false,
            _ => true,
        },
        _ => matches!(event, Event::RequestControl { .. } | Event::DrainObserved { .. } | Event::AwaitEnded { .. } | Event::Unfinished { .. } | Event::HostReleased | Event::HostRetaken),
    }
}
