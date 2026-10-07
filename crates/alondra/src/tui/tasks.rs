//! The Tasks screen: every request with its task tree, and what the daemon says each task is at.

use alondra_core::domain::{RequestId, TaskId, TaskState};

#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub enum TaskKey {
    Task(TaskId),
    Request(RequestId),
}

pub fn state_label(s: TaskState) -> &'static str {
    match s {
        TaskState::NotStarted => tr!("not started", "sin empezar"),
        TaskState::Running => tr!("working", "trabajando"),
        TaskState::NeedsYou => tr!("needs you", "te necesita"),
        TaskState::ForYourReview => tr!("in review", "en revisión"),
        TaskState::AwaitingBlocker => tr!("waiting", "en espera"),
        TaskState::WaitingDependency => tr!("waiting", "en espera"),
        TaskState::Closing => tr!("closing", "cerrando"),
        TaskState::Done => tr!("done", "terminada"),
        TaskState::Cancelled => tr!("cancelled", "descartada"),
    }
}
