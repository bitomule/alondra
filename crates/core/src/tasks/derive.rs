//! What moves tasks. Since the daemon records task events (`Event::TasksRecorded`), a task is made
//! by `TaskCreated` and moved only by `TaskMoved`: the daemon runs the rules below on a copy of
//! the board after each event and records what they did, so a replay applies exactly that. Tasks
//! of sessions first seen before then (older logs) are still read from the session's events by
//! the same rules: each session is one request with one task.

use std::collections::BTreeSet;

use crate::event::{Event, ReviewVerdict};
use crate::model::{Message, MessageAbout, MessageFrom, Millis, Question, QuestionOrigin, Report, ReportKind, TurnSignal};
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

/// Why a new card cannot go to the task a node named, or to any without one.
#[derive(Debug, PartialEq)]
pub enum CardTask {
    Unknown(String),
    NotYours(String),
    /// The node works on several open tasks and did not say which: their ids.
    Several(Vec<String>),
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
/// in the Fleet view).
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

/// Events that can move a recorded task, make one or give one to a node.
fn may_move_tasks(event: &Event) -> bool {
    match event {
        Event::Hook(h) => matches!(h.event.as_str(), "PostToolUse" | "PostToolUseFailure" | "UserPromptSubmit" | "SessionEnd"),
        Event::QuestionAsked(_)
        | Event::QuestionAnswered { .. }
        | Event::QuestionClosed { .. }
        | Event::ProcessExited { .. }
        | Event::ReportPosted(_)
        | Event::ReportRead { .. }
        | Event::ArtifactReviewed { .. }
        | Event::MessageHanded { .. }
        | Event::PrOpened { .. }
        | Event::PrUpdated(_)
        | Event::PrMerged { .. }
        | Event::PrDismissed { .. }
        | Event::PrChangesRequested { .. }
        | Event::PrAsked { .. }
        | Event::PrReassigned { .. }
        | Event::NodeRemoved { .. }
        | Event::DaemonSeen(_)
        | Event::Launched { .. }
        | Event::TaskCreated(_)
        | Event::TaskMoved { .. } => true,
        _ => false,
    }
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
            Some(id) => self.board.tasks.contains_key(id).then(|| id.clone()),
            None => {
                let id = task_id(session_id);
                if self.board.tasks.contains_key(&id) { Some(id) } else { self.session_task(session_id).map(|t| t.id.clone()) }
            }
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

    /// The task a session's first card goes to, and the event that gives it to the session: the
    /// launch it came from, or a task of its own.
    fn first_task(&self, session_id: &str) -> Option<(String, Event)> {
        let node = self.nodes.get(session_id).filter(|n| !n.is_orchestrator)?;
        if let Some(t) = self.waiting_launch(session_id) {
            return Some((t.id.clone(), Event::TaskAssigned { task: t.id.clone(), session_id: session_id.to_string(), agent_id: None }));
        }
        let id = task_id(session_id);
        let request = session_request(session_id);
        let title = fallback_title(self.board.requests.get(&request), node.display_name());
        let new = NewTask {
            id: id.clone(),
            request,
            title,
            brief: node.mission.clone(),
            depends_on: vec![],
            options: TaskOptions { auto_merge: node.name.as_ref().is_some_and(|n| self.auto_merge_nodes.contains(n)), ..TaskOptions::default() },
            session_id: Some(session_id.to_string()),
            launched: false,
            launched_as: None,
            start: None,
        };
        Some((id, Event::TaskCreated(new)))
    }

    /// The open task a subagent of the node works on: assigned to it, or the one it bound itself
    /// to (`activity`).
    fn agent_task(&self, session_id: &str, agent_id: &str) -> Option<&Task> {
        let bound = self.nodes.get(session_id).and_then(|n| n.ledger.bound(agent_id));
        self.tasks_of(session_id)
            .filter(|t| !t.state.finished())
            .find(|t| t.assignee.as_ref().and_then(|a| a.agent_id.as_deref()) == Some(agent_id) || bound == Some(t.id.as_str()))
    }

    /// The task a new card of the node goes to: the one it names, else its only open task, else
    /// its newest, else the one its first card gives it. A subagent's card goes to the open task
    /// it works on.
    pub fn card_task(&self, session_id: &str, named: Option<&str>, agent_id: Option<&str>) -> Result<String, CardTask> {
        if let Some(id) = named.map(str::trim).filter(|t| !t.is_empty()) {
            let task = self.board.tasks.get(id).ok_or_else(|| CardTask::Unknown(id.to_string()))?;
            return if task.is_assigned_to(session_id) { Ok(task.id.clone()) } else { Err(CardTask::NotYours(id.to_string())) };
        }
        let open: Vec<&Task> = self.tasks_of(session_id).filter(|t| !t.state.finished()).collect();
        match (open.as_slice(), agent_id.and_then(|a| self.agent_task(session_id, a))) {
            (_, Some(t)) => Ok(t.id.clone()),
            ([one], None) => Ok(one.id.clone()),
            ([], None) => Ok(self.session_task(session_id).map(|t| t.id.clone()).or_else(|| self.first_task(session_id).map(|(id, _)| id)).unwrap_or_else(|| task_id(session_id))),
            (several, None) => Err(CardTask::Several(several.iter().map(|t| t.id.clone()).collect())),
        }
    }

    /// The task a card of a fact (a hook, a menu, the sentinel) goes to: it cannot be refused,
    /// so with several open tasks it goes to the newest. The orchestrator's cards have none. A PR
    /// never goes by it: see `pr_task_for`.
    pub fn fact_task(&self, session_id: &str, agent_id: Option<&str>) -> Option<String> {
        if self.nodes.get(session_id).is_none_or(|n| n.is_orchestrator) {
            return None;
        }
        match self.card_task(session_id, None, agent_id) {
            Ok(id) => Some(id),
            Err(_) => self.session_task(session_id).map(|t| t.id.clone()),
        }
    }

    /// Attribution for every hook. A specific Alondra call or a bound subagent has one task;
    /// an unbound main thread or subagent may concern several, so keep all candidates.
    pub fn hook_task_ids(&self, session_id: &str, agent_id: Option<&str>, named: Option<&str>) -> Vec<String> {
        if self.nodes.get(session_id).is_some_and(|n| n.is_orchestrator) {
            return vec![];
        }
        if let Some(id) = named.filter(|id| self.board.tasks.get(*id).is_some_and(|t| t.is_assigned_to(session_id))) {
            return vec![id.to_string()];
        }
        if let Some(agent) = agent_id
            && let Some(task) = self.agent_task(session_id, agent)
        {
            return vec![task.id.clone()];
        }
        let mut ids: Vec<String> = self.tasks_of(session_id).filter(|t| !t.state.finished()).map(|t| t.id.clone()).collect();
        if ids.is_empty() {
            ids = self.tasks_of(session_id).map(|t| t.id.clone()).collect();
        }
        if ids.is_empty() {
            ids.extend(self.first_task(session_id).map(|(id, _)| id));
        }
        ids
    }

    /// The task a PR the node opened is for, in this order: the task its title, body or branch
    /// names (“Alondra task: t-…”), the task of the subagent that opened it, the node's only open
    /// task (its only task when none is open). None when the node works on several and nothing
    /// says which: Alondra asks the node instead of guessing. 25 Sep 00:01: the main thread of
    /// 7e661a50 opened #98 and #99 for its review, and both went to t-44370, its newest task.
    pub fn pr_task_for(&self, session_id: &str, agent_id: Option<&str>, texts: &[&str]) -> Option<String> {
        if self.nodes.get(session_id).is_some_and(|n| n.is_orchestrator) {
            return None;
        }
        self.named_task(session_id, texts)
            .or_else(|| agent_id.and_then(|a| self.agent_task(session_id, a)).map(|t| t.id.clone()))
            .or_else(|| self.single_task(session_id).map(str::to_string))
    }

    /// The node's task a PR's text names: the one after “Alondra task:”, else the only open task
    /// it mentions.
    fn named_task(&self, session_id: &str, texts: &[&str]) -> Option<String> {
        let mine = |id: &str| self.board.tasks.get(id).filter(|t| t.is_assigned_to(session_id));
        let resolve = |mention: &str| {
            let mut id = mention;
            loop {
                if let Some(t) = mine(id) {
                    return Some(t);
                }
                id = &id[..id.rfind('-').filter(|&i| i > 1)?];
            }
        };
        let named: Vec<(&Task, bool)> = texts.iter().flat_map(|t| task_mentions(t)).filter_map(|(id, marked)| resolve(id).map(|t| (t, marked))).collect();
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

    /// What Alondra types into a node, once, when a PR it opened got no task: it works on several
    /// and nothing said which.
    pub fn pr_task_question(&self, url: &str, at: Millis) -> Option<Message> {
        let (repo, number) = crate::pr::parse_url(url)?;
        let pr = self.prs.values().find(|p| p.repo == repo && p.number == number)?;
        if self.pr_task_of(pr).is_some() || self.nodes.get(&pr.session_id).is_none_or(|n| n.is_orchestrator) {
            return None;
        }
        let open: Vec<String> = self.tasks_of(&pr.session_id).filter(|t| !t.state.finished()).map(|t| format!("- {}: {}", t.id, t.title)).collect();
        let text = format!(
            "[Alondra] PR #{number}: which task is it for? Report it with `task`, or add “Alondra task: t-…” to its body. Your open tasks:\n{}",
            open.join("\n")
        );
        self.alondra_message(&pr.session_id, text, at)
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

    /// Why the machine would refuse the event on the task now, without applying it.
    pub fn task_refusal(&self, id: &str, event: &TaskEvent) -> Option<super::Refused> {
        let Some(task) = self.board.tasks.get(id) else { return Some(super::refused(format!("there is no task {id}"))) };
        super::transition(task.state, event, &self.guards_for(task, event)).err()
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

    /// What the daemon records before an event: the task a session's first card goes to.
    pub fn tasks_before(&self, event: &Event) -> Vec<Event> {
        if self.board.recorded_since.is_none() {
            return vec![];
        }
        let (session, named) = match event {
            Event::QuestionAsked(q) => (&q.session_id, q.task.as_ref()),
            Event::ReportPosted(r) if r.kind != ReportKind::Progress => (&r.session_id, r.task.as_ref()),
            Event::PrOpened { session_id, task, .. } => (session_id, task.as_ref()),
            _ => return vec![],
        };
        let has_one = self.tasks_of(session).next().is_some() || self.board.tasks.contains_key(&task_id(session));
        match self.first_task(session) {
            Some((id, event)) if !has_one && named.is_none_or(|n| *n == id) => vec![event],
            _ => vec![],
        }
    }

    /// What the daemon records after an event: the task events the rules apply to recorded tasks,
    /// worked out on a copy of the board. The view is left as it was.
    pub fn task_moves(&mut self, at: Millis, event: &Event, before: Before) -> Vec<Event> {
        if self.board.recorded_since.is_none() || !may_move_tasks(event) {
            return vec![];
        }
        if !self.worth_a_pass(event) {
            return vec![];
        }
        let real = self.board.clone();
        self.task_pass = Pass::Record(vec![]);
        self.move_tasks(at, event, before);
        let Pass::Record(moves) = std::mem::take(&mut self.task_pass) else { unreachable!("the pass was a record pass") };
        self.board = real;
        moves
    }

    /// Most events of a busy fleet are hooks and daemon sightings that move no recorded task:
    /// they skip the pass and its copy of the board.
    fn worth_a_pass(&self, event: &Event) -> bool {
        let recorded = || self.board.tasks.values().filter(|t| t.recorded);
        match event {
            Event::DaemonSeen(_) => recorded().any(|t| t.launched && t.assignee.is_none()),
            Event::Hook(h) if h.event == "UserPromptSubmit" => h.prompt.is_some() && recorded().any(|t| t.is_assigned_to(&h.session_id)),
            Event::Hook(_) => recorded().any(|t| t.blocker.is_some()),
            _ => true,
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
                let id = named.clone().or_else(|| self.pr_task_for(session_id, None, &[]));
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
                    None => self.session_task(&m.session_id).map(|t| t.id.clone()),
                    Some(MessageAbout::Task { task }) => self.board.tasks.get(task).filter(|t| t.is_assigned_to(&m.session_id)).map(|t| t.id.clone()),
                    Some(_) => None,
                });
                if let Some(tid) = task {
                    moved.extend(self.written_to(&tid, Item::Message(*id), at));
                }
            }
            (Event::Hook(h), _) if h.event == "UserPromptSubmit" && h.prompt.is_some() => {
                let typed = self.nodes.get(&h.session_id).is_some_and(|n| crate::states::talking_with_user(self, n));
                let task = self.session_task(&h.session_id).map(|t| t.id.clone()).filter(|_| typed);
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
            TaskState::WaitingDependency | TaskState::Running | TaskState::NeedsYou | TaskState::Closing | TaskState::Cancelled => return None,
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

    /// Applies an event as the daemon records it: the events that give a session its first task,
    /// the event, and the task events it causes.
    pub fn record(&mut self, at: Millis, event: &Event) -> Vec<Event> {
        let mut log = self.tasks_before(event);
        for e in &log {
            self.apply(self.seq + 1, at, e);
        }
        let before = self.before_tasks(event);
        self.apply(self.seq + 1, at, event);
        log.push(event.clone());
        let moves = self.task_moves(at, event, before);
        for e in &moves {
            self.apply(self.seq + 1, at, e);
        }
        log.extend(moves);
        log
    }

    /// What the machine would refuse if the event were recorded now: the task and the refused
    /// fact. A node's own call is turned down with it instead of being kept as an anomaly.
    pub fn refusal(&self, at: Millis, event: &Event) -> Option<(String, super::Anomaly)> {
        let mut after = self.clone();
        after.record(at, event);
        after.board.tasks.values().find_map(|t| {
            let known = self.board.tasks.get(&t.id).map_or(0, |b| b.anomalies.len());
            t.anomalies.get(known).map(|a| (t.id.clone(), a.clone()))
        })
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
        let options = crate::options::Resolved { project: Some("app".into()), auto_merge: false, effort: "hard".into(), flags: Default::default(), picked: vec![], issue: None };
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

    /// Fleet showed a task titled "alondra/estados-por-tarea" — its own node's name — next to
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

    /// 24 Sep: 40 tasks of the real log stayed running after their node was closed.
    #[test]
    fn closing_a_node_drops_what_it_was_doing_until_it_is_forgotten() {
        let mut run = Run::new();
        run.on(question(1)).on(Event::NodeRemoved { session_id: "s".into() }).on(answered(1));
        let task = run.view.board.tasks["t-s"].clone();
        assert_eq!((task.state, task.blocker), (Cancelled, None));
        assert_eq!(task.history.last().map(|h| h.event.clone()), Some(TaskEvent::Cancel { reason: Some(NODE_CLOSED.into()) }));
        run.on(Event::Pruned { before: 0 });
        assert!(run.view.board.tasks.contains_key("t-s"), "within the retention window");
        run.on(Event::Pruned { before: 100_000 });
        assert!(run.view.board.tasks.contains_key("t-s"), "finished: it stays until it is archived");
        run.on(Event::TasksArchived(run.view.board.archivable(None))).on(Event::ArchivedPruned { before: 100_000 });
        assert!(run.view.board.tasks.is_empty() && run.view.board.requests.is_empty(), "its node is gone and nothing of it waits");
    }

    #[test]
    fn a_task_whose_pr_still_waits_is_not_forgotten() {
        let mut run = Run::new();
        run.on(opened()).on(Event::NodeRemoved { session_id: "s".into() }).on(Event::Pruned { before: 100_000 });
        assert_eq!(run.view.board.tasks["t-s"].state, InReview);
    }

    /// The daemon's side: every event goes through `View::record`, and the log keeps what it wrote.
    mod recorded {
        use super::*;
        use crate::model::{Question, QuestionOrigin};

        const UPS: &str = "UserPromptSubmit";
        const PR150: &str = "https://github.com/a/blurcamera/pull/150";
        const PR151: &str = "https://github.com/a/blurcamera/pull/151";

        struct Daemon {
            view: View,
            log: Vec<(Millis, Event)>,
            at: Millis,
        }

        impl Daemon {
            fn new() -> Self {
                let mut d = Daemon { view: View::default(), log: vec![], at: 0 };
                d.on(Event::TasksRecorded);
                d
            }

            fn on(&mut self, e: Event) -> &mut Self {
                self.at += 1000;
                let at = self.at;
                let written = self.view.record(at, &e);
                self.log.extend(written.into_iter().map(|e| (at, e)));
                self
            }

            fn task(&self, sid: &str) -> &Task {
                self.view.session_task(sid).expect("the node's task")
            }

            fn replayed(&self, keep: impl Fn(&Event) -> bool) -> View {
                let mut v = View::default();
                for (i, (at, e)) in self.log.iter().filter(|(_, e)| keep(e)).enumerate() {
                    v.apply(i as u64 + 1, *at, e);
                }
                v
            }

            fn refusal(&self, e: Event) -> Option<String> {
                self.view.refusal(self.at + 1000, &e).map(|(_, a)| a.reason)
            }

            fn github(&mut self, id: u64, f: impl FnOnce(&mut PullRequest)) -> &mut Self {
                let mut pr = self.view.prs[&id].clone();
                pr.fetched = true;
                f(&mut pr);
                self.on(Event::PrUpdated(pr))
            }
        }

        fn hook_of(sid: &str, event: &str) -> Event {
            Event::Hook(HookEvent { session_id: sid.into(), event: event.into(), ..Default::default() })
        }

        fn asked(id: u64, sid: &str, f: impl FnOnce(&mut Question)) -> Event {
            let Event::QuestionAsked(mut q) = question(id) else { unreachable!() };
            q.session_id = sid.into();
            f(&mut q);
            Event::QuestionAsked(q)
        }

        fn reported(id: u64, sid: &str, kind: ReportKind, links: &[&str]) -> Event {
            let Event::ReportPosted(mut r) = report(id, kind, links) else { unreachable!() };
            r.session_id = sid.into();
            Event::ReportPosted(r)
        }

        fn opened_by(url: &str, sid: &str) -> Event {
            Event::PrOpened { url: url.into(), session_id: sid.into(), task: None }
        }

        fn moves(d: &Daemon) -> Vec<&'static str> {
            d.log.iter().filter_map(|(_, e)| match e {
                Event::TaskMoved { event, .. } => Some(event.name()),
                _ => None,
            }).collect()
        }

        #[test]
        fn the_log_holds_every_move_of_a_recorded_task_and_replays_to_the_same_board() {
            let mut d = Daemon::new();
            d.on(hook_of("s", UPS)).on(asked(1, "s", |_| {})).on(answered(1)).on(opened_by(PR, "s")).on(reported(1, "s", ReportKind::Final, &[PR]));
            d.on(Event::PrMerged { id: 1, method: MergeMethod::Squash });
            let kinds: Vec<&str> = d.log.iter().map(|(_, e)| e.kind()).collect();
            let created = kinds.iter().position(|k| *k == "task_created");
            assert!(created.is_some() && created < kinds.iter().position(|k| *k == "question_asked"), "the task comes before its first card: {kinds:?}");
            assert_eq!(moves(&d), ["blocker_raised", "answered", "pr_opened", "final_report", "merged"]);
            assert!(d.task("s").recorded);
            assert_eq!(d.task("s").state, Done);
            assert_eq!(d.replayed(|_| true).board, d.view.board, "a replay applies what the log says");
            let without = d.replayed(|e| !matches!(e, Event::TaskMoved { .. }));
            assert_eq!((without.board.tasks["t-s"].state, without.board.tasks["t-s"].history.len()), (Running, 0), "nothing but the task events moves it");
        }

        #[test]
        fn a_launch_makes_its_task_with_the_request_and_the_node_of_its_name_takes_it() {
            let options = crate::options::Resolved { project: Some("app".into()), auto_merge: true, effort: "hard".into(), flags: Default::default(), picked: vec![], issue: None };
            let mut d = Daemon::new();
            d.on(Event::RequestSent { request_id: "r-1".into(), options, text: Some("Fix the menu".into()), attachments: vec![] });
            d.on(Event::Launched { name: "app/fix".into(), request_id: Some("r-1".into()), auto_merge: true, task: None, title: None, session: None });
            let id = d.view.board.tasks.keys().next().cloned().expect("the launch made its task");
            assert_eq!(d.view.board.tasks[&id].assignee, None, "no node yet");
            d.on(seen("app/fix")).on(hook(UPS)).on(question(1));
            let task = d.task("s");
            assert_eq!((task.id.as_str(), task.request.as_str(), task.options.auto_merge, task.options.effort.as_deref()), (id.as_str(), "r-1", true, Some("hard")));
            assert_eq!(d.view.board.tasks.len(), 1, "the node took the launched task instead of making its own");
            assert_eq!(d.view.question_task(&d.view.questions[&1]).map(|t| t.id.clone()), Some(id.clone()), "a card that names no task goes to its node's");
            assert_eq!(d.task("s").state, NeedsYou);
        }

        #[test]
        fn a_second_question_while_one_is_open_is_refused_unless_it_replaces_it() {
            let mut d = Daemon::new();
            d.on(hook_of("s", UPS)).on(asked(1, "s", |_| {}));
            assert_eq!(d.refusal(asked(2, "s", |_| {})).as_deref(), Some("question #1 is still open"));
            assert_eq!(d.refusal(asked(2, "s", |q| q.blocking = false)), None, "a question that blocks nothing is not a second blocker");
            assert_eq!(d.refusal(asked(2, "s", |q| q.to_orchestrator = true)), None);
            assert_eq!(d.refusal(asked(2, "s", |q| q.replaces = Some(1))), None);
            d.on(asked(2, "s", |q| q.replaces = Some(1))).on(Event::QuestionClosed { id: 1, status: QuestionStatus::Withdrawn, note: None });
            let task = d.task("s");
            assert_eq!((task.state, task.blocker.map(|b| b.question), task.anomalies.len()), (NeedsYou, Some(2), 0));
        }

        #[test]
        fn a_permission_prompt_held_is_the_tasks_blocker() {
            let mut d = Daemon::new();
            d.on(hook_of("s", UPS)).on(asked(1, "s", |q| q.origin = QuestionOrigin::Permission));
            assert_eq!(d.task("s").state, NeedsYou);
            assert_eq!(d.refusal(asked(2, "s", |_| {})).as_deref(), Some("permission request #1 is still open"));
        }

        #[test]
        fn a_node_cannot_finish_while_its_question_waits_nor_ask_once_its_task_is_done() {
            let mut d = Daemon::new();
            d.on(hook_of("s", UPS)).on(asked(1, "s", |_| {}));
            assert_eq!(d.refusal(reported(1, "s", ReportKind::Final, &[])).as_deref(), Some("question #1 is still open"));
            d.on(answered(1)).on(reported(1, "s", ReportKind::Final, &[]));
            assert_eq!(d.task("s").state, Done);
            assert_eq!(d.refusal(asked(2, "s", |_| {})).as_deref(), Some("the task is done: reopen it first"));
            assert_eq!(d.refusal(reported(2, "s", ReportKind::Notice, &[])), None, "a notice moves nothing");
            assert_eq!(d.refusal(reported(2, "s", ReportKind::Final, &[])), None);
        }

        #[test]
        fn a_final_report_while_the_pr_waits_says_nothing_follows_the_merge() {
            let mut d = Daemon::new();
            d.on(hook_of("s", UPS)).on(opened_by(PR, "s"));
            assert_eq!(d.refusal(reported(1, "s", ReportKind::Final, &[PR])), None);
        }

        /// BlurCamera #150/#151 (23 Sep): David used «request changes» on #150 to ask a question.
        /// That parked #150 until a push, and #151, stacked on it, could only say "merge PR #150
        /// first".
        #[test]
        fn asking_on_a_pr_keeps_it_in_view_and_moves_only_its_own_task() {
            let mut d = Daemon::new();
            d.on(hook_of("a", UPS)).on(hook_of("b", UPS)).on(opened_by(PR150, "a")).on(opened_by(PR151, "b"));
            d.github(1, |p| (p.base, p.head, p.head_sha) = (Some("main".into()), Some("x".into()), Some("a1".into())));
            d.github(2, |p| (p.base, p.head, p.head_sha) = (Some("x".into()), Some("y".into()), Some("b1".into())));
            let in_view = |d: &Daemon| d.view.prs_awaiting_user().map(|p| p.number).collect::<Vec<_>>();
            assert_eq!((d.task("a").state, d.task("b").state), (InReview, InReview));
            assert_eq!(d.view.merge_blocked_by(2).map(|p| p.number), Some(150), "#151 merges after #150");

            d.on(Event::PrAsked { id: 1 });
            assert_eq!((d.task("a").state, d.task("b").state), (Running, InReview), "only #150's task goes back to work");
            assert_eq!(in_view(&d), [150, 151], "asking parks nothing");
            d.on(reported(1, "a", ReportKind::Notice, &[PR150]));
            assert_eq!(d.task("a").state, InReview, "the answer puts it back in review");
            assert_eq!(d.view.report_pr(&d.view.reports[&1]), Some(1));

            d.on(Event::PrChangesRequested { id: 1 });
            assert_eq!((d.task("a").state, d.task("b").state), (Running, InReview));
            assert_eq!(in_view(&d), [151], "request changes parks #150 until the node answers");
            d.on(reported(2, "a", ReportKind::Notice, &[]));
            assert_eq!((d.task("a").state, in_view(&d)), (InReview, vec![150, 151]), "answering without a push brings it back");

            d.on(Event::PrChangesRequested { id: 1 });
            d.github(1, |p| p.head_sha = Some("a2".into()));
            assert_eq!((d.task("a").state, in_view(&d)), (InReview, vec![150, 151]), "so does a push");

            d.on(Event::PrMerged { id: 1, method: MergeMethod::Squash });
            assert_eq!(d.view.merge_blocked_by(2), None);
            d.on(Event::PrMerged { id: 2, method: MergeMethod::Squash });
            assert_eq!((d.task("a").state, d.task("b").state), (Closing, Closing));
            assert!(d.task("a").anomalies.is_empty() && d.task("b").anomalies.is_empty());
        }

        /// 23-24 Sep: 12 merges reached nodes idle after their final report; 11 went back to work
        /// and 10 posted another report within an hour.
        #[test]
        fn a_merge_after_the_final_report_finishes_the_task_and_no_notice_reopens_it() {
            let mut d = Daemon::new();
            d.on(hook_of("s", UPS)).on(opened_by(PR, "s")).on(reported(1, "s", ReportKind::Final, &[PR]));
            assert!(d.view.merge_finishes_task(1), "the node gets no merge notice");
            d.on(Event::PrMerged { id: 1, method: MergeMethod::Squash });
            let notice = Message { id: 1, session_id: "s".into(), from: MessageFrom::Alondra, text: "merged".into(), answers_question: None, created_at: 0, handed_at: None, delivered_at: None, attachments: vec![], about: Some(MessageAbout::PrMerged { pr_id: 1, task: None }) };
            d.on(Event::MessageQueued(notice)).on(Event::MessageHanded { id: 1 });
            assert_eq!(d.task("s").state, Done);

            let mut working = Daemon::new();
            working.on(hook_of("s", UPS)).on(opened_by(PR, "s"));
            assert!(!working.view.merge_finishes_task(1), "a node with work after the merge gets the notice");
            working.on(Event::PrMerged { id: 1, method: MergeMethod::Squash });
            assert_eq!(working.task("s").state, Closing);
        }

        /// alondra/adjuntos (24 Sep): merging #78 withdrew action #217 and the node was nudged 18 s
        /// later.
        #[test]
        fn a_merge_withdraws_only_the_questions_of_its_own_task() {
            const PR78: &str = "https://github.com/a/alondra/pull/78";
            let link = |q: &mut Question| {
                q.links = vec![Link { url: PR78.into(), title: None }];
                q.about_pr = Some(78);
            };
            let mut d = Daemon::new();
            d.on(hook_of("a", UPS)).on(hook_of("b", UPS)).on(opened_by(PR78, "a"));
            d.on(asked(1, "a", link)).on(asked(2, "b", link)).on(asked(3, "b", |q| {
                link(q);
                q.action = true;
                q.blocking = false;
            }));
            d.on(Event::PrMerged { id: 1, method: MergeMethod::Squash });
            let status = |id: u64| d.view.questions[&id].status;
            assert_eq!((status(1), status(2), status(3)), (QuestionStatus::Withdrawn, QuestionStatus::Open, QuestionStatus::Open));
            assert_eq!(d.task("b").state, NeedsYou);
        }

        /// t-47328, 25 Sep: PR card 99 (bitomule/alondra#102) waited for David's review. Question #253
        /// (how #102 ships) and #254 (a cron that #102 breaks) both went into its card, and merging
        /// #102 withdrew #254 unanswered. Now #253 says it is about #102 and joins the card, #254 is
        /// refused, and the node opens a task for the cron, where #254 waits for David whatever
        /// happens to #102.
        #[test]
        fn a_question_about_other_work_goes_in_its_own_task_and_the_merge_leaves_it() {
            const PR102: &str = "https://github.com/bitomule/alondra/pull/102";
            let on = |task: &'static str, about_pr: Option<u64>| {
                move |q: &mut Question| {
                    q.task = Some(task.into());
                    q.about_pr = about_pr;
                    q.links = vec![Link { url: PR102.into(), title: None }];
                }
            };
            let created = |id: &str, title: &str| {
                Event::TaskCreated(NewTask {
                    id: id.into(),
                    request: "r-1".into(),
                    title: title.into(),
                    brief: None,
                    depends_on: vec![],
                    options: TaskOptions::default(),
                    session_id: Some("s".into()),
                    launched: false,
                    launched_as: None,
                    start: None,
                })
            };
            let mut d = Daemon::new();
            d.on(created("t-47328", "Only the sessions Alondra launches")).on(hook_of("s", UPS));
            d.on(Event::PrOpened { url: PR102.into(), session_id: "s".into(), task: Some("t-47328".into()) });
            assert_eq!(state(&d, "t-47328"), InReview);

            d.on(asked(253, "s", on("t-47328", Some(102))));
            assert_eq!(state(&d, "t-47328"), NeedsYou);
            assert_eq!(d.view.question_pr(&d.view.questions[&253]), Some(1), "#253 is about #102: it shows in its card");
            d.on(answered(253));
            assert_eq!(state(&d, "t-47328"), InReview, "#102 still waits for David");

            let before = d.view.board.clone();
            let refusal = d.refusal(asked(254, "s", on("t-47328", None)));
            assert_eq!(
                refusal.as_deref(),
                Some("PR #102 waits for review. If the answer changes what PR #102 contains or how it ships, resend with about_pr: 102. Otherwise open a separate task with new_task and ask there")
            );
            assert_eq!(d.view.board, before, "a refusal moves nothing");

            d.on(created("t-2", "Deliver the efficiency cron without Alondra"));
            let cron = &d.view.board.tasks["t-2"];
            assert_eq!((cron.request.as_str(), cron.assignee.as_ref().map(|a| a.session_id.as_str()), cron.state, cron.recorded), ("r-1", Some("s"), Running, true));
            d.on(asked(254, "s", on("t-2", None)));
            assert_eq!((state(&d, "t-2"), state(&d, "t-47328")), (NeedsYou, InReview), "#254 waits on its own task");
            assert_eq!(d.view.question_pr(&d.view.questions[&254]), None, "a link to #102 does not put it in #102's card");

            d.on(asked(255, "s", on("t-47328", Some(102))));
            d.on(Event::PrMerged { id: 1, method: MergeMethod::Merge });
            let status = |id: u64| d.view.questions[&id].status;
            assert_eq!((status(255), status(254)), (QuestionStatus::Withdrawn, QuestionStatus::Open), "the merge settles what was about #102 and leaves the cron's question");
            assert_eq!(state(&d, "t-2"), NeedsYou);
            assert_eq!(d.replayed(|_| true).board, d.view.board);
        }

        /// sin-dormidos (~4 h) and maquina-de-estados (3 h), 23-24 Sep: leftover background tasks
        /// showed nodes as working past their final report or a pending review.
        #[test]
        fn a_nodes_background_tasks_never_move_its_task() {
            let busy = || Event::DaemonSeen(DaemonObservation { session_id: "s".into(), short_id: "s".into(), pid: Some(1), tempo: Some("active".into()), tasks: Some(2), ..Default::default() });
            let mut d = Daemon::new();
            d.on(busy()).on(hook(UPS)).on(opened_by(PR, "s")).on(busy());
            assert_eq!(d.task("s").state, InReview);
            d.on(reported(1, "s", ReportKind::Final, &[PR])).on(busy()).on(Event::PrMerged { id: 1, method: MergeMethod::Merge }).on(busy());
            assert_eq!(d.task("s").state, Done);
        }

        /// PR card #50 with reports #146 and #147 and a question on the same decision (24 Sep):
        /// three cards for one piece of work.
        #[test]
        fn a_tasks_pr_reports_and_question_are_one_card() {
            const PR50: &str = "https://github.com/a/alondra/pull/50";
            let mut d = Daemon::new();
            d.on(hook_of("s", UPS)).on(opened_by(PR50, "s")).on(reported(146, "s", ReportKind::Notice, &[PR50])).on(reported(147, "s", ReportKind::Final, &[])).on(asked(1, "s", |q| q.about_pr = Some(50)));
            let v = &d.view;
            assert_eq!(v.prs_awaiting_user().count(), 1);
            assert_eq!(v.question_pr(&v.questions[&1]), Some(1));
            assert!(v.inbox_reports().all(|r| v.report_pr(r) == Some(1)), "both reports are inside the PR card");

            let mut plain = Daemon::new();
            plain.on(hook_of("s", UPS)).on(reported(146, "s", ReportKind::Notice, &[])).on(asked(1, "s", |_| {}));
            assert_eq!(plain.view.question_reports(&plain.view.questions[&1]), [146]);
            assert_eq!(plain.view.inbox_reports().count(), 0, "the report and the question are one card");
        }

        /// Before #27 (22 Sep) a node's own answered question kept it from being closed.
        #[test]
        fn an_answered_question_leaves_nothing_that_holds_the_task() {
            let mut d = Daemon::new();
            d.on(hook_of("s", UPS)).on(asked(1, "s", |_| {})).on(answered(1));
            assert_eq!((d.task("s").state, d.task("s").blocker), (Running, None));
            d.on(Event::NodeRemoved { session_id: "s".into() });
            assert_eq!(d.view.board.tasks["t-s"].state, Cancelled);
            assert!(d.view.board.tasks["t-s"].anomalies.is_empty());
            assert_eq!(moves(&d).last(), Some(&"cancel"), "the cancel is in the log");
        }

        #[test]
        fn closing_a_node_keeps_a_task_whose_pr_waits_for_david() {
            let mut d = Daemon::new();
            d.on(hook_of("s", UPS)).on(opened_by(PR, "s")).on(Event::PrAsked { id: 1 }).on(Event::NodeRemoved { session_id: "s".into() });
            assert_eq!(d.view.board.tasks["t-s"].state, Running, "the PR is still David's to merge");
        }

        fn defined(id: &str, depends_on: &[&str], name: &str) -> Event {
            Event::TaskCreated(NewTask {
                id: id.into(),
                request: "r-1".into(),
                title: format!("Task {id}"),
                brief: Some(format!("Build {id}.")),
                depends_on: depends_on.iter().map(|d| d.to_string()).collect(),
                options: TaskOptions::default(),
                session_id: None,
                launched: false,
                launched_as: None,
                start: Some(crate::tasks::Start::NewNode { name: name.into(), cwd: "/tmp".into(), model: None }),
            })
        }

        fn launched_for(task: &str, name: &str) -> Event {
            Event::Launched { name: name.into(), request_id: Some("r-1".into()), auto_merge: false, task: Some(task.into()), title: None, session: None }
        }

        fn seen_as(sid: &str, name: &str) -> Event {
            Event::DaemonSeen(DaemonObservation { session_id: sid.into(), short_id: sid.into(), name: Some(name.into()), pid: Some(1), ..Default::default() })
        }

        fn moved(task: &str, event: TaskEvent) -> Event {
            Event::TaskMoved { task: task.into(), event }
        }

        fn state(d: &Daemon, id: &str) -> TaskState {
            d.view.board.tasks[id].state
        }

        fn last_event(d: &Daemon, id: &str) -> Option<TaskEvent> {
            d.view.board.tasks[id].history.last().map(|h| h.event.clone())
        }

        /// A task cancelled while another waited on it (by the orchestrator, or by closing its node
        /// before it opened a PR) left the other waiting for good: nothing could start it, and
        /// reopening it was refused.
        #[test]
        fn a_task_waiting_for_a_cancelled_one_is_cancelled_with_it() {
            let mut d = Daemon::new();
            d.on(defined("t-a", &[], "app/a")).on(defined("t-b", &["t-a"], "app/b")).on(defined("t-c", &["t-b"], "app/c"));
            d.on(moved("t-a", TaskEvent::Cancel { reason: Some("not needed".into()) }));
            assert_eq!((state(&d, "t-a"), state(&d, "t-b"), state(&d, "t-c")), (Cancelled, Cancelled, Cancelled));
            assert_eq!(last_event(&d, "t-b"), Some(TaskEvent::Cancel { reason: Some("t-a was cancelled".into()) }));
            assert_eq!(last_event(&d, "t-c"), Some(TaskEvent::Cancel { reason: Some("t-b was cancelled".into()) }));
            assert_eq!(d.view.task_refusal("t-b", &TaskEvent::Reopen).map(|r| r.reason).as_deref(), Some("t-a was cancelled: reopen it first"));
            d.on(moved("t-a", TaskEvent::Reopen)).on(moved("t-b", TaskEvent::Reopen));
            assert_eq!((state(&d, "t-a"), state(&d, "t-b")), (Running, WaitingDependency), "back in line behind t-a");
            assert_eq!(d.replayed(|_| true).board, d.view.board);

            let mut closed = Daemon::new();
            closed.on(defined("t-a", &[], "app/a")).on(defined("t-b", &["t-a"], "app/b"));
            closed.on(launched_for("t-a", "app/a")).on(seen_as("s", "app/a")).on(Event::NodeRemoved { session_id: "s".into() });
            assert_eq!((state(&closed, "t-a"), state(&closed, "t-b")), (Cancelled, Cancelled), "its node closed before it opened a PR");
            assert!(closed.view.tasks_to_start().is_empty());
        }

        /// Reopening a task that waits for another sent it straight to work, ahead of what it
        /// builds on.
        #[test]
        fn a_reopened_task_waits_again_for_what_it_depends_on() {
            let mut d = Daemon::new();
            d.on(defined("t-a", &[], "app/a")).on(defined("t-b", &["t-a"], "app/b"));
            d.on(moved("t-b", TaskEvent::Cancel { reason: None })).on(moved("t-b", TaskEvent::Reopen));
            assert_eq!(state(&d, "t-b"), WaitingDependency);
            assert!(!d.view.board.tasks["t-b"].needs_start());
            assert_eq!(d.view.tasks_to_start().iter().map(|t| t.id.as_str()).collect::<Vec<_>>(), ["t-a"]);
            d.on(launched_for("t-a", "app/a")).on(seen_as("s", "app/a")).on(hook_of("s", UPS)).on(opened_by(PR, "s"));
            assert_eq!(state(&d, "t-b"), Running, "t-a has its PR open: t-b builds on it");
        }

        /// A node closed while its task waited for David's answer and its PR was open: the orphaned
        /// question stayed the task's blocker, and the merge left it needing David for good.
        #[test]
        fn a_node_closed_while_its_pr_waits_leaves_the_task_in_review_and_the_merge_finishes_it() {
            let mut d = Daemon::new();
            d.on(hook_of("s", UPS)).on(opened_by(PR, "s")).on(asked(1, "s", |q| q.about_pr = Some(7)));
            assert_eq!(state(&d, "t-s"), NeedsYou);
            d.on(Event::NodeRemoved { session_id: "s".into() });
            let t = &d.view.board.tasks["t-s"];
            assert_eq!((t.state, t.blocker), (InReview, None), "its PR waits for David; nobody is left to answer");
            d.on(Event::PrMerged { id: 1, method: MergeMethod::Squash });
            assert_eq!(state(&d, "t-s"), Done, "nobody is left to do what the merge unblocks");
            assert!(d.view.board.tasks["t-s"].anomalies.is_empty());
            assert_eq!(d.replayed(|_| true).board, d.view.board);
        }

        /// A merge is GitHub's fact: David merging an auto-merge PR by hand while a check ran, or a
        /// PR above one of its dependency's, left the task claiming it still waited.
        #[test]
        fn a_merge_on_github_moves_the_task_even_where_alondra_would_not_have_merged() {
            let mut d = Daemon::new();
            d.on(Event::Launched { name: "app/fix".into(), request_id: None, auto_merge: true, task: None, title: None, session: None }).on(seen("app/fix")).on(hook(UPS)).on(opened());
            d.github(1, |p| p.checks = vec![crate::pr::Check { name: "ci".into(), state: CheckState::Running, url: None }]);
            d.on(Event::PrMerged { id: 1, method: MergeMethod::Squash });
            let t = d.task("s");
            assert_eq!((t.state, t.anomalies.len()), (Closing, 0));
            assert_eq!(t.history.last().and_then(|h| h.note.as_deref()), Some("the checks of PR #7 have not passed"));

            let mut stacked = Daemon::new();
            stacked.on(defined("t-a", &[], "app/a")).on(defined("t-b", &["t-a"], "app/b"));
            stacked.on(launched_for("t-a", "app/a")).on(seen_as("a", "app/a")).on(hook_of("a", UPS)).on(opened_by(PR150, "a"));
            stacked.on(launched_for("t-b", "app/b")).on(seen_as("b", "app/b")).on(hook_of("b", UPS)).on(opened_by(PR151, "b"));
            stacked.github(2, |p| p.state = PrState::Merged);
            let t = &stacked.view.board.tasks["t-b"];
            assert_eq!((t.state, t.anomalies.len()), (Closing, 0));
            assert_eq!(t.history.last().and_then(|h| h.note.as_deref()), Some("merge PR #150 first"));
        }

        /// A final report, then a question and its answer: the node went back to work, so its
        /// report no longer says nothing follows the merge.
        #[test]
        fn a_question_after_the_final_report_means_work_follows_the_merge() {
            let mut d = Daemon::new();
            d.on(hook_of("s", UPS)).on(opened_by(PR, "s")).on(reported(1, "s", ReportKind::Final, &[PR]));
            assert!(d.view.merge_finishes_task(1));
            d.on(asked(1, "s", |q| q.about_pr = Some(7))).on(answered(1));
            assert_eq!(d.task("s").final_report, None);
            assert!(!d.view.merge_finishes_task(1), "the node gets the merge notice");
            d.on(Event::PrMerged { id: 1, method: MergeMethod::Squash });
            assert_eq!(d.task("s").state, Closing);
        }

        /// A node that opens a new PR for its done task has work on it again.
        #[test]
        fn a_new_pr_of_its_own_node_reopens_a_done_task() {
            let mut d = Daemon::new();
            d.on(hook_of("s", UPS)).on(reported(1, "s", ReportKind::Final, &[]));
            assert_eq!(d.task("s").state, Done);
            d.on(opened_by(PR, "s"));
            let t = d.task("s");
            assert_eq!((t.state, t.prs.clone(), t.anomalies.len()), (InReview, vec![1], 0));
            assert_eq!(t.history.iter().map(|h| h.event.name()).collect::<Vec<_>>(), ["final_report", "reopen", "pr_opened"]);
            assert_eq!(d.replayed(|_| true).board, d.view.board);
        }

        /// A launch made its task under the node's name; the orchestrator's title is what David reads.
        #[test]
        fn a_launched_task_goes_by_the_title_it_was_given_and_its_node_still_takes_it() {
            let mut d = Daemon::new();
            d.on(Event::Launched { name: "app/fix".into(), request_id: None, auto_merge: false, task: None, title: Some("Fix the login crash".into()), session: None });
            d.on(seen("app/fix")).on(hook(UPS)).on(question(1));
            let t = d.task("s");
            assert_eq!((t.title.as_str(), t.launch_name(), d.view.board.tasks.len()), ("Fix the login crash", "app/fix", 1));
            assert_eq!(d.replayed(|_| true).board, d.view.board);

            let mut plain = Daemon::new();
            plain.on(Event::Launched { name: "app/fix".into(), request_id: None, auto_merge: false, task: None, title: None, session: None }).on(seen("app/fix"));
            assert_eq!(plain.task("s").title, "app/fix", "without a title it goes by its node's name");
        }

        #[test]
        fn a_node_with_several_open_tasks_says_which_one_a_card_is_for() {
            let mut d = Daemon::new();
            d.on(hook_of("s", UPS)).on(asked(1, "s", |_| {}));
            let second = crate::tasks::NewTask { id: "t-2".into(), request: "r-1".into(), title: "Second".into(), brief: None, depends_on: vec![], options: Default::default(), session_id: Some("s".into()), launched: false, launched_as: None, start: None };
            d.on(Event::TaskCreated(second));
            assert_eq!(d.view.card_task("s", None, None), Err(CardTask::Several(vec!["t-2".into(), "t-s".into()])));
            assert_eq!(d.view.card_task("s", Some("t-2"), None), Ok("t-2".into()));
            assert_eq!(d.view.card_task("s", Some("t-9"), None), Err(CardTask::Unknown("t-9".into())));
            d.on(hook_of("o", UPS));
            assert_eq!(d.view.card_task("o", Some("t-2"), None), Err(CardTask::NotYours("t-2".into())));
            assert_eq!(d.refusal(asked(2, "s", |q| q.task = Some("t-2".into()))), None, "each task has its own blocker");
            d.on(asked(2, "s", |q| q.task = Some("t-2".into())));
            assert_eq!(d.view.board.tasks["t-2"].blocker.map(|b| b.question), Some(2));
            assert_eq!(d.view.board.tasks["t-s"].blocker.map(|b| b.question), Some(1));
        }

        const PR98: &str = "https://github.com/a/alondra/pull/98";
        const PR99: &str = "https://github.com/a/alondra/pull/99";

        /// Node s works on its own task t-s and on t-2, which the orchestrator gave it later: node
        /// 7e661a50 with its review and t-44370 on 25 Sep.
        fn two_tasks() -> Daemon {
            let mut d = Daemon::new();
            d.on(hook_of("s", UPS)).on(asked(1, "s", |_| {})).on(answered(1));
            let second = crate::tasks::NewTask { id: "t-2".into(), request: "r-1".into(), title: "Second".into(), brief: None, depends_on: vec![], options: Default::default(), session_id: Some("s".into()), launched: false, launched_as: None, start: None };
            d.on(Event::TaskCreated(second));
            d
        }

        fn pr_for(url: &str, task: Option<&str>) -> Event {
            Event::PrOpened { url: url.into(), session_id: "s".into(), task: task.map(Into::into) }
        }

        fn pr_task(d: &Daemon, id: u64) -> Option<String> {
            d.view.pr_task_of(&d.view.prs[&id]).map(|t| t.id.clone())
        }

        #[test]
        fn a_pr_goes_to_the_task_it_names_or_its_subagent_works_on_and_is_never_guessed() {
            let mut d = two_tasks();
            d.on(hook_of("o", UPS)).on(asked(2, "o", |_| {}));
            d.on(Event::Hook(HookEvent {
                session_id: "s".into(),
                event: "PostToolUse".into(),
                agent_id: Some("a1".into()),
                tool_name: Some("mcp__plugin_alondra_alondra__report".into()),
                task: Some("t-2".into()),
                ..Default::default()
            }));
            let cases: &[(&str, Option<&str>, &str, Option<&str>)] = &[
                ("the main thread, nothing named", None, "gh pr create --fill", None),
                ("the marker in the body", None, "gh pr create --title Fix --body \"Fixes.\n\nAlondra task: t-s\"", Some("t-s")),
                ("the marker in bold, the id in backticks", None, "--body '**Alondra task:** `t-2`'", Some("t-2")),
                ("the id alone, in the branch", None, "gh pr create --head review/t-s-fixes --fill", Some("t-s")),
                ("two ids, the marked one wins", None, "Builds on t-2. Alondra task: t-s", Some("t-s")),
                ("two ids and no marker", None, "t-2 and t-s", None),
                ("ids that only look like it", None, "t-22 at-2 t-2x", None),
                ("another node's task", None, "Alondra task: t-o", None),
                ("the subagent that bound itself to t-2", Some("a1"), "gh pr create --fill", Some("t-2")),
                ("what the PR says wins over the subagent", Some("a1"), "Alondra task: t-s", Some("t-s")),
            ];
            for (what, agent, text, task) in cases {
                assert_eq!(d.view.pr_task_for("s", *agent, &[text]).as_deref(), *task, "{what}");
            }
            assert_eq!(d.view.pr_task_for("o", None, &["gh pr create --fill"]).as_deref(), Some("t-o"), "a node with one open task needs no word");
        }

        /// A PR opened with no task waits for the node: a report naming its task, or GitHub's copy
        /// naming it, gives it one, and the task takes the PR as it is by then.
        #[test]
        fn a_pr_left_without_a_task_joins_the_one_a_report_or_its_body_names() {
            let mut d = two_tasks();
            d.on(pr_for(PR98, None)).on(pr_for(PR99, None));
            assert_eq!((pr_task(&d, 1), pr_task(&d, 2)), (None, None), "two open tasks and nothing said which");
            assert_eq!((state(&d, "t-s"), state(&d, "t-2")), (Running, Running));
            let asked = d.view.pr_task_question(PR98, 0).expect("the node is asked");
            assert!(asked.text.starts_with("[Alondra] PR #98: which task is it for?") && asked.text.contains("- t-2: Second") && asked.text.contains("- t-s: "), "{}", asked.text);

            d.on(pr_for(PR98, Some("t-s")));
            assert_eq!(pr_task(&d, 1).as_deref(), Some("t-s"), "a report linking it with task");
            assert_eq!((state(&d, "t-s"), d.view.pr_task_question(PR98, 0)), (InReview, None));

            d.github(2, |p| {
                p.state = PrState::Merged;
                p.body = Some("Redesign.\n\nAlondra task: t-2".into());
            });
            assert_eq!(pr_task(&d, 2).as_deref(), Some("t-2"), "GitHub's copy names it");
            assert_eq!(state(&d, "t-2"), Closing, "merged before it had a task: its task takes the merge");
            d.on(pr_for(PR98, Some("t-2")));
            assert_eq!(pr_task(&d, 1).as_deref(), Some("t-s"), "a PR with a task keeps it: moving it is David's");
            assert_eq!(d.replayed(|_| true).board, d.view.board);
        }

        /// 25 Sep 00:01: #98 and #99 of 7e661a50's review went to t-44370 while its subagent was
        /// still at work, and their merges sent it to closing.
        #[test]
        fn moving_a_pr_to_the_task_it_was_for_leaves_both_where_their_facts_say_and_replays_the_same() {
            let mut d = two_tasks();
            d.on(asked(2, "s", |q| q.task = Some("t-2".into()))).on(answered(2));
            d.on(pr_for(PR98, Some("t-2"))).on(pr_for(PR99, Some("t-2")));
            d.on(Event::PrMerged { id: 1, method: MergeMethod::Merge }).on(Event::PrMerged { id: 2, method: MergeMethod::Merge });
            assert_eq!((state(&d, "t-2"), state(&d, "t-s")), (Closing, Running), "where the wrong attribution left them");

            d.on(Event::PrReassigned { pr: 1, task: "t-s".into() });
            assert_eq!((state(&d, "t-2"), d.view.board.tasks["t-2"].prs.clone()), (Closing, vec![2]), "#99 is still t-2's");
            d.on(Event::PrReassigned { pr: 2, task: "t-s".into() });
            let (a, b) = (&d.view.board.tasks["t-s"], &d.view.board.tasks["t-2"]);
            let b_moves: Vec<&str> = b.history.iter().map(|h| h.event.name()).collect();
            assert_eq!((b.state, b.prs.clone(), b_moves), (Running, vec![], vec!["blocker_raised", "answered"]));
            assert_eq!((a.state, a.prs.clone()), (Closing, vec![1, 2]), "the review's task carries the merged PRs");
            assert_eq!((pr_task(&d, 1), pr_task(&d, 2)), (Some("t-s".into()), Some("t-s".into())));
            assert!(d.view.board.anomalies.is_empty());
            assert_eq!(d.replayed(|_| true).board, d.view.board, "a replay moves them the same");

            d.on(hook_of("o", UPS)).on(asked(3, "o", |_| {})).on(Event::PrReassigned { pr: 1, task: "t-o".into() });
            assert_eq!(pr_task(&d, 1).as_deref(), Some("t-s"), "never to another node's task");
        }
    }
}
