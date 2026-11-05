//! The failures a crash report explains, read from a task's lifecycle journal: its node's session
//! ended on its own, a message never reached it, or its execution did not come back when the daemon
//! started again. The “Send crash report” button of the task offers a report of the last one.
use super::*;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReportedFailure {
    SessionEnded,
    MessageNotDelivered,
    NotResumed,
}

impl ReportedFailure {
    /// The kind of failure as `alondra report send --kind` names it.
    pub fn word(self) -> &'static str {
        match self {
            ReportedFailure::SessionEnded => "session-ended",
            ReportedFailure::MessageNotDelivered => "message-not-delivered",
            ReportedFailure::NotResumed => "not-resumed",
        }
    }

    fn of(record: &LifecycleRecord, cards: &std::collections::BTreeMap<InteractionId, Interaction>) -> Option<ReportedFailure> {
        match record {
            LifecycleRecord::HostEnded { .. } => Some(ReportedFailure::SessionEnded),
            LifecycleRecord::InputDropped { .. } | LifecycleRecord::HandoffDoubted { .. } => Some(ReportedFailure::MessageNotDelivered),
            LifecycleRecord::Paused { card, .. } if cards.get(card).is_some_and(|card| card.system_card == Some(SystemCard::HostLost)) => Some(ReportedFailure::NotResumed),
            _ => None,
        }
    }
}

impl DomainState {
    /// The last failure of `task` a crash report explains, if it had one. A pause stops the host,
    /// so its end during one is no failure.
    pub fn task_failure(&self, task: &TaskId) -> Option<ReportedFailure> {
        let mut paused = false;
        let mut failure = None;
        for record in self.lifecycle_journal.get(task)? {
            match record {
                LifecycleRecord::Paused { card, .. } if self.node_pause_card(card) => paused = true,
                LifecycleRecord::Resumed { .. } | LifecycleRecord::PauseEnded { .. } => paused = false,
                LifecycleRecord::HostEnded { .. } if paused => continue,
                _ => {}
            }
            failure = ReportedFailure::of(record, &self.interactions).or(failure);
        }
        failure
    }

    /// The “Send crash report” button of `task`, judged by the guard of its command.
    pub fn report_failure_action(&self, task: &TaskId) -> AvailableAction {
        self.action("report_failure", Command::ReportFailure { task_id: task.clone() }, None, None, self)
    }
}
