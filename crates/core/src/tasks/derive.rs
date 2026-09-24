//! Tasks read from the events Alondra records today. Until nodes name the task they work on,
//! each session is one request with one task, and what happens to the session's cards moves it.
//! Replaying the same events always builds the same tasks.

use crate::event::{Event, ReviewVerdict};
use crate::model::{MessageFrom, Millis, Question, QuestionOrigin, Report, ReportKind, TurnSignal};
use crate::pr::{CheckState, PrState, PullRequest};
use crate::view::View;

use super::{Assignee, Blocker, BlockerKind, Guards, Item, Launch, Request, Task, TaskEvent, TaskOptions, TaskState};

/// A launch and a node of the same name go together when the node appeared this close to it.
const LAUNCH_WINDOW: Millis = 10 * 60 * 1000;

/// What an event is about to change that the tasks compare with once it has.
pub(crate) enum Before {
    Nothing,
    Pr(Option<PullRequest>),
    LastPr(u64),
    Reviewing(Vec<u64>),
}

pub fn task_id(session_id: &str) -> String {
    format!("t-{session_id}")
}

fn session_request(session_id: &str) -> String {
    format!("r-{session_id}")
}

fn blocks(q: &Question) -> bool {
    q.blocking && q.status.is_open() && !q.to_orchestrator
}

fn blocker(q: &Question) -> Blocker {
    let kind = match q.origin {
        _ if q.action => BlockerKind::Action,
        QuestionOrigin::Permission | QuestionOrigin::Elicitation => BlockerKind::Permission,
        QuestionOrigin::Ask | QuestionOrigin::AskUserQuestion | QuestionOrigin::Plan | QuestionOrigin::Classifier => BlockerKind::Question,
    };
    Blocker { kind, question: q.id, since: q.asked_at }
}

fn is_document(r: &Report) -> bool {
    r.kind == ReportKind::Document || !crate::artifact::in_report(r).is_empty()
}

fn file_report(task: &mut Task, report: u64, document: bool) {
    if document {
        task.documents.push(report);
    } else {
        task.notices.push(report);
    }
}

fn checks_failed(pr: &PullRequest) -> Option<u64> {
    pr.checks.iter().any(|c| matches!(c.state, CheckState::Failed | CheckState::Running)).then_some(pr.number)
}

impl View {
    pub fn session_task(&self, session_id: &str) -> Option<&Task> {
        self.board.tasks.get(&task_id(session_id))
    }

    fn review_pending(&self, report: u64) -> bool {
        self.reports.get(&report).is_some_and(|r| self.awaits_review(r))
    }

    fn open_prs<'a>(&'a self, task: &'a Task) -> impl Iterator<Item = &'a PullRequest> {
        task.prs.iter().filter_map(|id| self.prs.get(id)).filter(|p| p.state == PrState::Open)
    }

    /// A dependency is ready once its PR is open or it is done: the task builds on top of it.
    fn dependency_ready(&self, task: &Task) -> bool {
        matches!(task.state, TaskState::InReview | TaskState::Closing | TaskState::Done) || self.open_prs(task).next().is_some()
    }

    pub fn task_guards(&self, task: &Task) -> Guards {
        let dependencies = task.depends_on.iter().map(|d| (d, self.board.tasks.get(d)));
        Guards {
            auto_merge: task.options.auto_merge,
            waiting_on: dependencies.clone().find(|(_, t)| t.is_none_or(|t| !self.dependency_ready(t))).map(|(d, _)| d.clone()),
            unmerged_dependency: dependencies.filter_map(|(_, t)| t).flat_map(|t| self.open_prs(t)).map(|p| p.number).next(),
            blocker: task.blocker,
            unmerged_pr: self.open_prs(task).filter(|p| !p.auto_merge).map(|p| p.number).next(),
            unreviewed_document: task.documents.iter().copied().find(|id| self.review_pending(*id)),
            nothing_after_merge: task.final_report.is_some(),
            checks_failed: None,
        }
    }

