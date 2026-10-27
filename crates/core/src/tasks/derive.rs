//! What moves tasks. Since the daemon records task events (`Event::TasksRecorded`), a task is made
//! by `TaskCreated` and moved only by `TaskMoved`: the daemon runs the rules below on a copy of
//! the board after each event and records what they did, so a replay applies exactly that. Tasks
//! of sessions first seen before then (older logs) are still read from the session's events by
//! the same rules: each session is one request with one task.

use std::collections::BTreeSet;

use crate::event::{Event, ReviewVerdict};
use crate::model::{MessageAbout, MessageFrom, Millis, Question, QuestionOrigin, Report, ReportKind, TurnSignal};
use crate::pr::{CheckState, PrState, PullRequest};
use crate::view::View;

use super::{Assignee, Blocker, BlockerKind, Guards, Item, Launch, NewTask, Request, Task, TaskEvent, TaskOptions, TaskState, Transition};

/// A launch and a node of the same name go together when the node appeared this close to it.
pub const LAUNCH_WINDOW: Millis = 10 * 60 * 1000;

/// Why a node's task moved without a card of it: the node is gone.
pub const NODE_CLOSED: &str = "its node was closed";

/// Why a task that waited for another was cancelled with it.
pub fn dependency_cancelled(dependency: &str) -> String {
    format!("{dependency} was cancelled")
}

/// Which tasks the rules move in the pass under way: the ones read from events, or, on a copy
/// of the board, the recorded ones, keeping each move to be recorded.
#[derive(Clone, Debug, Default, PartialEq)]
pub enum Pass {
    #[default]
    Read,
    Record(Vec<Event>),
}

/// What an event is about to change that the tasks compare with once it has.
pub enum Before {
    Nothing,
    Pr(Option<PullRequest>),
    /// The newest card before a PR was reported, and the card of its URL that had no task yet.
    Opened { last: u64, orphan: Option<u64> },
    /// The card before GitHub's news, and whether it had no task yet.
    Updated { old: Option<PullRequest>, orphan: bool },
    Reviewing(Vec<u64>),
    /// The task a PR was on before it was moved.
    Reassigned(Option<String>),
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
    Blocker { kind, question: q.id, since: q.asked_at, asked: q.origin == QuestionOrigin::Ask, about_pr: q.about_pr }
}

fn is_document(r: &Report) -> bool {
    r.kind == ReportKind::Document || !crate::artifact::in_report(r).is_empty()
}

/// A task's own name lets it run over 80 characters before it is cut on a word: long enough for
/// a whole line David typed, short enough for a card's title.
const TASK_TITLE_MAX: usize = 80;

/// A line of free text, cut to its first sentence and capped on a word, for a task title made
/// from David's own words rather than chosen for him.
fn title_from_text(line: &str) -> String {
    let first = line.split(". ").next().unwrap_or(line).trim();
    if first.chars().count() <= TASK_TITLE_MAX {
        return first.to_string();
    }
    let cut: String = first.chars().take(TASK_TITLE_MAX).collect();
    let cut = cut.rsplit_once(' ').map(|(a, _)| a).unwrap_or(&cut);
    format!("{cut}…")
}

/// The title a task falls back to until something has named it: the orchestrator's short name
/// for its request if it gave one, else — when David wrote that request himself, not one made up
/// to hold a bare session — the first line of what he asked, else the node's own name, which is
/// the dead end t-47118 got stuck with (a task titled after its node, indistinguishable from it
/// in the list of nodes).
fn fallback_title(request: Option<&Request>, node_name: &str) -> String {
    let Some(r) = request else { return node_name.to_string() };
    if let Some(title) = &r.title {
        return title.clone();
    }
    if !r.synthetic
        && let Some(text) = &r.text
        && let Some(line) = text.lines().find(|l| !l.trim().is_empty())
    {
        return title_from_text(line);
    }
    node_name.to_string()
}

fn file_report(task: &mut Task, report: u64, document: bool) {
    let list = if document { &mut task.documents } else { &mut task.notices };
    if !list.contains(&report) {
        list.push(report);
    }
}

fn about_pr(event: &TaskEvent, pr: u64) -> bool {
    match event {
        TaskEvent::PrOpened { pr: p } | TaskEvent::AutoMerged { pr: p } => *p == pr,
        TaskEvent::ChangesRequested { on } | TaskEvent::NodeUpdated { on } => *on == Item::Pr(pr),
        TaskEvent::Merged { item } | TaskEvent::ReviewClosed { item } => *item == Item::Pr(pr),
        _ => false,
    }
}

/// The task ids a text mentions, each with whether it follows “Alondra task:”. An id starts a
/// word (`at-12` mentions none) and runs to the end of it, so a branch `t-12-fix` mentions
/// `t-12-fix`, which `View::named_task` shortens to the task it finds.
fn task_mentions(text: &str) -> Vec<(&str, bool)> {
    const MARKER: &str = "alondra task:";
    let mut found = vec![];
    let mut from = 0;
    while let Some(i) = text[from..].find("t-").map(|i| from + i) {
        from = i + 2;
        let starts_word = text[..i].chars().next_back().is_none_or(|c| !(c.is_alphanumeric() || c == '-' || c == '_'));
        let len = text[from..].find(|c: char| !(c.is_ascii_alphanumeric() || c == '-')).unwrap_or(text.len() - from);
        if !starts_word || len == 0 {
            continue;
        }
        let before = text[..i].trim_end_matches([' ', '\t', '`', '*', '"', '\'']);
        let cut = before.len().saturating_sub(MARKER.len());
        let marked = before.len() >= MARKER.len() && before.is_char_boundary(cut) && before[cut..].eq_ignore_ascii_case(MARKER);
        found.push((text[i..from + len].trim_end_matches('-'), marked));
    }
    found
}

fn checks_failed(pr: &PullRequest) -> Option<u64> {
    pr.checks.iter().any(|c| matches!(c.state, CheckState::Failed | CheckState::Running)).then_some(pr.number)
}

