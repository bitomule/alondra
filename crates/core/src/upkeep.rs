//! What Alondra does to a node's process on its own: restart one that ended while the node still
//! had work, and close a node that has been free for long when the user asked for that. Both read
//! stored events only, so a restart of alondrad neither repeats an attempt nor forgets one.

use crate::model::Node;
use crate::tasks::Task;
use crate::view::View;

/// Where the daemon leaves a session whose process ended without dying: `done` or `blocked` when
/// it stopped an idle one (a reply brings it back), `stopped` when someone stopped it on purpose.
const NOT_DEAD_STATES: &[&str] = &["done", "blocked", STOPPED];
const STOPPED: &str = "stopped";

/// The node's process ended while it was working: neither the daemon letting an idle session go
/// nor someone stopping it.
pub fn died_working(node: &Node) -> bool {
    node.watched() && !node.alive && node.exited_at.is_some() && node.daemon_state.as_deref().is_some_and(|s| !NOT_DEAD_STATES.contains(&s))
}

/// Someone stopped the node's session on purpose: Alondra neither restarts it nor types into it.
pub fn stopped_on_purpose(node: &Node) -> bool {
    node.watched() && !node.alive && node.daemon_state.as_deref() == Some(STOPPED)
}

/// The task a dead process leaves open: not finished, and not closed by the node's own final report.
pub(crate) fn open_task<'a>(view: &'a View, node: &Node) -> Option<&'a Task> {
    view.tasks_of(&node.session_id).find(|t| !t.state.finished() && t.final_report.is_none())
}