    /// Moves the task by a fact; true when its state changed.
    fn fact(&mut self, id: &str, event: TaskEvent, at: Millis) -> bool {
        self.fact_with(id, event, at, |_| {})
    }

    fn fact_with(&mut self, id: &str, event: TaskEvent, at: Millis, adjust: impl FnOnce(&mut Guards)) -> bool {
        let Some(task) = self.board.tasks.get(id) else { return false };
        let mut g = self.task_guards(task);
        adjust(&mut g);
        let from = task.state;
        self.board.apply_fact(id, event, &g, at).is_ok_and(|to| to != from)
    }

    fn task_state(&self, id: &str) -> Option<TaskState> {
        self.board.tasks.get(id).map(|t| t.state)
    }

    pub(crate) fn before_tasks(&self, event: &Event) -> Before {
        match event {
            Event::PrOpened { .. } => Before::LastPr(self.prs.keys().next_back().copied().unwrap_or(0)),
            Event::PrUpdated(p) => Before::Pr(self.prs.get(&p.id).cloned()),
            Event::PrMerged { id, .. } | Event::PrDismissed { id } => Before::Pr(self.prs.get(id).cloned()),
            Event::ReportRead { id } | Event::ArtifactReviewed { report: id, .. } => {
                let task = self.reports.get(id).and_then(|r| self.session_task(&r.session_id));
                Before::Reviewing(task.map(|t| t.documents.iter().copied().filter(|d| self.review_pending(*d)).collect()).unwrap_or_default())
            }
            _ => Before::Nothing,
        }
    }

    /// Keeps requests and tasks in step with an event the view has just applied.
    pub(crate) fn derive_tasks(&mut self, at: Millis, event: &Event, before: Before) {
        match event {
            Event::MarkedOrchestrator { session_id } => return self.drop_task(session_id),
            Event::NodeRemoved { .. } => return,
            _ => {}
        }
        if let Some(sid) = event.session_id() {
            self.ensure_task(sid, at);
        }
        let mut moved = vec![];
        let closes_questions = match event {
            Event::Hook(h) => matches!(h.event.as_str(), "PostToolUse" | "PostToolUseFailure"),
            Event::QuestionAnswered { .. } | Event::QuestionClosed { .. } | Event::ProcessExited { .. } | Event::PrUpdated(_) | Event::PrMerged { .. } => true,
            _ => false,
        };
        if closes_questions {
            moved.extend(self.settle_blockers(at));
        }
        match (event, before) {
            (Event::QuestionAsked(q), _) if blocks(q) => {
                let id = task_id(&q.session_id);
                if self.fact(&id, TaskEvent::BlockerRaised { blocker: blocker(q), replaces: false }, at) {
                    moved.push(id);
                }
            }
            (Event::ReportPosted(r), _) => moved.extend(self.report_posted(r.id, at)),
            (Event::MessageHanded { id }, _) => {
                let from_someone = self.messages.get(id).filter(|m| m.from != MessageFrom::Alondra && m.about.is_none() && m.answers_question.is_none());
                if let Some(tid) = from_someone.map(|m| task_id(&m.session_id)) {
                    moved.extend(self.written_to(&tid, Item::Message(*id), at));
                }
            }
            (Event::Hook(h), _) if h.event == "UserPromptSubmit" && h.prompt.is_some() => {
                let typed = self.nodes.get(&h.session_id).is_some_and(|n| crate::states::talking_with_user(self, n));
                if typed {
                    moved.extend(self.written_to(&task_id(&h.session_id), Item::Terminal, at));
                }
            }
            (Event::PrOpened { .. }, Before::LastPr(last)) => {
                let opened: Vec<PullRequest> = self.prs.range(last + 1..).map(|(_, p)| p.clone()).collect();
                for pr in opened {
                    let id = task_id(&pr.session_id);
                    if let Some(task) = self.board.tasks.get_mut(&id) {
                        task.prs.push(pr.id);
                        if self.fact_with(&id, TaskEvent::PrOpened { pr: pr.id }, at, |g| g.auto_merge |= pr.auto_merge) {
                            moved.push(id);
                        }
                    }
                }
            }
            (Event::PrUpdated(p), Before::Pr(Some(old))) => moved.extend(self.pr_changed(&old, p.id, at)),
            (Event::PrMerged { id, .. } | Event::PrDismissed { id }, Before::Pr(Some(old))) => moved.extend(self.pr_changed(&old, *id, at)),
            (Event::PrChangesRequested { id }, _) => {
                if let Some(tid) = self.pr_task(*id)
                    && self.fact(&tid, TaskEvent::ChangesRequested { on: Item::Pr(*id) }, at)
                {
                    moved.push(tid);
                }
            }
            (Event::ArtifactReviewed { report, verdict }, Before::Reviewing(pending)) => moved.extend(self.reviews_closed(pending, Some((*report, *verdict)), at)),
            (Event::ReportRead { .. }, Before::Reviewing(pending)) => moved.extend(self.reviews_closed(pending, None, at)),
            (Event::DaemonSeen(d), _) => self.bind_waiting_launch(&d.session_id),
            (Event::Launched { name, request_id, auto_merge }, _) => {
                self.launched(name, Launch { request_id: request_id.clone(), auto_merge: *auto_merge, at });
            }
            (Event::RequestSent { request_id, options, text, attachments }, _) => {
                let request = self.board.requests.entry(request_id.clone()).or_insert_with(|| Request::new(request_id, at, false));
                request.text = text.clone();
                request.attachments = attachments.clone();
                request.options = Some(options.clone());
            }
            _ => {}
        }
        for id in moved {
            self.raise_waiting_question(&id, at);
        }
    }