impl View {
    /// The tasks a node works on.
    pub fn tasks_of<'a>(&'a self, session_id: &str) -> impl Iterator<Item = &'a Task> + use<'a> {
        let session_id = session_id.to_string();
        self.board.tasks.values().filter(move |t| t.is_assigned_to(&session_id))
    }

    /// The task a node works on now: its newest open one, else its newest.
    pub fn session_task(&self, session_id: &str) -> Option<&Task> {
        let newest = |t: &&Task| (t.created_at, t.id.clone());
        let mine = || self.board.tasks.values().filter(|t| t.is_assigned_to(session_id));
        mine().filter(|t| !t.state.finished()).max_by_key(newest).or_else(|| mine().max_by_key(newest))
    }

    /// The task a card names, or for a card that names none, its session's own or current one.
    fn card_task_id(&self, named: Option<&String>, session_id: &str) -> Option<String> {
        match named {
            Some(id) => self.board.tasks.get(id).filter(|t| t.is_assigned_to(session_id)).map(|t| t.id.clone()),
            None => self.single_task(session_id).map(str::to_string),
        }
    }

    pub fn question_task(&self, q: &Question) -> Option<&Task> {
        self.card_task_id(q.task.as_ref(), &q.session_id).and_then(|id| self.board.tasks.get(&id))
    }

    pub fn report_task(&self, r: &Report) -> Option<&Task> {
        self.card_task_id(r.task.as_ref(), &r.session_id).and_then(|id| self.board.tasks.get(&id))
    }

    /// The task a PR card is for: the one it was given, or for a card from before cards carried
    /// one, the task that lists it. None while nothing said which: Alondra asked the node.
    pub fn pr_task_of(&self, p: &PullRequest) -> Option<&Task> {
        match &p.task {
            Some(id) => self.board.tasks.get(id),
            None => self.board.tasks.values().find(|t| t.prs.contains(&p.id)),
        }
    }

    fn pr_task(&self, pr: u64) -> Option<String> {
        self.pr_task_of(self.prs.get(&pr)?).map(|t| t.id.clone())
    }

    /// A launched task no node has taken yet that the node of this session goes with.
    fn waiting_launch(&self, session_id: &str) -> Option<&Task> {
        let node = self.nodes.get(session_id)?;
        let name = node.name.as_deref()?;
        self.board
            .tasks
            .values()
            .filter(|t| t.recorded && t.launched && t.assignee.is_none() && t.launch_name() == name && node.created_at >= t.created_at - LAUNCH_WINDOW)
            .max_by_key(|t| (t.created_at, t.id.clone()))
    }

    /// The open task a subagent of the node works on: assigned to it, or the one it bound itself
    /// to (`activity`).
    fn agent_task(&self, session_id: &str, agent_id: &str) -> Option<&Task> {
        let bound = self.nodes.get(session_id).and_then(|n| n.ledger.bound(agent_id));
        self.tasks_of(session_id)
            .filter(|t| !t.state.finished())
            .find(|t| t.assignee.as_ref().and_then(|a| a.agent_id.as_deref()) == Some(agent_id) || bound == Some(t.id.as_str()))
    }

    /// The task a PR the node opened is for, in this order: the task its title, body or branch
    /// names (“Alondra task: t-…”) for the main thread, the task of the subagent that opened it, the node's live
    /// `focus`, the node's only open task (its only task when none is open). None when the node
    /// works on several and nothing says which: Alondra asks the node instead of guessing. 25 Sep
    /// 00:01: the main thread of 7e661a50 opened #98 and #99 for its review, and both went to
    /// t-44370, its newest task.
    pub fn pr_task_for(&self, session_id: &str, agent_id: Option<&str>, texts: &[&str], now: Millis) -> Option<String> {
        if self.nodes.get(session_id).is_some_and(|n| n.is_orchestrator) {
            return None;
        }
        agent_id.and_then(|a| self.agent_task(session_id, a)).map(|t| t.id.clone())
            .or_else(|| self.named_task(session_id, texts))
            .or_else(|| agent_id.is_none().then(|| self.focus(session_id, now)).flatten().map(str::to_string))
            .or_else(|| self.single_task(session_id).map(str::to_string))
    }

    /// The node's task a mention names: `t-12-fix` names `t-12` when that one is the node's.
    pub(crate) fn mentioned_task(&self, session_id: &str, mention: &str) -> Option<&Task> {
        let mut id = mention;
        loop {
            if let Some(t) = self.board.tasks.get(id).filter(|t| t.is_assigned_to(session_id)) {
                return Some(t);
            }
            id = &id[..id.rfind('-').filter(|&i| i > 1)?];
        }
    }

    /// The node's task a PR's text names: the one after “Alondra task:”, else the only open task
    /// it mentions.
    fn named_task(&self, session_id: &str, texts: &[&str]) -> Option<String> {
        let named: Vec<(&Task, bool)> =
            texts.iter().flat_map(|t| task_mentions(t)).filter_map(|(id, marked)| self.mentioned_task(session_id, id).map(|t| (t, marked))).collect();
        if let Some((t, _)) = named.iter().find(|(_, marked)| *marked) {
            return Some(t.id.clone());
        }
        let open: BTreeSet<&str> = named.iter().filter(|(t, _)| !t.state.finished()).map(|(t, _)| t.id.as_str()).collect();
        match (open.first(), open.len()) {
            (Some(id), 1) => Some(id.to_string()),
            _ => None,
        }
    }

    /// The task GitHub's copy of a PR names in its title, body or branch.
    fn marked_task(&self, pr: u64) -> Option<String> {
        let p = self.prs.get(&pr)?;
        self.named_task(&p.session_id, &[&p.title, p.body.as_deref().unwrap_or_default(), p.head.as_deref().unwrap_or_default()])
    }

    fn untasked_pr(&self, url: &str) -> Option<u64> {
        let (repo, number) = crate::pr::parse_url(url)?;
        self.prs.values().find(|p| p.repo == repo && p.number == number && self.pr_task_of(p).is_none()).map(|p| p.id)
    }

    /// Whether the task can take the PR card: it is a task of the node that opened it.
    pub fn may_take_pr(&self, task: &str, pr: u64) -> bool {
        let opener = self.prs.get(&pr).map(|p| p.session_id.as_str());
        self.board.tasks.get(task).is_some_and(|t| opener.is_some_and(|s| t.is_assigned_to(s)))
    }

    fn review_pending(&self, report: u64) -> bool {
        self.reports.get(&report).is_some_and(|r| self.awaits_review(r))
    }

    pub(crate) fn open_prs<'a>(&'a self, task: &'a Task) -> impl Iterator<Item = &'a PullRequest> {
        task.prs.iter().filter_map(|id| self.prs.get(id)).filter(|p| p.state == PrState::Open)
    }

    /// A dependency is ready once its PR is open or it is done: the task builds on top of it.
    fn dependency_ready(&self, task: &Task) -> bool {
        matches!(task.state, TaskState::InReview | TaskState::Closing | TaskState::Done) || self.open_prs(task).next().is_some()
    }

    pub fn task_guards(&self, task: &Task) -> Guards {
        let dependencies = task.depends_on.iter().map(|d| (d, self.board.tasks.get(d)));
        let unready = dependencies.clone().filter(|(_, t)| t.is_none_or(|t| !self.dependency_ready(t)));
        Guards {
            auto_merge: task.options.auto_merge,
            waiting_on: unready.clone().map(|(d, _)| d.clone()).next(),
            cancelled_dependency: unready.filter(|(_, t)| t.is_some_and(|t| t.state == TaskState::Cancelled)).map(|(d, _)| d.clone()).next(),
            unmerged_dependency: dependencies.filter_map(|(_, t)| t).flat_map(|t| self.open_prs(t)).map(|p| p.number).next(),
            started: task.awaiting.is_some(),
            awaited_merge: task.awaiting.and_then(|id| self.prs.get(&id)).filter(|p| p.state == PrState::Open).map(|p| p.number),
            blocker: task.blocker,
            unmerged_pr: self.open_prs(task).filter(|p| !p.auto_merge).map(|p| p.number).next(),
            unreviewed_document: task.documents.iter().copied().find(|id| self.review_pending(*id)),
            nothing_after_merge: task.final_report.is_some() || self.assignee_gone(task),
            checks_failed: None,
        }
    }

    /// The node that worked on the task was closed or forgotten: nobody is left to do what a
    /// merge would unblock.
    fn assignee_gone(&self, task: &Task) -> bool {
        task.assignee.as_ref().is_some_and(|a| self.removed.contains_key(&a.session_id) || !self.nodes.contains_key(&a.session_id))
    }

    /// The guards for one event: a PR Alondra merges on its own counts for the PR it is about.
    fn guards_for(&self, task: &Task, event: &TaskEvent) -> Guards {
        let mut g = self.task_guards(task);
        match event {
            TaskEvent::PrOpened { pr } => g.auto_merge |= self.prs.get(pr).is_some_and(|p| p.auto_merge),
            TaskEvent::AutoMerged { pr } => g.checks_failed = self.prs.get(pr).and_then(checks_failed),
            _ => {}
        }
        g
    }

    /// Applies a task event with the guards the view gives it now; true when the state changed.
    pub(crate) fn apply_task_event(&mut self, id: &str, event: TaskEvent, at: Millis) -> bool {
        let Some(task) = self.board.tasks.get(id) else { return false };
        let g = self.guards_for(task, &event);
        let from = task.state;
        self.board.apply_fact(id, event, &g, at).is_ok_and(|to| to != from)
    }

    fn recording(&self) -> bool {
        matches!(self.task_pass, Pass::Record(_))
    }

    fn keep(&mut self, event: Event) {
        if let Pass::Record(out) = &mut self.task_pass {
            out.push(event);
        }
    }

    /// Moves the task by a fact, when it is one this pass moves; true when its state changed.
    fn fact(&mut self, id: &str, event: TaskEvent, at: Millis) -> bool {
        let Some(task) = self.board.tasks.get(id) else { return false };
        if task.recorded != self.recording() {
            return false;
        }
        self.keep(Event::TaskMoved { task: id.to_string(), event: event.clone() });
        self.apply_task_event(id, event, at)
    }

    fn task_state(&self, id: &str) -> Option<TaskState> {
        self.board.tasks.get(id).map(|t| t.state)
    }

    pub fn before_tasks(&self, event: &Event) -> Before {
        match event {
            Event::PrOpened { url, task, .. } => Before::Opened {
                last: self.prs.keys().next_back().copied().unwrap_or(0),
                orphan: task.as_ref().and_then(|_| self.untasked_pr(url)),
            },
            Event::PrUpdated(p) => {
                let old = self.prs.get(&p.id).cloned();
                let orphan = old.as_ref().is_some_and(|o| self.pr_task_of(o).is_none());
                Before::Updated { old, orphan }
            }
            Event::PrReassigned { pr, .. } => Before::Reassigned(self.pr_task(*pr)),
            Event::PrMerged { id, .. } | Event::PrDismissed { id } => Before::Pr(self.prs.get(id).cloned()),
            Event::ReportRead { id } | Event::ArtifactReviewed { report: id, .. } => {
                let task = self.reports.get(id).and_then(|r| self.report_task(r));
                Before::Reviewing(task.map(|t| t.documents.iter().copied().filter(|d| self.review_pending(*d)).collect()).unwrap_or_default())
            }
            _ => Before::Nothing,
        }
    }

    /// Keeps a task's lists and ownership in step with an event the view has just applied, for
    /// every task: which reports and PRs it has, and which session works on it.
    fn file_in_tasks(&mut self, at: Millis, event: &Event, before: &Before) {
        match (event, before) {
            (Event::TasksRecorded, _) => {
                self.board.recorded_since.get_or_insert(at);
            }
            (Event::TaskCreated(t), _) => self.board.create(t, at),
            (Event::TaskAssigned { task, session_id, agent_id }, _) => {
                if let Some(t) = self.board.tasks.get_mut(task) {
                    t.assignee = Some(Assignee { session_id: session_id.clone(), agent_id: agent_id.clone() });
                    t.taken(at);
                }
            }
            (Event::TaskReleased { task }, _) => {
                if let Some(t) = self.board.tasks.get_mut(task) {
                    t.released(at);
                }
            }
            (Event::TaskMoved { task, event }, _) => {
                self.apply_task_event(task, event.clone(), at);
            }
            (Event::Launched { name, task: Some(task), title, .. }, _) => {
                if let Some(t) = self.board.tasks.get_mut(task) {
                    t.launched = true;
                    t.launched_at = Some(at);
                    t.launched_as = Some(name.clone());
                    t.assignee = None;
                    t.taken(at);
                    if let Some(title) = title.as_deref().map(str::trim).filter(|t| !t.is_empty()) {
                        t.title = title.to_string();
                    }
                }
            }
            (Event::RequestTitled { request_id, title }, _) => {
                if let Some(r) = self.board.requests.get_mut(request_id) {
                    r.title = Some(title.clone());
                }
            }
            (Event::TaskTitled { task, title }, _) => {
                if let Some(t) = self.board.tasks.get_mut(task) {
                    t.title = title.clone();
                    t.titled = true;
                }
            }
            (Event::RequestClosed { request_id, answer, withdrawn }, _) => {
                if let Some(r) = self.board.requests.get_mut(request_id) {
                    r.closed = Some(super::RequestClosure { answer: answer.clone(), withdrawn: *withdrawn, at });
                }
            }
            (Event::MarkedOrchestrator { session_id }, _) => self.drop_tasks(session_id),
            (Event::RequestSent { request_id, options, text, attachments }, _) => {
                let request = self.board.requests.entry(request_id.clone()).or_insert_with(|| Request::new(request_id, at, false));
                request.text = text.clone();
                request.attachments = attachments.clone();
                request.options = Some(options.clone());
            }
            (Event::ReportPosted(r), _) if r.kind != ReportKind::Progress => {
                let Some(id) = self.card_task_id(r.task.as_ref(), &r.session_id) else { return };
                let document = is_document(r);
                if let Some(task) = self.board.tasks.get_mut(&id) {
                    file_report(task, r.id, document);
                }
                if !r.to_orchestrator {
                    self.bring_back_parked_prs(&id);
                }
            }
            (Event::PrOpened { session_id, task, .. }, Before::Opened { last, orphan }) => {
                let named = task.clone().filter(|t| self.board.tasks.contains_key(t));
                let opened: Vec<u64> = self.prs.range(last + 1..).map(|(id, _)| *id).collect();
                let id = named.clone().or_else(|| self.pr_task_for(session_id, None, &[], at));
                for pr in opened {
                    self.put_pr_in_task(pr, id.clone());
                }
                if let Some(pr) = *orphan {
                    let fits = named.filter(|t| self.may_take_pr(t, pr));
                    self.put_pr_in_task(pr, fits);
                }
            }
            (Event::PrUpdated(p), _) => {
                let task = self.pr_task(p.id).or_else(|| self.marked_task(p.id));
                self.put_pr_in_task(p.id, task);
            }
            (Event::PrReassigned { pr, task }, Before::Reassigned(from)) => self.reassign_pr(*pr, from.as_deref(), task),
            _ => {}
        }
    }

    fn put_pr_in_task(&mut self, pr: u64, task: Option<String>) {
        let Some(id) = task else { return };
        let auto_merge = self.board.tasks.get(&id).is_some_and(|t| t.options.auto_merge);
        if let Some(p) = self.prs.get_mut(&pr) {
            p.task.get_or_insert_with(|| id.clone());
            p.auto_merge |= auto_merge;
        }
        if let Some(t) = self.board.tasks.get_mut(&id)
            && !t.prs.contains(&pr)
        {
            t.prs.push(pr);
        }
    }

    /// The node answered while its PR waited for a push: the card is David's to review again.
    fn bring_back_parked_prs(&mut self, task: &str) {
        let Some(prs) = self.board.tasks.get(task).map(|t| t.prs.clone()) else { return };
        for id in prs {
            if let Some(p) = self.prs.get_mut(&id).filter(|p| p.state == PrState::Open && p.awaiting_push()) {
                p.changes_requested_on = None;
            }
        }
    }

    /// Keeps the tasks read from events in step with an event the view has just applied.
    pub(crate) fn derive_tasks(&mut self, at: Millis, event: &Event, before: Before) {
        if self.board.recorded_since.is_none()
            && !matches!(event, Event::NodeRemoved { .. } | Event::MarkedOrchestrator { .. })
            && let Some(sid) = event.session_id()
        {
            self.ensure_task(sid, at);
        }
        if let Some(sid) = event.session_id() {
            self.follow_node(sid);
        }
        self.file_in_tasks(at, event, &before);
        self.move_tasks(at, event, before);
    }

    /// The rules: what each event does to the tasks this pass moves.
    fn move_tasks(&mut self, at: Millis, event: &Event, before: Before) {
        let mut moved = match event {
            Event::NodeRemoved { session_id } => self.node_closed(session_id, at),
            _ => self.moved_by(at, event, before),
        };
        moved.extend(self.dependencies_cancelled(at));
        moved.extend(self.dependencies_ready(at));
        for id in moved {
            self.raise_waiting_question(&id, at);
        }
    }

    fn moved_by(&mut self, at: Millis, event: &Event, before: Before) -> Vec<String> {
        let mut moved = vec![];
        let closes_questions = match event {
            Event::Hook(h) => matches!(h.event.as_str(), "PostToolUse" | "PostToolUseFailure" | "SessionEnd"),
            Event::QuestionAnswered { .. } | Event::QuestionClosed { .. } | Event::ProcessExited { .. } | Event::PrUpdated(_) | Event::PrMerged { .. } => true,
            _ => false,
        };
        if closes_questions {
            moved.extend(self.settle_blockers(at));
        }
        match (event, before) {
            (Event::QuestionAsked(q), _) if blocks(q) => {
                if let Some(id) = self.card_task_id(q.task.as_ref(), &q.session_id) {
                    let current = self.board.tasks.get(&id).and_then(|t| t.blocker).map(|b| b.question);
                    let replaces = q.replaces.is_some() && q.replaces == current;
                    if self.fact(&id, TaskEvent::BlockerRaised { blocker: blocker(q), replaces }, at) {
                        moved.push(id);
                    }
                }
            }
            (Event::ReportPosted(r), _) => moved.extend(self.report_posted(r.id, at)),
            (Event::MessageHanded { id }, _) => {
                let from_someone = self.messages.get(id).filter(|m| m.from != MessageFrom::Alondra && m.answers_question.is_none());
                let task = from_someone.and_then(|m| match &m.about {
                    None => self.single_task(&m.session_id).map(str::to_string),
                    Some(MessageAbout::Task { task }) => self.board.tasks.get(task).filter(|t| t.is_assigned_to(&m.session_id)).map(|t| t.id.clone()),
                    Some(_) => None,
                });
                if let Some(tid) = task {
                    moved.extend(self.written_to(&tid, Item::Message(*id), at));
                }
            }
            (Event::Hook(h), _) if h.event == "UserPromptSubmit" && h.prompt.is_some() => {
                let typed = self.nodes.get(&h.session_id).is_some_and(|n| crate::states::talking_with_user(self, n));
                let task = self.single_task(&h.session_id).map(str::to_string).filter(|_| typed);
                if let Some(tid) = task {
                    moved.extend(self.written_to(&tid, Item::Terminal, at));
                }
            }
            (Event::PrOpened { .. }, Before::Opened { last, orphan }) => {
                let opened: Vec<u64> = self.prs.range(last + 1..).map(|(id, _)| *id).collect();
                for pr in opened {
                    let Some(id) = self.pr_task(pr) else { continue };
                    let reopens = self.own_pr_on_done_task(&id, pr) && self.fact(&id, TaskEvent::Reopen, at);
                    if self.fact(&id, TaskEvent::PrOpened { pr }, at) || reopens {
                        moved.push(id);
                    }
                }
                if let Some(pr) = orphan {
                    moved.extend(self.pr_joined(pr, at));
                }
            }
            (Event::PrUpdated(p), Before::Updated { orphan: true, .. }) => moved.extend(self.pr_joined(p.id, at)),
            (Event::PrUpdated(p), Before::Updated { old: Some(old), .. }) => moved.extend(self.pr_changed(&old, p.id, at)),
            (Event::PrReassigned { pr, task }, Before::Reassigned(from)) if from.as_ref() != Some(task) && self.pr_task(*pr).as_ref() == Some(task) => {
                moved.extend(from);
                moved.extend(self.pr_joined(*pr, at));
            }
            (Event::PrMerged { id, .. } | Event::PrDismissed { id }, Before::Pr(Some(old))) => moved.extend(self.pr_changed(&old, *id, at)),
            (Event::PrChangesRequested { id } | Event::PrAsked { id }, _) => {
                if let Some(tid) = self.pr_task(*id)
                    && self.fact(&tid, TaskEvent::ChangesRequested { on: Item::Pr(*id) }, at)
                {
                    moved.push(tid);
                }
            }
            (Event::ArtifactReviewed { report, verdict }, Before::Reviewing(pending)) => moved.extend(self.reviews_closed(pending, Some((*report, *verdict)), at)),
            (Event::ReportRead { .. }, Before::Reviewing(pending)) => moved.extend(self.reviews_closed(pending, None, at)),
            (Event::DaemonSeen(d), _) => self.bind_launch_of(&d.session_id),
            (Event::Launched { name, request_id, auto_merge, task, title, .. }, _) => match task {
                Some(task) => self.launched_for(task),
                None => self.launched(name, title.as_deref(), Launch { request_id: request_id.clone(), auto_merge: *auto_merge, at }),
            },
            _ => {}
        }
        moved
    }

    /// A PR that joins its task late (a report named the task, GitHub's copy names it, or David
    /// moved it there) brings what already happened to it: it was opened, and merged or closed if
    /// it was. A finished task that gets a merged or closed PR only lists it.
    fn pr_joined(&mut self, pr: u64, at: Millis) -> Option<String> {
        let id = self.pr_task(pr)?;
        let (state, auto_merged) = self.prs.get(&pr).map(|p| (p.state, p.auto_merge))?;
        let mut moved = state == PrState::Open && self.own_pr_on_done_task(&id, pr) && self.fact(&id, TaskEvent::Reopen, at);
        if self.task_state(&id).is_none_or(|s| s.finished() || s == TaskState::WaitingDependency) {
            return moved.then_some(id);
        }
        moved |= self.fact(&id, TaskEvent::PrOpened { pr }, at);
        moved |= match state {
            PrState::Merged if auto_merged => self.fact(&id, TaskEvent::AutoMerged { pr }, at),
            PrState::Merged => self.fact(&id, TaskEvent::Merged { item: Item::Pr(pr) }, at),
            PrState::Closed | PrState::Dismissed => self.fact(&id, TaskEvent::ReviewClosed { item: Item::Pr(pr) }, at),
            PrState::Open | PrState::Unknown => false,
        };
        moved.then_some(id)
    }

    /// David or the orchestrator said which task a PR is for. It leaves the task it was given,
    /// which goes back to where its own facts leave it; the task it joins takes it in `moved_by`.
    fn reassign_pr(&mut self, pr: u64, from: Option<&str>, to: &str) {
        if from == Some(to) || !self.may_take_pr(to, pr) {
            return;
        }
        if let Some(p) = self.prs.get_mut(&pr) {
            p.task = None;
        }
        if let Some(from) = from {
            if let Some(t) = self.board.tasks.get_mut(from) {
                t.prs.retain(|id| *id != pr);
            }
            self.drop_pr_facts(from, pr);
        }
        self.put_pr_in_task(pr, Some(to.to_string()));
    }

    /// Takes the facts of a PR that was never the task's out of it: its history up to the PR's
    /// first fact stays, and the facts after it are applied again without the PR's, so the task
    /// is where its own facts leave it. A fact the machine no longer takes is left out.
    fn drop_pr_facts(&mut self, id: &str, pr: u64) {
        let Some(task) = self.board.tasks.get_mut(id) else { return };
        task.anomalies.retain(|a| !about_pr(&a.event, pr));
        let Some(first) = task.history.iter().position(|h| about_pr(&h.event, pr)) else { return };
        let later: Vec<Transition> = task.history.split_off(first);
        task.state = later[0].from;
        task.state_since = task.history.iter().rev().find(|h| h.from != h.to).map_or(task.created_at, |h| h.at);
        task.rehold();
        for h in later.into_iter().filter(|h| !about_pr(&h.event, pr)) {
            let Some(task) = self.board.tasks.get(id) else { return };
            let g = self.guards_for(task, &h.event);
            if super::transition(task.state, &h.event, &g).is_ok() {
                let _ = self.board.apply_fact(id, h.event, &g, h.at);
            }
        }
        if let Some(t) = self.board.tasks.get_mut(id).filter(|t| !t.state.finished()) {
            t.archived_at = None;
        }
    }

    /// The node that works on a done task opened a new PR for it: the task has work again.
    fn own_pr_on_done_task(&self, id: &str, pr: u64) -> bool {
        let opener = self.prs.get(&pr).map(|p| p.session_id.as_str());
        self.board.tasks.get(id).is_some_and(|t| t.state == TaskState::Done && opener.is_some_and(|s| t.is_assigned_to(s)))
    }

    fn new_task(&self, session_id: &str, at: Millis) -> Option<Task> {
        let node = self.nodes.get(session_id).filter(|n| !n.is_orchestrator)?;
        let request = session_request(session_id);
        let title = fallback_title(self.board.requests.get(&request), node.display_name());
        Some(Task {
            id: task_id(session_id),
            request,
            title,
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
            awaiting: None,
            launched: false,
            launched_at: None,
            launched_as: None,
            start: None,
            recorded: false,
            titled: false,
            history: vec![],
            anomalies: vec![],
            archived_at: None,
        })
    }

    /// The session's task, read from its first event, named after its node.
    fn ensure_task(&mut self, session_id: &str, at: Millis) {
        if self.board.tasks.contains_key(&task_id(session_id)) {
            return;
        }
        if let Some(task) = self.new_task(session_id, at) {
            self.add_task(task, at);
        }
    }

    /// A session's own task goes by its node's name until something gives it a real title (the
    /// orchestrator's name for its request, David's own words, or an explicit `TaskTitled`), and
    /// always carries what the daemon says the node does.
    fn follow_node(&mut self, session_id: &str) {
        let Some(node) = self.nodes.get(session_id).filter(|n| !n.is_orchestrator) else { return };
        let (name, brief) = (node.display_name().to_string(), node.mission.clone());
        let id = task_id(session_id);
        let Some(current) = self.board.tasks.get(&id) else { return };
        let title = (!current.titled).then(|| fallback_title(self.board.requests.get(&current.request), &name)).filter(|t| *t != current.title);
        let Some(task) = self.board.tasks.get_mut(&id) else { return };
        if let Some(title) = title {
            task.title = title;
        }
        if brief.is_some() && task.brief != brief {
            task.brief = brief;
        }
    }

    fn add_task(&mut self, task: Task, at: Millis) {
        self.board.requests.entry(task.request.clone()).or_insert_with(|| Request::new(&task.request, at, true));
        self.board.tasks.insert(task.id.clone(), task);
    }

    /// The orchestrator works on no task.
    pub(crate) fn drop_tasks(&mut self, session_id: &str) {
        let ids: Vec<String> = self.board.tasks.values().filter(|t| t.is_assigned_to(session_id) || t.id == task_id(session_id)).map(|t| t.id.clone()).collect();
        for id in ids {
            if let Some(task) = self.board.tasks.remove(&id) {
                self.forget_request_if_unused(&task.request);
            }
        }
    }

    fn forget_request_if_unused(&mut self, id: &str) {
        let used = self.board.tasks.values().any(|t| t.request == id);
        if !used && self.board.requests.get(id).is_some_and(|r| r.synthetic) {
            self.board.requests.remove(id);
        }
    }

    /// A node closed before it finished: what it was doing is dropped, unless a PR of it still
    /// waits for David's merge. Then the question it left open no longer holds the task, which
    /// waits for David on that PR.
    fn node_closed(&mut self, session_id: &str, at: Millis) -> Vec<String> {
        let open: Vec<(String, TaskState, Option<u64>, Option<u64>)> = self
            .tasks_of(session_id)
            .filter(|t| matches!(t.state, TaskState::Running | TaskState::NeedsYou | TaskState::Closing))
            .map(|t| (t.id.clone(), t.state, self.open_prs(t).next().map(|p| p.id), t.blocker.map(|b| b.question)))
            .collect();
        let mut moved = vec![];
        for (id, state, pr, question) in open {
            let changed = match (state, pr) {
                (_, None) => self.fact(&id, TaskEvent::Cancel { reason: Some(NODE_CLOSED.to_string()) }, at),
                (TaskState::NeedsYou, Some(pr)) => {
                    let settled = question.is_some_and(|q| self.fact(&id, TaskEvent::Answered { question: q }, at));
                    self.fact(&id, TaskEvent::NodeUpdated { on: Item::Pr(pr) }, at) || settled
                }
                (_, Some(_)) => false,
            };
            if changed {
                moved.push(id);
            }
        }
        moved
    }

    /// Tasks waiting for a dependency that was cancelled before it was ready can never start:
    /// they are cancelled too, and so on down the tree.
    fn dependencies_cancelled(&mut self, at: Millis) -> Vec<String> {
        let mut moved = vec![];
        loop {
            let dropped: Vec<(String, String)> = self
                .board
                .tasks
                .values()
                .filter(|t| t.state == TaskState::WaitingDependency && t.recorded == self.recording())
                .filter_map(|t| Some((t.id.clone(), self.task_guards(t).cancelled_dependency?)))
                .collect();
            let cancelled: Vec<String> =
                dropped.into_iter().filter(|(id, d)| self.fact(id, TaskEvent::Cancel { reason: Some(dependency_cancelled(d)) }, at)).map(|(id, _)| id).collect();
            if cancelled.is_empty() {
                return moved;
            }
            moved.extend(cancelled);
        }
    }

    /// The blockers whose question is no longer open were answered or withdrawn. Questions a
    /// removed node left open stay.
    fn settle_blockers(&mut self, at: Millis) -> Vec<String> {
        let settled: Vec<(String, u64)> = self
            .board
            .tasks
            .values()
            .filter_map(|t| {
                let b = t.blocker?;
                let sid = &t.assignee.as_ref()?.session_id;
                let open = self.questions.get(&b.question).is_some_and(|q| q.status.is_open());
                (!open && !self.removed.contains_key(sid)).then(|| (t.id.clone(), b.question))
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
        let waiting = self.questions.values().find(|q| blocks(q) && self.question_task(q).is_some_and(|t| t.id == id)).map(blocker);
        if let Some(b) = waiting {
            self.fact(id, TaskEvent::BlockerRaised { blocker: b, replaces: false }, at);
        }
    }

    fn report_posted(&mut self, report: u64, at: Millis) -> Vec<String> {
        let Some(r) = self.reports.get(&report).filter(|r| r.kind != ReportKind::Progress) else { return vec![] };
        let (kind, reviewing) = (r.kind, self.awaits_review(r));
        let Some(id) = self.report_task(r).map(|t| t.id.clone()) else { return vec![] };
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
    /// review, or one that waited for a dependency's merge, back to work.
    fn written_to(&mut self, id: &str, by: Item, at: Millis) -> Option<String> {
        let started = self.board.tasks.get(id).is_some_and(|t| t.awaiting.is_some());
        let event = match self.task_state(id)? {
            TaskState::Done => TaskEvent::Reopen,
            TaskState::InReview => TaskEvent::ChangesRequested { on: by },
            TaskState::WaitingDependency if started => TaskEvent::ChangesRequested { on: by },
            TaskState::NotStarted | TaskState::WaitingDependency | TaskState::Running | TaskState::NeedsYou | TaskState::Closing | TaskState::Cancelled => return None,
        };
        self.fact(id, event, at).then(|| id.to_string())
    }

    fn pr_changed(&mut self, old: &PullRequest, pr: u64, at: Millis) -> Option<String> {
        let id = self.pr_task(pr)?;
        let now = self.prs.get(&pr)?.clone();
        let pushed = old.fetched && old.head_sha.is_some() && now.head_sha.is_some() && old.head_sha != now.head_sha;
        let working = matches!(self.task_state(&id), Some(TaskState::Running | TaskState::InReview | TaskState::Closing));
        let moved = match (old.state, now.state) {
            (PrState::Merged, _) => false,
            (_, PrState::Merged) if now.auto_merge => self.fact(&id, TaskEvent::AutoMerged { pr }, at),
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
            let Some(id) = self.reports.get(&report).and_then(|r| self.report_task(r)).map(|t| t.id.clone()) else { continue };
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

    /// A launch makes its task at once, with the request's options; the node that shows up
    /// under its name takes it. Before tasks were recorded, the launch waited for its node and
    /// joined the task read from it.
    fn launched(&mut self, name: &str, title: Option<&str>, launch: Launch) {
        if self.recording() {
            let request = launch.request_id.clone();
            let options = request.as_ref().and_then(|r| self.requests.get(r));
            let id = format!("t-{}", self.seq);
            let title = title.map(str::trim).filter(|t| !t.is_empty() && *t != name);
            let new = NewTask {
                id: id.clone(),
                request: request.clone().unwrap_or_else(|| format!("r-{}", self.seq)),
                title: title.unwrap_or(name).to_string(),
                brief: None,
                depends_on: vec![],
                options: TaskOptions {
                    auto_merge: launch.auto_merge,
                    effort: options.map(|o| o.effort.clone()),
                    project: options.and_then(|o| o.project.clone()),
                    issue: options.and_then(|o| o.issue.clone()),
                },
                session_id: None,
                launched: true,
                launched_as: title.map(|_| name.to_string()),
                start: None,
            };
            self.board.create(&new, launch.at);
            self.keep(Event::TaskCreated(new));
            let taker = self.nodes.values().find(|n| self.tasks_of(&n.session_id).next().is_none() && self.waiting_launch(&n.session_id).is_some_and(|t| t.id == id));
            if let Some(sid) = taker.map(|n| n.session_id.clone()) {
                self.assign(&id, &sid);
            }
            return;
        }
        if self.board.recorded_since.is_some() {
            return;
        }
        let session = self
            .nodes
            .values()
            .filter(|n| n.name.as_deref() == Some(name) && n.created_at >= launch.at - LAUNCH_WINDOW)
            .filter(|n| self.board.tasks.get(&task_id(&n.session_id)).is_some_and(|t| !t.launched))
            .max_by_key(|n| (n.created_at, n.session_id.clone()))
            .map(|n| n.session_id.clone());
        match session {
            Some(sid) => self.bind_launch(&sid, launch),
            None => {
                self.board.launches.insert(name.to_string(), launch);
            }
        }
    }

    /// A node was launched for a task that already exists: a node of its name that is already
    /// here takes it; otherwise the first one to show up does.
    fn launched_for(&mut self, task: &str) {
        if !self.recording() {
            return;
        }
        let taker = self.nodes.values().find(|n| self.tasks_of(&n.session_id).next().is_none() && self.waiting_launch(&n.session_id).is_some_and(|t| t.id == task));
        if let Some(sid) = taker.map(|n| n.session_id.clone()) {
            self.assign(task, &sid);
        }
    }

    /// Tasks whose every dependency now has its PR open or is done start: they build on top. A
    /// task that waited for a dependency's merge goes on once that PR is no longer open.
    fn dependencies_ready(&mut self, at: Millis) -> Vec<String> {
        let ready: Vec<(String, String)> = self
            .board
            .tasks
            .values()
            .filter(|t| t.state == TaskState::WaitingDependency && t.recorded == self.recording())
            .filter(|t| {
                let g = self.task_guards(t);
                g.waiting_on.is_none() && g.awaited_merge.is_none()
            })
            .filter_map(|t| {
                let awaited = t.awaiting.and_then(|pr| self.prs.get(&pr)).and_then(|p| p.task.clone()).filter(|d| t.depends_on.contains(d));
                Some((t.id.clone(), awaited.or_else(|| t.depends_on.last().cloned())?))
            })
            .collect();
        ready.into_iter().filter(|(id, dependency)| self.fact(id, TaskEvent::DependencyReady { task: dependency.clone() }, at)).map(|(id, _)| id).collect()
    }

    fn assign(&mut self, task: &str, session_id: &str) {
        let event = Event::TaskAssigned { task: task.to_string(), session_id: session_id.to_string(), agent_id: None };
        if let Some(t) = self.board.tasks.get_mut(task) {
            t.assignee = Some(Assignee { session_id: session_id.to_string(), agent_id: None });
        }
        self.keep(event);
    }

    /// The node of a launch showed up under its name.
    fn bind_launch_of(&mut self, session_id: &str) {
        if self.recording() {
            let waiting = self.waiting_launch(session_id).map(|t| t.id.clone()).filter(|_| self.tasks_of(session_id).next().is_none());
            if let Some(id) = waiting {
                self.assign(&id, session_id);
            }
            return;
        }
        let Some(node) = self.nodes.get(session_id) else { return };
        let Some(name) = node.name.clone() else { return };
        let fits = self.board.launches.get(&name).is_some_and(|l| node.created_at >= l.at - LAUNCH_WINDOW);
        if fits
            && self.board.tasks.get(&task_id(session_id)).is_some_and(|t| !t.launched && !t.recorded)
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
        if self.board.recorded_since.is_some() {
            return;
        }
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
    use crate::model::{Answer, Attachment, Link, Message, MessageAbout, QuestionStatus, Report};
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
            task: None,
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
            replaces: None, about_pr: None,
            task: None,
        })
    }

    fn answered(id: u64) -> Event {
        Event::QuestionAnswered { id, answer: Answer { text: "Yes".into(), option: None, by: None, device: None, at: 0 } }
    }

    fn opened() -> Event {
        Event::PrOpened { url: PR.into(), session_id: "s".into(), task: None }
    }

    #[test]
    fn a_pr_keeps_the_task_in_review_and_the_merge_finishes_what_the_node_said_was_finished() {
        let mut run = Run::new();
        run.on(opened()).on(report(1, ReportKind::Final, &[PR]));
        assert_eq!(run.task().final_report, Some(1));
        run.on(Event::PrMerged { id: 1, method: MergeMethod::Squash });
        run.message(1, MessageFrom::Alondra, Some(MessageAbout::PrMerged { pr_id: 1, task: None }));
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
        run.on(Event::Launched { name: "app/fix".into(), request_id: None, auto_merge: true, task: None, title: None, session: None }).on(seen("app/fix"));
        assert!(run.task().launched && run.task().options.auto_merge);
        run.on(opened()).on(report(1, ReportKind::Final, &[PR])).pr(|p| p.state = PrState::Merged);
        assert_eq!(run.states(), vec![Running, Done, Done]);
        assert!(matches!(run.task().history[2].event, TaskEvent::AutoMerged { pr: 1 }));
    }

    #[test]
    fn a_launch_puts_the_task_in_the_request_david_sent() {
        let options = crate::options::Resolved { project: Some("app".into()), auto_merge: false, effort: "hard".into(), flags: Default::default(), picked: vec![], issue: None, selection: None };
        let mut run = Run::new();
        run.on(Event::RequestSent { request_id: "r-1".into(), options, text: Some("Fix the menu".into()), attachments: vec![] });
        assert_eq!(run.view.board.request_state("r-1"), RequestState::Defining);
        run.on(seen("app/fix")).on(Event::Launched { name: "app/fix".into(), request_id: Some("r-1".into()), auto_merge: false, task: None, title: None, session: None });
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

    /// David's decision on t-47118 (a document that never moved its task): the answer goes as
    /// final with the file attached, and it both finishes the task and keeps the attachment.
    #[test]
    fn a_final_report_with_an_attachment_finishes_its_task_and_keeps_the_attachment() {
        let attachment = Attachment { name: "analysis.md".into(), source_path: "/tmp/analysis.md".into(), stored_path: Some("/store/r-1/analysis.md".into()), bytes: Some(42) };
        let mut run = Run::new();
        run.on(Event::ReportPosted(Report {
            id: 1,
            session_id: "s".into(),
            node_name: None,
            title: "Here is the answer".into(),
            body: Some("The analysis is done.".into()),
            links: vec![],
            attachments: vec![attachment.clone()],
            kind: ReportKind::Final,
            created_at: 0,
            read: false,
            to_orchestrator: false,
            mission: None,
            looks_like_question: false,
            task: None,
        }));
        assert_eq!(run.task().state, Done);
        assert_eq!(run.view.reports[&1].attachments, vec![attachment]);
    }

    /// Same decision: when David has to decide instead, the file goes on the `ask`, not a
    /// standalone document, and the task waits for him.
    #[test]
    fn an_ask_with_an_attachment_puts_the_task_in_needs_you() {
        let attachment = Attachment { name: "analysis.md".into(), source_path: "/tmp/analysis.md".into(), stored_path: Some("/store/q-1/analysis.md".into()), bytes: Some(42) };
        let mut run = Run::new();
        run.on(Event::QuestionAsked(Question {
            id: 1,
            session_id: "s".into(),
            node_name: None,
            title: "This is what is happening: do you want a task to fix it?".into(),
            detail: None,
            options: vec![],
            links: vec![],
            attachments: vec![attachment],
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
            replaces: None, about_pr: None,
            task: None,
        }));
        assert_eq!(run.task().state, NeedsYou);
    }

    /// The node list showed a task titled "alondra/estados-por-tarea" — its own node's name — next to
    /// real titled tasks: a session with no request behind it falls back to its node's display
    /// name, and nothing renamed it afterwards. `TaskTitled` fixes the "nothing" part.
    #[test]
    fn a_tasks_title_follows_its_node_until_something_names_it_for_real() {
        let mut run = Run::new();
        assert_eq!(run.task().title, run.view.nodes["s"].display_name().to_string(), "falls back to the node's own name");
        run.on(seen("app/thing"));
        assert_eq!(run.task().title, "app/thing", "still follows a rename: nothing has named it for real yet");
        let id = run.task().id.clone();
        run.on(Event::TaskTitled { task: id, title: "Fix the timeout".into() });
        assert_eq!(run.task().title, "Fix the timeout");
        run.on(seen("app/renamed"));
        assert_eq!(run.task().title, "Fix the timeout", "an explicit title survives a later rename");
    }

    #[test]
    fn the_fallback_title_prefers_the_orchestrators_name_then_davids_words_then_the_node() {
        let node = "app/thing";
        let synthetic = Request::new("r-1", 0, true);
        assert_eq!(fallback_title(None, node), node, "no request at all: the node's name");
        assert_eq!(fallback_title(Some(&synthetic), node), node, "a made-up request tells us nothing");
        let mut real = Request::new("r-1", 0, false);
        real.text = Some("Fix the timeout on checkout. It happens on slow networks.".into());
        assert_eq!(fallback_title(Some(&real), node), "Fix the timeout on checkout", "David's own words, cut to a sentence");
        real.title = Some("Checkout timeout".into());
        assert_eq!(fallback_title(Some(&real), node), "Checkout timeout", "the orchestrator's own name wins");
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
    fn a_task_whose_pr_still_waits_is_not_forgotten() {
        let mut run = Run::new();
        run.on(opened()).on(Event::NodeRemoved { session_id: "s".into() }).on(Event::Pruned { before: 100_000 });
        assert_eq!(run.view.board.tasks["t-s"].state, InReview);
    }
}
