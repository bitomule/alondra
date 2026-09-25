//! Which Claude sessions are Alondra's. Alondra launches every node and its orchestrator with its
//! plugin and its mark (`ALONDRA_LAUNCHED`), and records the short id `claude --bg` prints. Any
//! other session is left alone: no node, no task, no card. Sessions from before the mark count
//! when the records tie them to a launch.

use crate::model::QuestionStatus;
use crate::view::View;

/// What Alondra does with a session it hears from.
#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub enum Admission {
    /// Alondra follows it already.
    Follows,
    /// Alondra launched it and hears from it for the first time.
    Joins,
    /// Not Alondra's: nothing about it is recorded.
    Outside,
}

/// The short id in what `claude --bg` printed: `backgrounded · 7c5dcf5d · name`, then
/// `claude attach 7c5dcf5d` among the commands.
pub fn launched_short(output: &str) -> Option<String> {
    let valid = |s: &str| !s.is_empty() && s.chars().all(|c| c.is_ascii_alphanumeric());
    let banner = output.lines().find_map(|line| {
        let rest = line.trim().strip_prefix("backgrounded")?;
        rest.split('·').map(str::trim).find(|p| !p.is_empty()).filter(|s| valid(s))
    });
    let attach = || output.lines().find_map(|line| line.trim().strip_prefix("claude attach ")?.split_whitespace().next().filter(|s| valid(s)));
    banner.or_else(attach).map(str::to_string)
}

impl View {
    pub fn admission(&self, session_id: &str, marked: bool) -> Admission {
        if self.removed.contains_key(session_id) {
            Admission::Outside
        } else if self.nodes.contains_key(session_id) {
            Admission::Follows
        } else if marked || self.launch_printed(session_id) {
            Admission::Joins
        } else {
            Admission::Outside
        }
    }

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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::{DaemonObservation, Event, HookEvent};
    use crate::model::{Millis, Question, QuestionOrigin, Report, ReportKind};
    use crate::tasks::{NewTask, TaskOptions};

    fn seen(sid: &str, name: &str) -> Event {
        Event::DaemonSeen(DaemonObservation { session_id: sid.into(), short_id: sid[..8].into(), name: Some(name.into()), pid: Some(1), ..Default::default() })
    }

    fn hook(sid: &str, event: &str) -> Event {
        Event::Hook(HookEvent { session_id: sid.into(), event: event.into(), ..Default::default() })
    }

    fn asked(id: u64, sid: &str) -> Event {
        Event::QuestionAsked(Question {
            id,
            session_id: sid.into(),
            node_name: None,
            title: "Merge it?".into(),
            detail: None,
            options: vec![],
            links: vec![],
            attachments: vec![],
            blocking: true,
            origin: QuestionOrigin::Ask,
            tool_use_id: None,
            to_orchestrator: false,
            node_context: None,
            status: QuestionStatus::Open,
            answer: None,
            asked_at: 0,
            action: false,
            steps: vec![],
            mission: None,
            replaces: None,
            task: None,
        })
    }

    fn reported(id: u64, sid: &str) -> Event {
        Event::ReportPosted(Report {
            id,
            session_id: sid.into(),
            node_name: None,
            title: "Weekly sweep done".into(),
            body: Some("Two PRs".into()),
            links: vec![],
            attachments: vec![],
            kind: ReportKind::Notice,
            created_at: 0,
            read: false,
            to_orchestrator: false,
            mission: None,
            looks_like_question: false,
            task: None,
        })
    }

    fn request(id: &str) -> Event {
        let options = crate::options::Resolved { project: None, auto_merge: false, effort: "auto".into(), flags: Default::default(), picked: vec![], issue: None };
        Event::RequestSent { request_id: id.into(), options, text: Some("Do it".into()), attachments: vec![] }
    }

    const OWN: &str = "7e661a50-a308-4859-a2e1-003554431ff7";
    const ORCHESTRATOR: &str = "0100014c-8803-469d-ba89-ae6fd660c420";
    const HANDED: &str = "a7e1d69b-4ecd-4686-8796-a36ea056dc50";
    const CRON: &str = "eacfc103-0000-0000-0000-000000000000";
    const PROBE: &str = "516aecfe-0000-0000-0000-000000000000";
    const GONE: &str = "58e7d0aa-0000-0000-0000-000000000000";