    fn new_task(&self, session_id: &str, at: Millis) -> Option<Task> {
        let node = self.nodes.get(session_id).filter(|n| !n.is_orchestrator)?;
        Some(Task {
            id: task_id(session_id),
            request: session_request(session_id),
            title: node.display_name().to_string(),
            brief: node.mission.clone(),
            depends_on: vec![],
            options: TaskOptions::default(),
            assignee: Some(Assignee { session_id: session_id.to_string(), agent_id: None }),
            prs: vec![],
            documents: vec![],
            blocker: None,
            notices: vec![],
            state: TaskState::Running,
            state_since: at,
            created_at: at,
            final_report: None,
            launched: false,
            history: vec![],
            anomalies: vec![],
        })
    }

    /// The session's task, made on its first event, named after its node.
    fn ensure_task(&mut self, session_id: &str, at: Millis) {
        let Some(node) = self.nodes.get(session_id).filter(|n| !n.is_orchestrator) else { return };
        let (title, brief) = (node.display_name(), &node.mission);
        if let Some(task) = self.board.tasks.get_mut(&task_id(session_id)) {
            if task.title != title {
                task.title = title.to_string();
            }
            if brief.is_some() && task.brief != *brief {
                task.brief = brief.clone();
            }
            return;
        }
        if let Some(task) = self.new_task(session_id, at) {
            self.add_task(task, at);
        }
    }

    fn add_task(&mut self, task: Task, at: Millis) {
        self.board.requests.entry(task.request.clone()).or_insert_with(|| Request::new(&task.request, at, true));
        self.board.tasks.insert(task.id.clone(), task);
    }

    fn drop_task(&mut self, session_id: &str) {
        if let Some(task) = self.board.tasks.remove(&task_id(session_id)) {
            self.forget_request_if_unused(&task.request);
        }
    }

    fn forget_request_if_unused(&mut self, id: &str) {
        let used = self.board.tasks.values().any(|t| t.request == id);
        if !used && self.board.requests.get(id).is_some_and(|r| r.synthetic) {
            self.board.requests.remove(id);
        }
    }

