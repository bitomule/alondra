//! The Tasks screen: every request with its task tree, and what the daemon says each task is at.

use alondra_core::domain::{RequestId, TaskId};

#[derive(Clone, PartialEq, Eq, Hash, Debug, serde::Serialize, serde::Deserialize)]
pub enum TaskKey {
    Task(TaskId),
    Request(RequestId),
}
