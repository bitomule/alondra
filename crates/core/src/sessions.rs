//! Which Claude sessions are Alondra's. Alondra launches every node and its orchestrator with its
//! plugin and its mark (`ALONDRA_LAUNCHED`). Any other session is left alone: no node, no task, no
//! card. Sessions from before the mark count when the records tie them to a launch.

use crate::model::QuestionStatus;
use crate::view::View;

impl View {
    /// A launch of Alondra's printed this session's short id.
    pub fn launch_printed(&self, session_id: &str) -> bool {
        session_id.get(..8).is_some_and(|short| self.launched_sessions.contains_key(short))
    }

    /// The records tie the session to Alondra: it is the orchestrator, a launch printed its id,
    /// or it works on a task Alondra launched it for or the orchestrator handed it.
    pub fn launched_by_alondra(&self, session_id: &str) -> bool {
        self.nodes.get(session_id).is_some_and(|n| n.is_orchestrator)
            || self.launch_printed(session_id)
            || self.tasks_of(session_id).any(|t| t.launched || self.board.requests.get(&t.request).is_some_and(|r| !r.synthetic))
    }

    /// `LaunchedOnly`: every session the records do not tie to Alondra leaves the view with the
    /// single-node request made up for it, and its open cards close.
    pub(crate) fn leave_out_foreign(&mut self) {
        let assignees = self.board.tasks.values().filter_map(|t| t.assignee.as_ref().map(|a| a.session_id.clone()));
        let sessions: std::collections::BTreeSet<String> = self.nodes.keys().cloned().chain(assignees).collect();
        let foreign: Vec<String> = sessions.into_iter().filter(|s| !self.launched_by_alondra(s)).collect();
        for sid in foreign {
            self.nodes.remove(&sid);
            for q in self.questions.values_mut().filter(|q| q.session_id == sid && q.status.is_open()) {
                q.status = QuestionStatus::Orphaned;
            }
            self.drop_tasks(&sid);
            self.left_out.insert(sid);
        }
    }
}