    fn pr_task(&self, pr: u64) -> Option<String> {
        let id = task_id(&self.prs.get(&pr)?.session_id);
        self.board.tasks.contains_key(&id).then_some(id)
    }

    /// The blockers whose question is no longer open were answered or withdrawn. Questions a
    /// removed node left open stay: closing a node never touches its task.
    fn settle_blockers(&mut self, at: Millis) -> Vec<String> {
        let settled: Vec<(String, u64)> = self
            .board
            .tasks
            .values()
            .filter_map(|t| {
                let b = t.blocker?;
                let sid = &t.assignee.as_ref()?.session_id;
                let open = self.questions.get(&b.question).is_some_and(|q| q.status.is_open());
                (!open && !self.removed.contains(sid)).then(|| (t.id.clone(), b.question))
            })
            .collect();
        settled.into_iter().filter(|(id, q)| self.fact(id, TaskEvent::Answered { question: *q }, at)).map(|(id, _)| id).collect()
    }

    /// A question the task could not take while it was blocked or finished becomes its blocker
    /// once it can.
    fn raise_waiting_question(&mut self, id: &str, at: Millis) {
        let Some(task) = self.board.tasks.get(id) else { return };
        if task.blocker.is_some() || !matches!(task.state, TaskState::Running | TaskState::InReview | TaskState::Closing) {
            return;
        }
        let Some(sid) = task.assignee.as_ref().map(|a| a.session_id.clone()) else { return };
        let waiting = self.questions.values().find(|q| q.session_id == sid && blocks(q)).map(blocker);
        if let Some(b) = waiting {
            self.fact(id, TaskEvent::BlockerRaised { blocker: b, replaces: false }, at);
        }
    }

    fn report_posted(&mut self, report: u64, at: Millis) -> Vec<String> {
        let Some(r) = self.reports.get(&report).filter(|r| r.kind != ReportKind::Progress) else { return vec![] };
        let (kind, reviewing, id) = (r.kind, self.awaits_review(r), task_id(&r.session_id));
        let document = is_document(r);
        let Some(task) = self.board.tasks.get_mut(&id) else { return vec![] };
        file_report(task, report, document);
        let mut changed = false;
        let answered_on_open_pr = self.board.tasks.get(&id).is_some_and(|t| t.state == TaskState::Running && self.task_guards(t).unmerged_pr.is_some());
        if answered_on_open_pr {
            changed |= self.fact(&id, TaskEvent::NodeUpdated { on: Item::Report(report) }, at);
        }
        if reviewing {
            changed |= self.fact(&id, TaskEvent::DocumentSent { report }, at);
        }
        if kind == ReportKind::Final {
            changed |= self.fact(&id, TaskEvent::FinalReport { report }, at);
        }
        if changed { vec![id] } else { vec![] }
    }

    /// David or the orchestrator wrote to the node: that reopens a done task, and sends one in
    /// review back to work.
    fn written_to(&mut self, id: &str, by: Item, at: Millis) -> Option<String> {
        let event = match self.task_state(id)? {
            TaskState::Done => TaskEvent::Reopen,
            TaskState::InReview => TaskEvent::ChangesRequested { on: by },
            TaskState::WaitingDependency | TaskState::Running | TaskState::NeedsYou | TaskState::Closing | TaskState::Cancelled => return None,
        };
        self.fact(id, event, at).then(|| id.to_string())
    }

