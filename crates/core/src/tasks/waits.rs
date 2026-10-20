//! What a started task waits for outside its node: the merge of a PR, its own or one of a task it
//! depends on. A task that has nothing left but that merge waits for it (`awaitable_merge`), and
//! every merge reaches the nodes of the tasks it concerns (`merge_notices_due`), whichever way the
//! PR was merged and whenever those nodes can hear it.

use super::{Task, TaskState};
use crate::pr::PullRequest;
use crate::view::View;

impl View {
    /// The dependency's PR a running task can only wait for: nothing of its own is open for
    /// review, and a task it depends on still has a PR open.
    pub fn awaitable_merge(&self, task: &Task) -> Option<&PullRequest> {
        if task.state != TaskState::Running || task.blocker.is_some() || self.open_prs(task).next().is_some() {
            return None;
        }
        task.depends_on.iter().filter_map(|d| self.board.tasks.get(d)).flat_map(|d| self.open_prs(d)).next()
    }
}
