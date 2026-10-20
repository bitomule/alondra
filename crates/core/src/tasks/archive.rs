//! Archiving: the user clears finished work off the Tasks list. An archived task leaves the
//! active list, and the board `archived_tasks_days` later (`Event::ArchivedPruned`); until it is
//! archived a finished task stays, whatever the retention window.

use serde::{Deserialize, Serialize};

use super::{Board, RequestState, Task};
use crate::model::Millis;

/// What an archive takes: finished tasks, and requests that finished without tasks.
#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
pub struct Archive {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tasks: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub requests: Vec<String>,
}

impl Board {
    /// A request finished without tasks: the orchestrator answered it instead of defining work.
    pub(super) fn finished_without_tasks(&self, id: &str) -> bool {
        self.request_state(id) == RequestState::Done && !self.tasks.values().any(|t| t.request == id)
    }

    pub(crate) fn archive(&mut self, what: &Archive, at: Millis) {
        for id in &what.tasks {
            if let Some(t) = self.tasks.get_mut(id).filter(|t| t.state.finished() && t.archived_at.is_none()) {
                t.archived_at = Some(at);
            }
        }
        for id in &what.requests {
            if self.finished_without_tasks(id)
                && let Some(r) = self.requests.get_mut(id).filter(|r| r.archived_at.is_none())
            {
                r.archived_at = Some(at);
            }
        }
    }

    /// Forgets the tasks archived before `before` that `keep` does not hold, the requests they
    /// leave without tasks, and the requests archived without tasks before `before`.
    pub(crate) fn prune_archived(&mut self, before: Millis, keep: impl Fn(&Task) -> bool) {
        let expired: Vec<(String, String)> =
            self.tasks.values().filter(|t| t.archived_at.is_some_and(|at| at < before) && !keep(t)).map(|t| (t.id.clone(), t.request.clone())).collect();
        for (id, _) in &expired {
            self.tasks.remove(id);
        }
        for (_, request) in &expired {
            if !self.tasks.values().any(|t| &t.request == request) {
                self.requests.remove(request);
            }
        }
        let tasks = &self.tasks;
        self.requests.retain(|id, r| r.archived_at.is_none_or(|at| at >= before) || tasks.values().any(|t| &t.request == id));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::{Event, HookEvent};
    use crate::tasks::{NewTask, TaskEvent, TaskOptions, TaskState};
    use crate::view::View;

    const DAY: Millis = 24 * 60 * 60 * 1000;

    fn new_task(id: &str, request: &str, session: Option<&str>) -> NewTask {
        NewTask {
            id: id.into(), request: request.into(), title: id.into(), brief: None, depends_on: vec![], options: TaskOptions::default(),
            session_id: session.map(str::to_string), launched: false, start: None, launched_as: None,
        }
    }

    fn created(id: &str, request: &str, session: Option<&str>) -> Event {
        Event::TaskCreated(new_task(id, request, session))
    }

    fn sent(id: &str) -> Event {
        let options = crate::options::Resolved { project: None, auto_merge: false, effort: "normal".into(), flags: Default::default(), picked: vec![], issue: None };
        Event::RequestSent { request_id: id.into(), options, text: Some("How is it going?".into()), attachments: vec![] }
    }

    fn moved(id: &str, event: TaskEvent) -> Event {
        Event::TaskMoved { task: id.into(), event }
    }

    /// r-1 holds t-done (done) and t-gone (cancelled); r-2 holds t-run, still running.
    fn log() -> Vec<(Millis, Event)> {
        vec![
            (1, Event::TasksRecorded),
            (2, created("t-done", "r-1", Some("n"))),
            (3, created("t-gone", "r-1", Some("n"))),
            (4, created("t-run", "r-2", Some("n"))),
            (5, moved("t-done", TaskEvent::FinalReport { report: 1 })),
            (6, moved("t-gone", TaskEvent::Cancel { reason: None })),
        ]
    }

    fn replay(log: &[(Millis, Event)]) -> View {
        let mut v = View::default();
        for (seq, (at, e)) in log.iter().enumerate() {
            v.apply(seq as u64 + 1, *at, e);
        }
        v
    }

    fn apply(v: &mut View, at: Millis, e: Event) {
        v.apply(v.seq + 1, at, &e);
    }

    #[test]
    fn finished_tasks_outlive_the_retention_window_until_they_are_archived() {
        let mut v = replay(&log());
        apply(&mut v, 100 * DAY, Event::Pruned { before: 99 * DAY });
        assert!(v.board.tasks.contains_key("t-done") && v.board.tasks.contains_key("t-gone") && v.board.requests.contains_key("r-1"));
    }

    /// Only what an older log left behind goes with the window: a task read from a node's events,
    /// and the request made up for it, once the node is gone.
    #[test]
    fn unfinished_recorded_tasks_and_the_users_requests_outlive_the_window() {
        let mut v = replay(&log());
        let start = crate::tasks::Start::NewNode { name: "later".into(), cwd: "/tmp".into(), model: None };
        apply(&mut v, 7, Event::TaskCreated(NewTask { depends_on: vec!["t-run".into()], start: Some(start), ..new_task("t-wait", "r-2", None) }));
        apply(&mut v, 8, sent("r-defining"));
        apply(&mut v, 100 * DAY, Event::Pruned { before: 99 * DAY });
        assert_eq!(v.board.tasks.get("t-wait").map(|t| t.state), Some(TaskState::WaitingDependency), "it waits for t-run");
        assert!(v.board.tasks.contains_key("t-run") && v.board.requests.contains_key("r-2"), "running, with no node yet");
        assert!(v.board.requests.contains_key("r-defining"), "the orchestrator has not defined it yet");

        let mut older = View::default();
        older.apply(1, 1, &Event::Hook(HookEvent { session_id: "s".into(), event: "UserPromptSubmit".into(), ..Default::default() }));
        assert!(older.board.tasks.get("t-s").is_some_and(|t| !t.recorded));
        older.apply(2, 100 * DAY, &Event::Pruned { before: 99 * DAY });
        assert!(older.board.tasks.is_empty() && older.board.requests.is_empty(), "its node went silent: its task and the request made up for it go");
    }
}