    fn pr_changed(&mut self, old: &PullRequest, pr: u64, at: Millis) -> Option<String> {
        let id = self.pr_task(pr)?;
        let now = self.prs.get(&pr)?.clone();
        if let Some(task) = self.board.tasks.get_mut(&id)
            && !task.prs.contains(&pr)
        {
            task.prs.push(pr);
        }
        let pushed = old.fetched && old.head_sha.is_some() && now.head_sha.is_some() && old.head_sha != now.head_sha;
        let working = matches!(self.task_state(&id), Some(TaskState::Running | TaskState::InReview | TaskState::Closing));
        let moved = match (old.state, now.state) {
            (PrState::Merged, _) => false,
            (_, PrState::Merged) if now.auto_merge => self.fact_with(&id, TaskEvent::AutoMerged { pr }, at, |g| g.checks_failed = checks_failed(&now)),
            (_, PrState::Merged) => self.fact(&id, TaskEvent::Merged { item: Item::Pr(pr) }, at),
            (PrState::Open, PrState::Closed | PrState::Dismissed) => self.fact(&id, TaskEvent::ReviewClosed { item: Item::Pr(pr) }, at),
            (PrState::Open, PrState::Open) if pushed && working => self.fact(&id, TaskEvent::NodeUpdated { on: Item::Pr(pr) }, at),
            _ => false,
        };
        moved.then_some(id)
    }

    /// Documents that stopped waiting for David's review: the one he gave his verdict on, and
    /// the others reading it settled.
    fn reviews_closed(&mut self, pending: Vec<u64>, verdict: Option<(u64, ReviewVerdict)>, at: Millis) -> Vec<String> {
        let mut moved = vec![];
        let settled: Vec<u64> = pending.into_iter().filter(|r| !self.review_pending(*r)).collect();
        for report in settled {
            let Some(id) = self.reports.get(&report).map(|r| task_id(&r.session_id)) else { continue };
            let event = match verdict {
                Some((r, ReviewVerdict::Approved)) if r == report => TaskEvent::Merged { item: Item::Report(report) },
                Some((r, ReviewVerdict::Comments)) if r == report => TaskEvent::ChangesRequested { on: Item::Report(report) },
                _ => TaskEvent::ReviewClosed { item: Item::Report(report) },
            };
            if self.fact(&id, event, at) {
                moved.push(id);
            }
        }
        moved
    }

    fn launched(&mut self, name: &str, launch: Launch) {
        let session = self
            .nodes
            .values()
            .filter(|n| n.name.as_deref() == Some(name) && n.created_at >= launch.at - LAUNCH_WINDOW)
            .filter(|n| self.session_task(&n.session_id).is_some_and(|t| !t.launched))
            .max_by_key(|n| (n.created_at, n.session_id.clone()))
            .map(|n| n.session_id.clone());
        match session {
            Some(sid) => self.bind_launch(&sid, launch),
            None => {
                self.board.launches.insert(name.to_string(), launch);
            }
        }
    }

    fn bind_waiting_launch(&mut self, session_id: &str) {
        let Some(node) = self.nodes.get(session_id) else { return };
        let Some(name) = node.name.clone() else { return };
        let fits = self.board.launches.get(&name).is_some_and(|l| node.created_at >= l.at - LAUNCH_WINDOW);
        if fits && self.session_task(session_id).is_some_and(|t| !t.launched)
            && let Some(launch) = self.board.launches.remove(&name)
        {
            self.bind_launch(session_id, launch);
        }
    }

    /// The node was launched for a request: its task joins that request and takes its options.
    fn bind_launch(&mut self, session_id: &str, launch: Launch) {
        let options = launch.request_id.as_ref().and_then(|r| self.requests.get(r)).cloned();
        let Some(task) = self.board.tasks.get_mut(&task_id(session_id)) else { return };
        task.launched = true;
        task.options.auto_merge = launch.auto_merge;
        if let Some(o) = &options {
            task.options.effort = Some(o.effort.clone());
            task.options.project = o.project.clone();
            task.options.issue = o.issue.clone();
        }
        let Some(request) = launch.request_id else { return };
        let old = std::mem::replace(&mut task.request, request.clone());
        self.board.requests.entry(request.clone()).or_insert_with(|| Request { options, ..Request::new(&request, launch.at, false) });
        self.forget_request_if_unused(&old);
    }

