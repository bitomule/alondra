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

impl Archive {
    pub fn is_empty(&self) -> bool {
        self.tasks.is_empty() && self.requests.is_empty()
    }
}

impl Board {
    /// When the request was archived: every task of it is, or, with no task, it finished and
    /// was archived itself.
    pub fn request_archived_at(&self, id: &str) -> Option<Millis> {
        let mut tasks = self.tasks.values().filter(|t| t.request == id).peekable();
        if tasks.peek().is_none() {
            return self.requests.get(id)?.archived_at;
        }
        tasks.try_fold(0, |newest, t| t.archived_at.map(|at| newest.max(at)))
    }

    /// A request finished without tasks: the orchestrator answered it instead of defining work.
    pub(super) fn finished_without_tasks(&self, id: &str) -> bool {
        self.request_state(id) == RequestState::Done && !self.tasks.values().any(|t| t.request == id)
    }

    /// What is finished and not archived yet, all of it or what `ids` names: tasks, and requests
    /// that finished without tasks.
    pub fn archivable(&self, ids: Option<&[String]>) -> Archive {
        let named = |id: &String| ids.is_none_or(|ids| ids.contains(id));
        Archive {
            tasks: self.tasks.values().filter(|t| t.state.finished() && t.archived_at.is_none() && named(&t.id)).map(|t| t.id.clone()).collect(),
            requests: self
                .requests
                .values()
                .filter(|r| r.archived_at.is_none() && named(&r.id) && self.finished_without_tasks(&r.id))
                .map(|r| r.id.clone())
                .collect(),
        }
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

    fn created(id: &str, request: &str, session: Option<&str>) -> Event {
        Event::TaskCreated(NewTask {
            id: id.into(), request: request.into(), title: id.into(), brief: None, depends_on: vec![], options: TaskOptions::default(),
            session_id: session.map(str::to_string), launched: false, start: None,
        })
    }

    fn moved(id: &str, event: TaskEvent) -> Event {
        Event::TaskMoved { task: id.into(), event }
    }

    /// r-1 holds t-done (done) and t-gone (cancelled); r-2 holds t-run, still running.
    fn log() -> Vec<(Millis, Event)> {
        vec![
            (1, Event::TasksRecorded),
            (2, created("t-done", "r-1", None)),
            (3, created("t-gone", "r-1", None)),
            (4, created("t-run", "r-2", None)),
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

    fn archived(v: &View, id: &str) -> Option<Millis> {
        v.board.tasks[id].archived_at
    }

    #[test]
    fn an_archive_is_recorded_and_survives_replay_and_a_snapshot() {
        let mut log = log();
        let what = replay(&log).board.archivable(None);
        assert_eq!(what, Archive { tasks: vec!["t-done".into(), "t-gone".into()], requests: vec![] });
        log.push((10 * DAY, Event::TasksArchived(what)));
        log.push((11 * DAY, moved("t-run", TaskEvent::Cancel { reason: None })));
        let stored: Event = serde_json::from_str(&serde_json::to_string(&log[6].1).unwrap()).unwrap();
        assert_eq!(stored, log[6].1, "the event reads back from the log as it was written");

        let v = replay(&log);
        assert_eq!((archived(&v, "t-done"), archived(&v, "t-gone"), archived(&v, "t-run")), (Some(10 * DAY), Some(10 * DAY), None));
        assert_eq!((v.board.request_archived_at("r-1"), v.board.request_archived_at("r-2")), (Some(10 * DAY), None));

        let snapshot = serde_json::to_string(&replay(&log[..7])).unwrap();
        let mut resumed: View = serde_json::from_str(&snapshot).unwrap();
        for (at, e) in &log[7..] {
            apply(&mut resumed, *at, e.clone());
        }
        assert_eq!(resumed.board, v.board, "a snapshot taken after the archive keeps it");
    }

    #[test]
    fn only_finished_tasks_are_archived_and_a_running_one_never_is() {
        let mut v = replay(&log());
        let named = v.board.archivable(Some(&["t-run".into(), "t-done".into()]));
        assert_eq!(named.tasks, vec!["t-done".to_string()], "the running task named is left out");
        apply(&mut v, 7, Event::TasksArchived(Archive { tasks: vec!["t-run".into(), "t-done".into()], requests: vec![] }));
        assert_eq!((archived(&v, "t-done"), archived(&v, "t-gone"), archived(&v, "t-run")), (Some(7), None, None), "an event naming a running task does not archive it");
        assert_eq!(v.board.request_archived_at("r-1"), None, "t-gone is not archived yet");

        let rest = v.board.archivable(None);
        apply(&mut v, 8, Event::TasksArchived(rest));
        assert_eq!((archived(&v, "t-done"), archived(&v, "t-gone")), (Some(7), Some(8)), "archiving again keeps the first date");
        assert_eq!(v.board.request_archived_at("r-1"), Some(8));
        assert!(v.board.archivable(None).is_empty());

        apply(&mut v, 9, moved("t-done", TaskEvent::Reopen));
        assert_eq!((v.board.tasks["t-done"].state, archived(&v, "t-done")), (TaskState::Running, None), "a reopened task is active again");
        assert_eq!(v.board.request_archived_at("r-1"), None);
    }

    #[test]
    fn archived_work_is_forgotten_once_its_days_are_over_and_not_before() {
        let mut v = replay(&log());
        apply(&mut v, 7, Event::Hook(HookEvent { session_id: "s".into(), event: "UserPromptSubmit".into(), ..Default::default() }));
        apply(&mut v, 8, created("t-live", "r-3", Some("s")));
        apply(&mut v, 9, moved("t-live", TaskEvent::Cancel { reason: None }));
        let what = v.board.archivable(None);
        apply(&mut v, 10 * DAY, Event::TasksArchived(what));
        apply(&mut v, 40 * DAY, Event::ArchivedPruned { before: 10 * DAY });
        assert!(v.board.tasks.contains_key("t-done") && v.board.requests.contains_key("r-1"), "archived exactly as long ago as the option says: kept");

        apply(&mut v, 40 * DAY + 1, Event::ArchivedPruned { before: 10 * DAY + 1 });
        assert!(!v.board.tasks.contains_key("t-done") && !v.board.tasks.contains_key("t-gone"));
        assert!(!v.board.requests.contains_key("r-1"), "a request left without tasks goes with them");
        assert!(v.board.tasks.contains_key("t-run") && v.board.requests.contains_key("r-2"), "unarchived work is untouched");
        assert!(v.board.tasks.contains_key("t-live"), "its node is still here");
    }

    #[test]
    fn a_request_archived_without_tasks_goes_after_its_days_too() {
        let mut v = replay(&log());
        let options = crate::options::Resolved { project: None, auto_merge: false, effort: "normal".into(), flags: Default::default(), picked: vec![], issue: None };
        let sent = |id: &str| Event::RequestSent { request_id: id.into(), options: options.clone(), text: Some("How is it going?".into()), attachments: vec![] };
        apply(&mut v, 7, sent("r-answered"));
        apply(&mut v, 8, sent("r-defining"));
        v.board.requests.get_mut("r-answered").unwrap().archived_at = Some(9);
        assert_eq!(v.board.request_archived_at("r-answered"), Some(9));
        apply(&mut v, 20, Event::ArchivedPruned { before: 10 });
        assert!(!v.board.requests.contains_key("r-answered"));
        assert!(v.board.requests.contains_key("r-defining"), "a request still being defined is not archived work");
    }

    #[test]
    fn finished_tasks_outlive_the_retention_window_until_they_are_archived() {
        let mut v = replay(&log());
        apply(&mut v, 100 * DAY, Event::Pruned { before: 99 * DAY });
        assert!(v.board.tasks.contains_key("t-done") && v.board.tasks.contains_key("t-gone") && v.board.requests.contains_key("r-1"));
        assert!(!v.board.tasks.contains_key("t-run"), "an unfinished task nobody works on still goes with the window");
    }
}