    /// The fleet as it stood before the mark: a node bound to its launch before tasks were
    /// recorded, the orchestrator, a node the orchestrator handed a task, a cron session, a probe
    /// that asked something, and a closed cron session.
    fn before_the_mark() -> View {
        let mut v = View::default();
        let mut at: Millis = 1_000;
        let mut on = |v: &mut View, e: Event| {
            at += 1_000;
            v.record(at, &e);
        };
        on(&mut v, request("r-1790263152254"));
        on(&mut v, Event::Launched { name: "alondra/estados-por-tarea".into(), request_id: Some("r-1790263152254".into()), auto_merge: false, task: None, title: None, session: None });
        on(&mut v, seen(OWN, "alondra/estados-por-tarea"));
        on(&mut v, Event::MarkedOrchestrator { session_id: ORCHESTRATOR.into() });
        on(&mut v, hook(ORCHESTRATOR, "SessionStart"));
        on(&mut v, seen(GONE, "Kestrel · revisión jueves"));
        on(&mut v, Event::NodeRemoved { session_id: GONE.into() });
        on(&mut v, Event::TasksRecorded);
        on(&mut v, seen(HANDED, "redraft/atasco-analizando"));
        on(&mut v, request("r-2"));
        let task = NewTask {
            id: "t-5".into(),
            request: "r-2".into(),
            title: "Redraft: beta".into(),
            brief: None,
            depends_on: vec![],
            options: TaskOptions::default(),
            session_id: None,
            launched: false,
            launched_as: None,
            start: None,
        };
        on(&mut v, Event::TaskCreated(task));
        on(&mut v, Event::TaskAssigned { task: "t-5".into(), session_id: HANDED.into(), agent_id: None });
        on(&mut v, seen(CRON, "MealScale · mejora semanal"));
        on(&mut v, hook(CRON, "UserPromptSubmit"));
        on(&mut v, reported(1, CRON));
        on(&mut v, hook(PROBE, "SessionStart"));
        on(&mut v, asked(1, PROBE));
        v
    }

    #[test]
    fn the_short_id_comes_from_what_claude_bg_printed() {
        let printed = "backgrounded · ff2d5ef8 · blurcamera/redaccion-mas-alla-de-caras\n  claude agents             list sessions\n  claude attach ff2d5ef8    open in this terminal\n";
        assert_eq!(launched_short(printed).as_deref(), Some("ff2d5ef8"));
        assert_eq!(launched_short("  claude attach 7c5dcf5d    open in this terminal").as_deref(), Some("7c5dcf5d"));
        assert_eq!(launched_short("started"), None);
    }

    #[test]
    fn only_sessions_the_records_tie_to_alondra_stay_once_it_follows_only_its_own() {
        let mut v = before_the_mark();
        assert_eq!(v.board.requests.keys().filter(|r| v.board.requests[*r].synthetic).count(), 3, "cron, probe and closed cron got made-up requests: {:?}", v.board.requests.keys());
        v.apply(v.seq + 1, 99_000, &Event::LaunchedOnly);

        let nodes: Vec<&str> = v.nodes.keys().map(String::as_str).collect();
        assert_eq!(nodes, vec![ORCHESTRATOR, OWN, HANDED], "the cron and the probe leave Fleet");
        let requests: Vec<&str> = v.board.requests.keys().map(String::as_str).collect();
        assert_eq!(requests, vec!["r-1790263152254", "r-2"], "no single-node request is left for a session Alondra did not launch");
        let tasks: Vec<&str> = v.board.tasks.keys().map(String::as_str).collect();
        assert_eq!(tasks, vec!["t-5".to_string(), format!("t-{OWN}")]);
        assert_eq!(v.questions[&1].status, QuestionStatus::Orphaned, "the probe's question leaves the inbox");
        assert_eq!(v.inbox_reports().count(), 0, "the cron's report leaves the inbox");
        assert_eq!(v.left_out.len(), 3);
        assert_eq!(v.launched_only_since, Some(99_000));
    }

    #[test]
    fn a_session_joins_by_its_mark_or_by_the_id_its_launch_printed() {
        let mut v = before_the_mark();
        v.apply(v.seq + 1, 99_000, &Event::LaunchedOnly);
        assert_eq!(v.admission(OWN, false), Admission::Follows, "a running Alondra node needs no mark");
        assert_eq!(v.admission(ORCHESTRATOR, false), Admission::Follows);
        assert_eq!(v.admission(CRON, false), Admission::Outside);
        assert_eq!(v.admission("11112222-new", true), Admission::Joins);
        assert_eq!(v.admission("33334444-new", false), Admission::Outside);
        v.apply(v.seq + 1, 99_500, &Event::Launched { name: "app/fix".into(), request_id: None, auto_merge: false, task: None, title: None, session: Some("33334444".into()) });
        assert_eq!(v.admission("33334444-new", false), Admission::Joins);
        v.apply(v.seq + 1, 99_600, &Event::NodeRemoved { session_id: OWN.into() });
        assert_eq!(v.admission(OWN, true), Admission::Outside, "a node David closed stays closed");
    }
}