    /// Gives a task to every node that has none, from what the view shows: nodes loaded from a
    /// snapshot taken before tasks existed. Called once, right after loading a snapshot.
    pub fn backfill_tasks(&mut self) {
        let missing: Vec<String> = self
            .nodes
            .values()
            .filter(|n| !n.is_orchestrator && self.session_task(&n.session_id).is_none())
            .map(|n| n.session_id.clone())
            .collect();
        for sid in missing {
            let Some(node) = self.nodes.get(&sid) else { continue };
            let (created, since, done, finished) = (node.created_at, node.state_since, node.state == crate::model::NodeState::Done, node.turn_signal == Some(TurnSignal::Finished));
            let Some(mut task) = self.new_task(&sid, created) else { continue };
            task.prs = self.prs.values().filter(|p| p.session_id == sid).map(|p| p.id).collect();
            for r in self.reports.values().filter(|r| r.session_id == sid && r.kind != ReportKind::Progress) {
                file_report(&mut task, r.id, is_document(r));
            }
            task.blocker = self.questions.values().find(|q| q.session_id == sid && blocks(q)).map(blocker);
            let finals = self.reports.values().filter(|r| r.session_id == sid && r.kind == ReportKind::Final);
            task.final_report = finals.map(|r| r.id).max().filter(|_| finished);
            let reviewing = task.prs.iter().filter_map(|id| self.prs.get(id)).any(|p| p.state == PrState::Open && !p.auto_merge && !p.awaiting_push())
                || task.documents.iter().any(|d| self.review_pending(*d));
            task.state = if task.blocker.is_some() {
                TaskState::NeedsYou
            } else if reviewing {
                TaskState::InReview
            } else if done && self.task_guards(&task).unmerged_pr.is_none() {
                TaskState::Done
            } else {
                TaskState::Running
            };
            task.state_since = since;
            self.add_task(task, created);
        }
    }

    /// Tasks worth keeping whatever their age: their node is still here, or something of theirs
    /// still waits for David.
    pub(crate) fn tasks_to_keep(&self) -> std::collections::HashSet<String> {
        self.board
            .tasks
            .values()
            .filter(|t| {
                let node = t.assignee.as_ref().is_some_and(|a| self.nodes.contains_key(&a.session_id));
                let blocker = t.blocker.is_some_and(|b| self.questions.get(&b.question).is_some_and(|q| q.status.is_open()));
                let pr = t.prs.iter().any(|id| self.prs.get(id).is_some_and(|p| p.state == PrState::Open));
                let document = t.documents.iter().any(|d| self.review_pending(*d));
                node || blocker || pr || document
            })
            .map(|t| t.id.clone())
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::{DaemonObservation, HookEvent};
    use crate::model::{Answer, Link, Message, MessageAbout, QuestionStatus, Report};
    use crate::pr::MergeMethod;
    use crate::tasks::RequestState;
    use TaskState::*;

    const PR: &str = "https://github.com/a/b/pull/7";
    const ARTIFACT: &str = "https://claude.ai/code/artifact/plan-1";

    struct Run {
        view: View,
        seq: u64,
    }

    impl Run {
        fn new() -> Self {
            let mut run = Run { view: View::default(), seq: 0 };
            run.on(hook("UserPromptSubmit"));
            run
        }

        fn on(&mut self, e: Event) -> &mut Self {
            self.seq += 1;
            self.view.apply(self.seq, self.seq as i64 * 1000, &e);
            self
        }

        fn task(&self) -> &Task {
            self.view.session_task("s").expect("the node's task")
        }

        fn states(&self) -> Vec<TaskState> {
            self.task().history.iter().map(|h| h.to).collect()
        }

        fn pr(&mut self, f: impl FnOnce(&mut PullRequest)) -> &mut Self {
            let mut pr = self.view.prs[&1].clone();
            pr.fetched = true;
            f(&mut pr);
            self.on(Event::PrUpdated(pr))
        }

        fn message(&mut self, id: u64, from: MessageFrom, about: Option<MessageAbout>) -> &mut Self {
            let m = Message { id, session_id: "s".into(), from, text: "more".into(), answers_question: None, created_at: 0, handed_at: None, delivered_at: None, attachments: vec![], about };
            self.on(Event::MessageQueued(m)).on(Event::MessageHanded { id })
        }
    }

    fn hook(event: &str) -> Event {
        Event::Hook(HookEvent { session_id: "s".into(), event: event.into(), ..Default::default() })
    }

    fn seen(name: &str) -> Event {
        Event::DaemonSeen(DaemonObservation { session_id: "s".into(), short_id: "s".into(), name: Some(name.into()), pid: Some(1), ..Default::default() })
    }

    fn report(id: u64, kind: ReportKind, links: &[&str]) -> Event {
        Event::ReportPosted(Report {
            id,
            session_id: "s".into(),
            node_name: None,
            title: "Ready".into(),
            body: None,
            links: links.iter().map(|u| Link { url: u.to_string(), title: None }).collect(),
            attachments: vec![],
            kind,
            created_at: 0,
            read: false,
            to_orchestrator: false,
            mission: None,
            looks_like_question: false,
            related: vec![],
        })
    }

    fn question(id: u64) -> Event {
        Event::QuestionAsked(Question {
            id,
            session_id: "s".into(),
            node_name: None,
            title: "Deploy?".into(),
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
            related: vec![],
        })
    }

    fn answered(id: u64) -> Event {
        Event::QuestionAnswered { id, answer: Answer { text: "Yes".into(), option: None, by: None, device: None, at: 0 } }
    }

    fn opened() -> Event {
        Event::PrOpened { url: PR.into(), session_id: "s".into() }
    }

    #[test]
    fn a_pr_keeps_the_task_in_review_and_the_merge_finishes_what_the_node_said_was_finished() {
        let mut run = Run::new();
        run.on(opened()).on(report(1, ReportKind::Final, &[PR]));
        assert_eq!(run.task().final_report, Some(1));
        run.on(Event::PrMerged { id: 1, method: MergeMethod::Squash });
        run.message(1, MessageFrom::Alondra, Some(MessageAbout::PrMerged { pr_id: 1 }));
        assert_eq!(run.states(), vec![InReview, InReview, Done], "the merge notice does not reopen it");
        assert_eq!(run.task().prs, vec![1]);
    }

    #[test]
    fn after_a_merge_the_task_closes_until_the_node_reports_final() {
        let mut run = Run::new();
        run.on(opened()).on(Event::PrMerged { id: 1, method: MergeMethod::Merge }).on(question(1)).on(answered(1)).on(report(1, ReportKind::Final, &[]));
        assert_eq!(run.states(), vec![InReview, Closing, NeedsYou, Running, Done]);
    }

    #[test]
    fn changes_requested_send_the_task_back_to_work_until_the_node_pushes_or_answers() {
        let mut pushed = Run::new();
        pushed.on(opened()).pr(|p| p.head_sha = Some("a1".into())).on(Event::PrChangesRequested { id: 1 }).pr(|p| p.head_sha = Some("b2".into()));
        assert_eq!(pushed.states(), vec![InReview, Running, InReview]);

        let mut answered = Run::new();
        answered.on(opened()).on(Event::PrChangesRequested { id: 1 }).on(report(1, ReportKind::Final, &[]));
        assert_eq!(answered.states(), vec![InReview, Running, InReview, InReview], "a final report while the PR is open is an answer, not the end");
        assert!(answered.task().anomalies.is_empty());
    }

    #[test]
    fn an_auto_merge_task_does_not_wait_for_its_pr() {
        let mut run = Run::new();
        run.on(Event::Launched { name: "app/fix".into(), request_id: None, auto_merge: true }).on(seen("app/fix"));
        assert!(run.task().launched && run.task().options.auto_merge);
        run.on(opened()).on(report(1, ReportKind::Final, &[PR])).pr(|p| p.state = PrState::Merged);
        assert_eq!(run.states(), vec![Running, Done, Done]);
        assert!(matches!(run.task().history[2].event, TaskEvent::AutoMerged { pr: 1 }));
    }

    #[test]
    fn a_launch_puts_the_task_in_the_request_david_sent() {
        let options = crate::options::Resolved { project: Some("app".into()), auto_merge: false, effort: "hard".into(), flags: Default::default(), picked: vec![], issue: None };
        let mut run = Run::new();
        run.on(Event::RequestSent { request_id: "r-1".into(), options, text: Some("Fix the menu".into()), attachments: vec![] });
        assert_eq!(run.view.board.request_state("r-1"), RequestState::Defining);
        run.on(seen("app/fix")).on(Event::Launched { name: "app/fix".into(), request_id: Some("r-1".into()), auto_merge: false });
        let task = run.task();
        assert_eq!((task.request.as_str(), task.options.effort.as_deref(), task.options.project.as_deref()), ("r-1", Some("hard"), Some("app")));
        assert_eq!(run.view.board.requests.keys().collect::<Vec<_>>(), vec!["r-1"], "the made-up request is gone");
        assert_eq!(run.view.board.requests["r-1"].text.as_deref(), Some("Fix the menu"));
        assert_eq!(run.view.board.request_state("r-1"), RequestState::InProgress);
        run.on(report(1, ReportKind::Final, &[]));
        assert_eq!(run.view.board.request_state("r-1"), RequestState::Done);
    }

    #[test]
    fn a_document_waits_for_davids_verdict() {
        let mut run = Run::new();
        run.on(report(1, ReportKind::Document, &[ARTIFACT]))
            .on(Event::ArtifactReviewed { report: 1, verdict: ReviewVerdict::Comments })
            .on(report(2, ReportKind::Final, &[ARTIFACT]))
            .on(Event::ArtifactReviewed { report: 2, verdict: ReviewVerdict::Approved });
        assert_eq!(run.states(), vec![InReview, Running, InReview, InReview, Done]);
        assert_eq!(run.task().documents, vec![1, 2]);

        let mut read = Run::new();
        read.on(report(1, ReportKind::Notice, &[ARTIFACT])).on(Event::ReportRead { id: 1 });
        assert_eq!(read.states(), vec![InReview, Running], "read without a verdict");
    }

    #[test]
    fn a_message_from_david_reopens_a_done_task_and_one_from_alondra_does_not() {
        let mut run = Run::new();
        run.on(report(1, ReportKind::Final, &[]));
        run.message(1, MessageFrom::Alondra, None);
        assert_eq!(run.task().state, Done);
        run.message(2, MessageFrom::User, None);
        assert_eq!(run.states(), vec![Done, Running]);
    }

    #[test]
    fn the_orchestrator_has_no_task() {
        let mut run = Run::new();
        run.on(Event::MarkedOrchestrator { session_id: "s".into() }).on(question(1));
        assert!(run.view.board.tasks.is_empty() && run.view.board.requests.is_empty());
    }

    #[test]
    fn closing_a_node_leaves_its_task_as_it_was_until_it_is_forgotten() {
        let mut run = Run::new();
        run.on(question(1)).on(Event::NodeRemoved { session_id: "s".into() }).on(answered(1));
        let task = run.view.board.tasks["t-s"].clone();
        assert_eq!((task.state, task.blocker.map(|b| b.question)), (NeedsYou, Some(1)));
        run.on(Event::Pruned { before: 0 });
        assert!(run.view.board.tasks.contains_key("t-s"), "within the retention window");
        run.on(Event::Pruned { before: 100_000 });
        assert!(run.view.board.tasks.is_empty() && run.view.board.requests.is_empty(), "its node is gone and its question orphaned");
    }

    #[test]
    fn a_task_whose_pr_still_waits_is_not_forgotten() {
        let mut run = Run::new();
        run.on(opened()).on(Event::NodeRemoved { session_id: "s".into() }).on(Event::Pruned { before: 100_000 });
        assert_eq!(run.view.board.tasks["t-s"].state, InReview);
    }
}
