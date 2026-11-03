//! What a node shows. The node itself is only free, busy or waiting for the machine: the first
//! row of `NODE_TABLE` whose evidence is present decides it. Whatever else today's clients read in
//! its `state` (waiting for an answer, in review, done…) comes from its tasks (`from_tasks`).
//! Every row rests on a fact the node's hooks, its process or the Claude daemon reported; none
//! depends on how long ago that was, so the same events give the same states on any clock. The
//! `now` the caller passes only reaches the task rows that read GitHub's merge state.

use crate::model::{
    Evidence, MessageFrom, Millis, Node, NodeState, NodeStatus, OrchestratorState, PromptOrigin, QuestionStatus, ReportKind, Signal,
    TurnSignal,
};
use crate::pr::{PrState, PullRequest};
use crate::tasks::{BlockerKind, Task, TaskState};
use crate::view::View;

/// What made the reducer look at the node again.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Trigger {
    Hook,
    Daemon,
    ProcessExit,
    Question,
    Report,
    Declaration,
    Message,
    Other,
}

pub struct Row<S: 'static> {
    pub name: &'static str,
    /// States the row applies to; empty means any.
    pub from: &'static [S],
    /// Triggers the row applies to; empty means any.
    pub on: &'static [Trigger],
    pub when: Signal,
    pub to: S,
}

const fn row<S>(name: &'static str, when: Signal, to: S) -> Row<S> {
    Row { name, from: &[], on: &[], when, to }
}

/// The node's own evidence, tried in order; the first row that matches wins and the last one
/// always does.
pub const NODE_TABLE: &[Row<NodeState>] = &[
    row("a permission prompt is on screen", Signal::PermissionPrompt, NodeState::WaitingAnswer),
    row("a tool is in flight", Signal::ToolInFlight, NodeState::Working),
    row("a hook inside a turn", Signal::Hook, NodeState::Working),
    row("Alondra typed a message into the session and its turn has not ended", Signal::MessageHanded, NodeState::Working),
    row("the Claude daemon reports the session active", Signal::DaemonActive, NodeState::Working),
    row("a background task of the node is still running", Signal::BackgroundTask, NodeState::Working),
    row("a usage limit stopped the node", Signal::UsageLimit, NodeState::WaitingMachine),
    row("the node declared it waits for the machine", Signal::DeclaredWait, NodeState::WaitingMachine),
    row("nothing runs", Signal::Nothing, NodeState::Idle),
];

pub const ORCHESTRATOR_TABLE: &[Row<OrchestratorState>] = &[
    row("the orchestrator asked the user", Signal::OpenQuestion, OrchestratorState::WaitingForYou),
    row("a permission prompt is on the orchestrator's screen", Signal::PermissionPrompt, OrchestratorState::WaitingForYou),
    row("the orchestrator has no process", Signal::NoProcess, OrchestratorState::Offline),
    row("a tool is in flight", Signal::ToolInFlight, OrchestratorState::Handling),
    row("a recent hook inside a turn", Signal::Hook, OrchestratorState::Handling),
    row("Alondra just typed a message into the session", Signal::MessageHanded, OrchestratorState::Handling),
    row("the Claude daemon reports the session active", Signal::DaemonActive, OrchestratorState::Handling),
    row("a background task is still running", Signal::BackgroundTask, OrchestratorState::Handling),
    row("alive and idle", Signal::Nothing, OrchestratorState::OnDuty),
];

/// The evidence available for a node at a given time: each signal present, with the time it
/// was last confirmed.
#[derive(Debug, Default)]
pub struct Facts {
    signals: Vec<(Signal, Millis)>,
}

impl Facts {
    pub fn at(&self, signal: Signal) -> Option<Millis> {
        self.signals.iter().find(|(s, _)| *s == signal).map(|(_, at)| *at)
    }

    fn add(&mut self, signal: Signal, at: Option<Millis>) {
        if let Some(at) = at {
            self.signals.push((signal, at));
        }
    }
}

fn after_stop(node: &Node) -> impl Fn(&Millis) -> bool + '_ {
    |at| node.last_stop_at.is_none_or(|stop| *at > stop)
}

fn daemon_active(node: &Node) -> Option<Millis> {
    let active = node.alive && node.daemon_tempo.as_deref() == Some("active");
    node.daemon_seen_at.filter(|_| active).filter(after_stop(node))
}

/// The Claude daemon saw the session idle after `since`: nothing it started then still runs.
fn daemon_idle_after(node: &Node, since: Millis) -> bool {
    node.alive && node.daemon_tempo.as_deref() == Some("idle") && node.daemon_seen_at.is_some_and(|at| at > since)
}

pub fn facts(view: &View, node: &Node) -> Facts {
    let mut f = Facts::default();
    f.add(Signal::OpenQuestion, open_questions(view, node, false).map(|q| q.asked_at).max());
    f.add(Signal::PermissionPrompt, (node.waiting_permission || node.menu_waiting.is_some()).then_some(node.last_seen));

    let live = reopened(view, node);
    f.add(Signal::ToolInFlight, node.tool.as_ref().filter(|t| live && !daemon_idle_after(node, t.since)).map(|t| t.since));
    f.add(Signal::Hook, node.last_work_at.filter(|at| live && node.busy && !daemon_idle_after(node, *at)));
    f.add(Signal::MessageHanded, node.handed_at.filter(after_stop(node)).filter(|at| live && !daemon_idle_after(node, *at)));
    f.add(Signal::DaemonActive, daemon_active(node).filter(|_| live));
    let tasks_seen = if node.background.is_empty() {
        node.daemon_tasks_since.filter(|_| node.alive && node.daemon_tasks > 0)
    } else {
        node.daemon_seen_at.filter(|_| node.alive)
    };
    f.add(Signal::BackgroundTask, tasks_seen.filter(|_| live));

    f.add(Signal::UsageLimit, node.usage_limited_at.filter(|_| node.usage_limited));
    if node.turn_signal == Some(TurnSignal::WaitingMachine) {
        f.add(Signal::DeclaredWait, Some(node.signal_at.unwrap_or(node.episode_since)));
    }
    f.add(Signal::NoProcess, (!node.alive).then_some(node.last_seen));
    f.add(Signal::Nothing, Some(node.last_seen));
    f
}

/// Only the user or the orchestrator reopen a done node: a message of theirs typed in after it was
/// done, or the user typing in its terminal. What Alondra or Claude Code type into it moves nothing.
fn reopened(view: &View, node: &Node) -> bool {
    if node.state != NodeState::Done {
        return true;
    }
    let done = node.done_at.unwrap_or(node.state_since);
    let written = view.messages.values().any(|m| m.session_id == node.session_id && m.from != MessageFrom::Alondra && m.handed_at.is_some_and(|at| at > done));
    let typed = talking_with_user(node) && node.user_prompt_at.is_some_and(|at| at > done);
    written || typed
}

fn first<S: Copy + PartialEq>(table: &'static [Row<S>], facts: &Facts, current: S, trigger: Trigger) -> (&'static Row<S>, Millis) {
    table
        .iter()
        .find_map(|r| {
            let from = r.from.is_empty() || r.from.contains(&current);
            let on = r.on.is_empty() || r.on.contains(&trigger);
            let at = facts.at(r.when)?;
            (from && on).then_some((r, at))
        })
        .expect("the last row of a state table always matches")
}

#[derive(Debug, Clone, PartialEq)]
pub struct Outcome {
    pub state: NodeState,
    pub status: NodeStatus,
    pub orchestrator: Option<OrchestratorState>,
    pub evidence: Evidence,
    /// The Claude daemon saw the session idle after the tool the node opened: stop showing it.
    pub drop_tool: bool,
}

pub fn evaluate(view: &View, node: &Node, now: Millis, trigger: Trigger) -> Outcome {
    if node.session_id == crate::orchestrator::NODE_ID && let Some(runtime) = &view.orchestrator {
        let state = runtime.state();
        return Outcome {
            state: state.node_state(), status: if runtime.is_active() { NodeStatus::Busy } else { NodeStatus::Free },
            orchestrator: Some(state), evidence: Evidence { row: "durable orchestrator lifecycle".into(), signal: Signal::Nothing, at: runtime.updated_at }, drop_tool: !runtime.is_active(),
        };
    }
    let facts = facts(view, node);
    let drop_tool = node.tool.as_ref().is_some_and(|tool| daemon_idle_after(node, tool.since));
    let (own, own_at) = first(NODE_TABLE, &facts, node.state, trigger);
    let status = match own.to {
        NodeState::Working | NodeState::WaitingAnswer => NodeStatus::Busy,
        NodeState::WaitingMachine => NodeStatus::WaitingMachine,
        _ => NodeStatus::Free,
    };
    if node.is_orchestrator {
        let current = node.orchestrator_state.unwrap_or(OrchestratorState::OnDuty);
        let (row, at) = first(ORCHESTRATOR_TABLE, &facts, current, trigger);
        return Outcome {
            state: row.to.node_state(),
            status,
            orchestrator: Some(row.to),
            evidence: Evidence { row: row.name.to_string(), signal: row.when, at },
            drop_tool,
        };
    }
    let (state, evidence) = from_tasks(view, node, now, own, own_at).unwrap_or_else(|| (own.to, Evidence { row: own.name.to_string(), signal: own.when, at: own_at }));
    Outcome { state, status, orchestrator: None, evidence, drop_tool }
}

fn open_questions<'a>(view: &'a View, node: &'a Node, action: bool) -> impl Iterator<Item = &'a crate::model::Question> + 'a {
    view.questions
        .values()
        .filter(move |q| q.session_id == node.session_id && q.status == QuestionStatus::Open && q.blocking && !q.to_orchestrator && q.action == action)
}

pub const TASK_ASKS: &str = "a task of the node waits for the user's answer";
pub const TASK_ACTION: &str = "a task of the node waits for the user's action";
pub const MERGE_NOTICE_DUE: &str = "the merge of its PR has yet to reach it";
pub const REVIEW_REPLY_DUE: &str = "the user's review has yet to reach it";
pub const TASK_AWAITS_MERGE: &str = "its task has nothing left but a dependency's PR to see merged";
pub const TASK_AUTO_MERGING: &str = "Alondra waits on GitHub to merge a PR of its task";
pub const TASK_DELIVERED: &str = "its task runs and it delivered a report";
pub const TASK_IN_REVIEW: &str = "a task of the node waits for the user's review";
pub const TASK_WAITING: &str = "its task waits for another task";
pub const TASKS_FINISHED: &str = "every task of the node is finished";
/// Every row `from_tasks` can give, in the order it tries them.
pub const TASK_ROWS: &[&str] = &[
    TASK_ASKS,
    TASK_ACTION,
    MERGE_NOTICE_DUE,
    REVIEW_REPLY_DUE,
    TASK_AWAITS_MERGE,
    TASK_AUTO_MERGING,
    TASK_DELIVERED,
    TASK_IN_REVIEW,
    TASK_WAITING,
    TASKS_FINISHED,
];

/// What the node's tasks make of it, over what it shows of its own: waiting for the user's
/// answer or action (its task's blocker, or an open card the machine could not take), and, when
/// it is free, in review or done.
/// `None` leaves the node's own row.
fn from_tasks(view: &View, node: &Node, now: Millis, own: &Row<NodeState>, own_at: Millis) -> Option<(NodeState, Evidence)> {
    let said = |state: NodeState, row: &str, signal: Signal, at: Millis| Some((state, Evidence { row: row.to_string(), signal, at }));
    let tasks: Vec<&Task> = view.tasks_of(&node.session_id).collect();
    let blocked = |action: bool| {
        let by_task = tasks.iter().filter(|t| t.state == TaskState::NeedsYou).filter_map(|t| t.blocker).filter(|b| (b.kind == BlockerKind::Action) == action).map(|b| b.since).max();
        by_task.max(open_questions(view, node, action).map(|q| q.asked_at).max())
    };
    if let Some(at) = blocked(false) {
        return said(NodeState::WaitingAnswer, TASK_ASKS, Signal::OpenQuestion, at);
    }
    if own.when == Signal::PermissionPrompt {
        return None;
    }
    if let Some(at) = blocked(true) {
        return said(NodeState::YourTurn, TASK_ACTION, Signal::ActionRequested, at);
    }
    if own.to != NodeState::Idle {
        return None;
    }
    if let Some(at) = view.merge_notice_due(&node.session_id) {
        return said(NodeState::WaitingMerge, MERGE_NOTICE_DUE, Signal::PrAwaitingUser, at);
    }
    if let Some(at) = view.review_reply_due(&node.session_id) {
        return said(NodeState::WaitingMerge, REVIEW_REPLY_DUE, Signal::ReviewAwaitingUser, at);
    }
    let since = |states: &[TaskState]| tasks.iter().filter(|t| states.contains(&t.state)).map(|t| t.state_since).max();
    if let Some(at) = since(&[TaskState::Running, TaskState::Closing]) {
        let working = tasks.iter().filter(|t| matches!(t.state, TaskState::Running | TaskState::Closing));
        return if awaits_merge(view, node).is_some() {
            said(NodeState::WaitingMerge, TASK_AWAITS_MERGE, Signal::PrAwaitingUser, at)
        } else if tasks.iter().any(|t| view.auto_merge_pending(t, now)) {
            said(NodeState::WaitingMerge, TASK_AUTO_MERGING, Signal::PrAwaitingUser, at)
        } else if matches!(node.turn_signal, Some(TurnSignal::Delivered | TurnSignal::Finished)) && working.clone().any(|t| declared_for(node, t)) {
            said(NodeState::Delivered, TASK_DELIVERED, Signal::Report, node.signal_at.unwrap_or(at))
        } else {
            None
        };
    }
    if let Some(at) = since(&[TaskState::InReview]) {
        let pr_open = tasks.iter().filter(|t| t.state == TaskState::InReview).flat_map(|t| &t.prs).filter_map(|id| view.prs.get(id)).any(|p| p.state == PrState::Open);
        let signal = if pr_open { Signal::PrAwaitingUser } else { Signal::ReviewAwaitingUser };
        return said(NodeState::WaitingMerge, TASK_IN_REVIEW, signal, at);
    }
    if let Some(at) = since(&[TaskState::WaitingDependency]) {
        return said(NodeState::WaitingMerge, TASK_WAITING, Signal::PrAwaitingUser, at);
    }
    if !tasks.is_empty() && tasks.iter().all(|t| t.state.finished()) {
        let at = since(&[TaskState::Done, TaskState::Cancelled]).unwrap_or(own_at);
        return said(NodeState::Done, TASKS_FINISHED, Signal::FinalReport, at);
    }
    None
}

/// Whether the node shows work right now, ignoring what it is waiting for.
pub fn works(view: &View, node: &Node) -> bool {
    let f = facts(view, node);
    [Signal::ToolInFlight, Signal::Hook, Signal::MessageHanded, Signal::DaemonActive, Signal::BackgroundTask].iter().any(|s| f.at(*s).is_some())
}

/// Nodes in limbo at `now`: a background node whose state is out of date, or that is not
/// working and has no inbox item for its state, or that has an open question while showing
/// another state. Empty after every event.
pub fn limbo(view: &View, now: Millis) -> Vec<(String, &'static str)> {
    let mut found = vec![];
    for node in view.nodes.values().filter(|n| n.watched()) {
        let sid = &node.session_id;
        let outcome = evaluate(view, node, now, Trigger::Other);
        if outcome.state != node.state {
            found.push((sid.clone(), "its state is out of date"));
            continue;
        }
        let open = |action: bool| {
            view.questions.values().any(|q| {
                &q.session_id == sid && q.status.is_open() && q.blocking && !q.to_orchestrator && q.action == action
            })
        };
        let reported = |kinds: &[ReportKind]| {
            view.reports.values().any(|r| &r.session_id == sid && r.created_at >= node.episode_since && kinds.contains(&r.kind))
        };
        let blocked = |action: bool| {
            view.tasks_of(sid).any(|t| t.state == TaskState::NeedsYou && t.blocker.is_some_and(|b| (b.kind == BlockerKind::Action) == action))
        };
        let missing = match node.state {
            NodeState::Working => !works(view, node),
            NodeState::WaitingAnswer => !open(false) && !blocked(false) && !node.waiting_permission && node.menu_waiting.is_none(),
            NodeState::YourTurn => !open(true) && !blocked(true),
            NodeState::WaitingMerge => !matches!(outcome.evidence.signal, Signal::PrAwaitingUser | Signal::ReviewAwaitingUser),
            NodeState::WaitingMachine => !matches!(outcome.evidence.signal, Signal::UsageLimit | Signal::DeclaredWait),
            NodeState::Done => !view.tasks_of(sid).all(|t| t.state.finished()),
            NodeState::Delivered => !reported(&[ReportKind::Final, ReportKind::Notice, ReportKind::Document]),
            NodeState::Idle => false,
        };
        if missing {
            found.push((sid.clone(), "its state has no inbox item behind it"));
        }
        let asks = open(false) || open(true);
        if asks && !matches!(node.state, NodeState::WaitingAnswer | NodeState::YourTurn) {
            found.push((sid.clone(), "it has an open question but shows another state"));
        }
    }
    found
}

/// The user typed the node's last prompt in its own terminal or through Remote Control: they
/// are talking with it and have its answer on screen. What Alondra, a plugin or Claude Code
/// submitted, and a prompt whose origin no mod reported, do not count.
pub fn talking_with_user(node: &Node) -> bool {
    node.last_prompt_origin.is_some_and(PromptOrigin::is_user)
}

/// What the node declared in its episode covers the task: a declaration covers the task it
/// named, or every task of the node when it named none. A wait for the machine covers them all.
pub fn declared_for(node: &Node, task: &Task) -> bool {
    match node.turn_signal {
        None => false,
        Some(TurnSignal::WaitingMachine) => true,
        Some(_) => node.signal_task.as_deref().is_none_or(|t| t == task.id),
    }
}

/// The node's turn ended and nothing moves it: no work in sight, nothing on screen waiting for
/// anyone, nothing from the user, the orchestrator or a merge on its way to it.
fn quiet(view: &View, node: &Node) -> bool {
    let turn_ended = node.last_stop_at.is_some_and(|stop| stop >= node.episode_since && stop >= node.turn_since);
    node.watched()
        && node.has_plugin
        && turn_ended
        && !node.usage_limited
        && !node.waiting_permission
        && node.menu_waiting.is_none()
        && node.background.is_empty()
        && !crate::upkeep::died_working(node)
        && !crate::upkeep::stopped_on_purpose(node)
        && !works(view, node)
        && !view.deliverable_messages(&node.session_id).any(|m| m.from != MessageFrom::Alondra || view.is_merge_notice(m))
}

/// The running task of a quiet node that has nothing left but a dependency's PR to see merged,
/// and that PR: whatever the node said, the task waits for that merge. A node waiting for the
/// machine waits for the machine.
pub fn awaits_merge<'a>(view: &'a View, node: &Node) -> Option<(&'a Task, &'a PullRequest)> {
    if node.turn_signal == Some(TurnSignal::WaitingMachine) || !quiet(view, node) {
        return None;
    }
    view.tasks_of(&node.session_id).find_map(|t| Some((t, view.awaitable_merge(t)?)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::{DaemonObservation, Event, HookEvent};
    use crate::model::{BackgroundItem, BackgroundKind, Message, MessageFrom, Question, QuestionOrigin, Report};
    use crate::pr::CheckState;

    const T0: Millis = 1_000_000;

    fn hook(event: &str) -> Event {
        Event::Hook(HookEvent { session_id: "s".into(), event: event.into(), ..Default::default() })
    }

    fn tool(event: &str, id: &str) -> Event {
        Event::Hook(HookEvent { session_id: "s".into(), event: event.into(), tool_name: Some("Bash".into()), tool_use_id: Some(id.into()), ..Default::default() })
    }

    fn daemon(state: &str, tempo: &str, tasks: u32, needs: Option<&str>) -> Event {
        Event::DaemonSeen(DaemonObservation {
            session_id: "s".into(),
            short_id: "s".into(),
            pid: Some(7),
            state: Some(state.into()),
            tempo: Some(tempo.into()),
            needs: needs.map(str::to_string),
            tasks: Some(tasks),
            ..Default::default()
        })
    }

    fn daemon_running(items: &[(BackgroundKind, Millis)]) -> Event {
        let background = items.iter().map(|&(kind, started_at)| BackgroundItem { kind, label: "wait for the build".into(), started_at }).collect();
        let Event::DaemonSeen(d) = daemon("working", "idle", items.len() as u32, None) else { unreachable!() };
        Event::DaemonSeen(DaemonObservation { background, ..d })
    }

    fn question(action: bool, origin: QuestionOrigin) -> Event {
        Event::QuestionAsked(Question {
            id: 1,
            session_id: "s".into(),
            node_name: None,
            title: "t".into(),
            detail: None,
            options: vec![],
            links: vec![],
            attachments: vec![],
            blocking: true,
            origin,
            tool_use_id: None,
            to_orchestrator: false,
            node_context: None,
            status: QuestionStatus::Open,
            answer: None,
            asked_at: T0,
            action,
            steps: vec![],
            mission: None,
            replaces: None, about_pr: None,
            task: None,
        })
    }

    fn pr_opened() -> Event {
        Event::PrOpened { url: "https://github.com/a/b/pull/7".into(), session_id: "s".into(), task: None }
    }

    fn report(kind: ReportKind) -> Event {
        report_with(kind, false)
    }

    fn report_to_orchestrator(kind: ReportKind) -> Event {
        report_with(kind, true)
    }

    fn report_with(kind: ReportKind, to_orchestrator: bool) -> Event {
        Event::ReportPosted(Report {
            id: 1,
            session_id: "s".into(),
            node_name: None,
            title: "t".into(),
            body: None,
            links: vec![],
            attachments: vec![],
            kind,
            created_at: T0,
            read: false,
            to_orchestrator,
            mission: None,
            looks_like_question: false,
            task: None,
        })
    }

    fn usage_limit() -> Event {
        Event::Hook(HookEvent { session_id: "s".into(), event: "StopFailure".into(), error_type: Some("rate_limit".into()), ..Default::default() })
    }

    fn wait() -> Event {
        Event::WaitingMachine { session_id: "s".into(), reason: "RAM".into() }
    }

    fn message(from: MessageFrom) -> [Event; 2] {
        let m = Message { id: 9, session_id: "s".into(), from, text: "go on".into(), answers_question: None, created_at: T0, handed_at: None, delivered_at: None, about: None, attachments: vec![] };
        [Event::MessageQueued(m), Event::MessageHanded { id: 9 }]
    }

    /// Events one second apart from T0.
    fn run(events: &[Event]) -> View {
        spaced(events, 1000)
    }

    /// Events `gap` ms apart from T0.
    fn spaced(events: &[Event], gap: Millis) -> View {
        let mut v = View::default();
        for (i, e) in events.iter().enumerate() {
            v.apply(i as u64 + 1, T0 + i as Millis * gap, e);
        }
        v
    }

    struct Case {
        what: &'static str,
        events: Vec<Event>,
        state: NodeState,
        row: &'static str,
    }

    fn auto_merging(checks: CheckState) -> Vec<Event> {
        let named = Event::DaemonSeen(DaemonObservation { session_id: "s".into(), short_id: "s".into(), name: Some("n".into()), pid: Some(7), ..Default::default() });
        let mut pr = crate::pr::PullRequest::opened(1, "https://github.com/a/b/pull/7", "s", Some("n".into()), T0).unwrap();
        pr.fetched = true;
        pr.checks = vec![crate::pr::Check { name: "ci".into(), state: checks, url: None }];
        let launched = Event::Launched { name: "n".into(), request_id: None, auto_merge: true, task: None, title: None, session: None };
        vec![launched, named, hook("UserPromptSubmit"), pr_opened(), hook("Stop"), Event::PrUpdated(pr)]
    }

    fn row_named(to: NodeState, signal: Signal) -> &'static str {
        NODE_TABLE.iter().find(|r| r.to == to && r.when == signal && r.on.is_empty()).map(|r| r.name).unwrap()
    }

    fn cases() -> Vec<Case> {
        let min = 60_000;
        let [queued, handed] = message(MessageFrom::Alondra);
        let [notice, _] = merge_notice();
        let comments = verdict(1, crate::event::ReviewVerdict::Comments, 9);
        vec![
            Case { what: "an open ask", events: vec![hook("UserPromptSubmit"), question(false, QuestionOrigin::Ask), hook("Stop")], state: NodeState::WaitingAnswer, row: TASK_ASKS },
            Case { what: "a permission prompt", events: vec![hook("UserPromptSubmit"), hook("PermissionRequest")], state: NodeState::WaitingAnswer, row: row_named(NodeState::WaitingAnswer, Signal::PermissionPrompt) },
            Case { what: "an action for the user", events: vec![daemon("done", "idle", 0, None), question(true, QuestionOrigin::Classifier)], state: NodeState::YourTurn, row: TASK_ACTION },
            Case { what: "a long tool", events: vec![hook("UserPromptSubmit"), tool("PreToolUse", "t")], state: NodeState::Working, row: row_named(NodeState::Working, Signal::ToolInFlight) },
            Case { what: "a hook inside a turn", events: vec![hook("UserPromptSubmit")], state: NodeState::Working, row: row_named(NodeState::Working, Signal::Hook) },
            Case { what: "a nudge typed into an idle session", events: vec![hook("UserPromptSubmit"), hook("Stop"), queued, handed], state: NodeState::Working, row: row_named(NodeState::Working, Signal::MessageHanded) },
            Case { what: "the daemon says active after the Stop", events: vec![hook("UserPromptSubmit"), hook("Stop"), daemon("working", "active", 0, None)], state: NodeState::Working, row: row_named(NodeState::Working, Signal::DaemonActive) },
            Case { what: "a build left running in the background", events: vec![hook("UserPromptSubmit"), hook("Stop"), daemon_running(&[(BackgroundKind::Shell, T0 + 80 * min)])], state: NodeState::Working, row: row_named(NodeState::Working, Signal::BackgroundTask) },
            Case { what: "a wait loop is still work", events: vec![hook("UserPromptSubmit"), hook("Stop"), daemon_running(&[(BackgroundKind::Shell, T0)])], state: NodeState::Working, row: row_named(NodeState::Working, Signal::BackgroundTask) },
            Case { what: "a usage limit", events: vec![hook("UserPromptSubmit"), usage_limit()], state: NodeState::WaitingMachine, row: row_named(NodeState::WaitingMachine, Signal::UsageLimit) },
            Case { what: "a declared wait", events: vec![hook("UserPromptSubmit"), wait(), hook("Stop")], state: NodeState::WaitingMachine, row: row_named(NodeState::WaitingMachine, Signal::DeclaredWait) },
            Case { what: "an open PR card after the turn", events: vec![hook("UserPromptSubmit"), pr_opened(), report(ReportKind::Final), hook("Stop")], state: NodeState::WaitingMerge, row: TASK_IN_REVIEW },
            Case { what: "an artifact sent for review", events: vec![hook("UserPromptSubmit"), sent_for_review(1, ReportKind::Document), hook("Stop")], state: NodeState::WaitingMerge, row: TASK_IN_REVIEW },
            Case { what: "a final report", events: vec![hook("UserPromptSubmit"), report(ReportKind::Final), hook("Stop")], state: NodeState::Done, row: TASKS_FINISHED },
            Case { what: "a notice", events: vec![hook("UserPromptSubmit"), report(ReportKind::Notice), hook("Stop")], state: NodeState::Delivered, row: TASK_DELIVERED },
            Case { what: "a notice the user read", events: vec![hook("UserPromptSubmit"), report(ReportKind::Notice), hook("Stop"), Event::ReportRead { id: 1 }], state: NodeState::Delivered, row: TASK_DELIVERED },
            Case { what: "a handoff final report, nothing else pending", events: vec![hook("UserPromptSubmit"), report_to_orchestrator(ReportKind::Final), hook("Stop")], state: NodeState::Done, row: TASKS_FINISHED },
            Case { what: "a handoff notice, still working", events: vec![hook("UserPromptSubmit"), report_to_orchestrator(ReportKind::Notice), hook("Stop")], state: NodeState::Delivered, row: TASK_DELIVERED },
            Case { what: "merged, the notice on its way", events: background(&[hook("UserPromptSubmit"), pr_opened(), hook("Stop"), notice, merged()]), state: NodeState::WaitingMerge, row: MERGE_NOTICE_DUE },
            Case { what: "the user's comments on their way", events: [&[hook("UserPromptSubmit"), sent_for_review(1, ReportKind::Document), hook("Stop")][..], &comments[..2]].concat(), state: NodeState::WaitingMerge, row: REVIEW_REPLY_DUE },
            Case { what: "an auto-merge PR waiting on its checks", events: auto_merging(CheckState::Running), state: NodeState::WaitingMerge, row: TASK_AUTO_MERGING },
            Case { what: "a turn that ended with nothing", events: vec![hook("UserPromptSubmit"), hook("Stop")], state: NodeState::Idle, row: row_named(NodeState::Idle, Signal::Nothing) },
        ]
    }

    #[test]
    fn every_row_of_the_table_is_reached_and_asserted() {
        let cases = cases();
        for c in &cases {
            let v = run(&c.events);
            let n = &v.nodes["s"];
            assert_eq!(n.state, c.state, "{}", c.what);
            assert_eq!(n.evidence.as_ref().map(|e| e.row.as_str()), Some(c.row), "{}", c.what);
        }
        let mut waiting = run(&background(&[hook("UserPromptSubmit"), hook("Stop")]));
        waiting.board.tasks.get_mut("t-s").expect("the node's task").state = TaskState::WaitingDependency;
        waiting.recompute("s", T0 + 60_000, Trigger::Other);
        let n = &waiting.nodes["s"];
        assert_eq!((n.state, n.evidence.as_ref().map(|e| e.row.as_str())), (NodeState::WaitingMerge, Some(TASK_WAITING)), "a task waiting for another");
        let asserted_apart = [TASK_WAITING, TASK_AWAITS_MERGE];
        let reached = |name: &str| asserted_apart.contains(&name) || cases.iter().any(|c| c.row == name);
        for name in NODE_TABLE.iter().map(|r| r.name).chain(TASK_ROWS.iter().copied()) {
            assert!(reached(name), "no case reaches the row {name:?}");
        }
    }

    /// The user's rule: a state changes only on facts, never for how long it has lasted. Every case
    /// of the table, its events a second or two days apart, and read again an hour or two days
    /// after its last event, ends in the same state on the same row.
    #[test]
    fn the_same_events_on_any_clock_give_the_same_states() {
        let hour = 60 * 60_000;
        for c in cases() {
            for gap in [1000, 48 * hour] {
                let v = spaced(&c.events, gap);
                let n = &v.nodes["s"];
                let last = T0 + (c.events.len() as Millis - 1) * gap;
                for later in [0, hour, 48 * hour] {
                    let read = evaluate(&v, n, last + later, Trigger::Other);
                    assert_eq!((read.state, read.status, read.evidence.row.as_str()), (c.state, n.status, c.row),
                        "{}: events {gap} ms apart, read {later} ms after the last", c.what);
                    assert!(!read.drop_tool, "{}: no clock drops a tool", c.what);
                }
            }
        }
    }

    #[test]
    fn the_node_itself_is_only_free_busy_or_waiting_for_the_machine() {
        let cases: Vec<(&str, Vec<Event>, NodeStatus, NodeState)> = vec![
            ("inside a turn", vec![hook("UserPromptSubmit")], NodeStatus::Busy, NodeState::Working),
            ("on a permission prompt", vec![hook("UserPromptSubmit"), hook("PermissionRequest")], NodeStatus::Busy, NodeState::WaitingAnswer),
            ("stopped by a usage limit", vec![hook("UserPromptSubmit"), usage_limit()], NodeStatus::WaitingMachine, NodeState::WaitingMachine),
            ("its question waits for the user", background(&[hook("UserPromptSubmit"), question(false, QuestionOrigin::Ask), hook("Stop")]), NodeStatus::Free, NodeState::WaitingAnswer),
            ("its task is done", vec![hook("UserPromptSubmit"), report(ReportKind::Final), hook("Stop")], NodeStatus::Free, NodeState::Done),
            ("its turn ended with nothing declared", background(&[hook("UserPromptSubmit"), hook("Stop")]), NodeStatus::Free, NodeState::Idle),
        ];
        for (what, events, status, state) in cases {
            let n = run(&events).nodes["s"].clone();
            assert_eq!((n.status, n.state), (status, state), "{what}");
        }
    }

    #[test]
    fn a_background_task_counts_as_work_however_long_it_runs() {
        let started = [hook("UserPromptSubmit"), hook("Stop"), daemon_running(&[(BackgroundKind::Shell, T0), (BackgroundKind::Agent, T0)])];
        let later = spaced(&started, 48 * 60 * 60_000);
        assert_eq!(later.nodes["s"].state, NodeState::Working, "the shell started two days ago is still work");
        let v = run(&[hook("UserPromptSubmit"), hook("Stop"), daemon_running(&[(BackgroundKind::Shell, T0)]), daemon("working", "idle", 0, None)]);
        assert!(v.nodes["s"].background.is_empty(), "no task in flight means nothing runs, whatever the list says");
    }

    #[test]
    fn events_and_nodes_recorded_before_the_background_list_still_load() {
        let old = r#"{"type":"daemon_seen","session_id":"s","short_id":"s","tasks":2,"task_kinds":["local_bash"]}"#;
        let Event::DaemonSeen(d) = serde_json::from_str::<Event>(old).unwrap() else { panic!("not a daemon event") };
        assert!(d.background.is_empty());
        let mut node = serde_json::to_value(Node::new("s", T0)).unwrap();
        node.as_object_mut().unwrap().remove("background");
        assert!(serde_json::from_value::<Node>(node).unwrap().background.is_empty());
    }

    fn harness(prompt: &str) -> Event {
        Event::Hook(HookEvent { session_id: "s".into(), event: "UserPromptSubmit".into(), prompt: Some(prompt.into()), ..Default::default() })
    }

    fn background(events: &[Event]) -> Vec<Event> {
        let mut all = vec![daemon("working", "active", 0, None)];
        all.extend_from_slice(events);
        all
    }

    #[test]
    fn an_undeclared_end_of_turn_leaves_the_node_idle_and_only_declarations_and_cards_say_more() {
        let [queued, handed] = message(MessageFrom::User);
        let typed = Event::Hook(HookEvent { session_id: "s".into(), event: "UserPromptSubmit".into(), prompt: Some("what is left?".into()), ..Default::default() });
        let cases: Vec<(&str, Vec<Event>, NodeState)> = vec![
            ("ended its turn saying what it will do next", background(&[hook("UserPromptSubmit"), hook("Stop")]), NodeState::Idle),
            ("asked first", background(&[hook("UserPromptSubmit"), question(false, QuestionOrigin::Ask), hook("Stop")]), NodeState::WaitingAnswer),
            ("handed the user an action", background(&[hook("UserPromptSubmit"), question(true, QuestionOrigin::Ask), hook("Stop")]), NodeState::YourTurn),
            ("reported final", background(&[hook("UserPromptSubmit"), report(ReportKind::Final), hook("Stop")]), NodeState::Done),
            ("declared a machine wait", background(&[hook("UserPromptSubmit"), wait(), hook("Stop")]), NodeState::WaitingMachine),
            ("opened a PR, which has its card", background(&[hook("UserPromptSubmit"), pr_opened(), hook("Stop")]), NodeState::WaitingMerge),
            ("its PR was merged", background(&[hook("UserPromptSubmit"), pr_opened(), Event::PrMerged { id: 1, method: crate::pr::MergeMethod::Merge }, hook("Stop")]), NodeState::Idle),
            ("changes were requested on its PR", background(&[hook("UserPromptSubmit"), pr_opened(), Event::PrChangesRequested { id: 1 }, hook("Stop")]), NodeState::Idle),
            ("the user is typing in its terminal", background(&[typed, hook("Stop")]), NodeState::Idle),
            ("Claude Code typed a subagent's notice", background(&[harness("<task-notification>\n<task-id>a1</task-id>"), hook("Stop")]), NodeState::Idle),
            ("another session's message arrived", background(&[harness("<agent-message from=\"x\">done</agent-message>"), hook("Stop")]), NodeState::Idle),
            ("a foreground session", vec![hook("UserPromptSubmit"), hook("Stop")], NodeState::Idle),
            ("declared, then got a new message and stopped silently", background(&[hook("UserPromptSubmit"), wait(), hook("Stop"), queued, handed, hook("UserPromptSubmit"), hook("Stop")]), NodeState::Idle),
        ];
        for (what, events, state) in cases {
            assert_eq!(run(&events).nodes["s"].state, state, "{what}");
        }
    }

    fn typed(text: &str, origin: PromptOrigin) -> [Event; 2] {
        [Event::PromptSubmitted { session_id: "s".into(), origin }, harness(text)]
    }

    fn attach(on: bool) -> Event {
        Event::AttachChanged { session_id: "s".into(), attached: on }
    }

    fn menu(waiting: Option<&str>) -> Event {
        Event::MenuSeen { session_id: "s".into(), waiting_for: waiting.map(str::to_string) }
    }

    #[test]
    fn a_menu_keeps_the_node_waiting_for_an_answer_until_it_is_answered() {
        let cases: Vec<(&str, Vec<Event>, NodeState)> = vec![
            ("the daemon says it waits on a permission", background(&[hook("UserPromptSubmit"), menu(Some("permission prompt"))]), NodeState::WaitingAnswer),
            ("a permission request Alondra did not hold", background(&[hook("UserPromptSubmit"), hook("PermissionRequest")]), NodeState::WaitingAnswer),
            ("the menu is already a card", background(&[hook("UserPromptSubmit"), menu(Some("permission prompt")), question(false, QuestionOrigin::Permission)]), NodeState::WaitingAnswer),
            ("the user is at the terminal", background(&[hook("UserPromptSubmit"), attach(true), menu(Some("dialog open"))]), NodeState::WaitingAnswer),
            ("the menu was answered and work goes on", background(&[hook("UserPromptSubmit"), menu(Some("permission prompt")), tool("PreToolUse", "t")]), NodeState::Working),
            ("the daemon says nothing is pending", background(&[hook("UserPromptSubmit"), menu(Some("permission prompt")), menu(None)]), NodeState::Working),
            ("a foreground session", vec![hook("UserPromptSubmit"), hook("PermissionRequest")], NodeState::WaitingAnswer),
        ];
        for (what, events, state) in cases {
            let v = run(&events);
            assert_eq!(v.nodes["s"].state, state, "{what}");
        }
    }

    /// alondra/sin-dormidos (45b58bd9), 24 Sep: idle for hours, `daemon_tasks: 1`, `background: []`.
    /// The count is what the Claude daemon reports: it is work until the daemon reports none.
    #[test]
    fn a_task_count_is_work_until_the_daemon_reports_none_however_long_it_is_polled() {
        let min = 60_000;
        for listed in [true, false] {
            let mut v = run(&background(&[hook("UserPromptSubmit"), hook("Stop")]));
            let seen = |at: Millis| if listed { daemon_running(&[(BackgroundKind::Shell, at)]) } else { daemon("working", "idle", 1, None) };
            for (i, at) in (0..=180 * min).step_by((10 * min) as usize).enumerate() {
                v.apply(100 + i as u64, T0 + 10_000 + at, &seen(T0 + 10_000 + at));
            }
            assert_eq!(v.nodes["s"].state, NodeState::Working, "listed {listed}, polled for 3 h");
            v.apply(200, T0 + 200 * min, &daemon("working", "idle", 0, None));
            assert_eq!(v.nodes["s"].state, NodeState::Idle, "listed {listed}: the daemon reports nothing running");
        }
    }

    #[test]
    fn a_daemon_active_before_the_stop_does_not_count() {
        let v = run(&[hook("UserPromptSubmit"), daemon("working", "active", 0, None), hook("Stop")]);
        assert_eq!(v.nodes["s"].state, NodeState::Idle);
    }

    #[test]
    fn a_tool_stays_on_show_for_any_time_until_the_daemon_sees_the_session_idle_after_it() {
        let v = spaced(&[hook("UserPromptSubmit"), tool("PreToolUse", "t")], 48 * 60 * 60_000);
        assert!(v.nodes["s"].tool.is_some());
        assert_eq!(v.nodes["s"].state, NodeState::Working);
        let v = run(&[hook("UserPromptSubmit"), tool("PreToolUse", "t"), daemon("working", "idle", 0, None)]);
        assert!(v.nodes["s"].tool.is_none());
    }

    fn call(name: &str) -> Event {
        Event::Hook(HookEvent { session_id: "s".into(), event: "PreToolUse".into(), tool_name: Some(name.into()), tool_use_id: Some("c".into()), ..Default::default() })
    }

    #[test]
    fn a_call_of_any_kind_works_for_two_days() {
        for name in ["Bash", "Agent", "Task", "mcp__x__deploy"] {
            let v = spaced(&background(&[hook("UserPromptSubmit"), call(name)]), 48 * 60 * 60_000);
            assert_eq!(v.nodes["s"].state, NodeState::Working, "{name}");
            assert!(v.nodes["s"].tool.is_some(), "{name}: the call stays on show");
        }
    }

    #[test]
    fn a_declared_wait_holds_for_any_time_until_the_node_moves_on() {
        let v = spaced(&[hook("UserPromptSubmit"), wait(), hook("Stop")], 48 * 60 * 60_000);
        assert_eq!(v.nodes["s"].state, NodeState::WaitingMachine);
    }

    #[test]
    fn a_final_report_while_tools_run_is_working_until_the_turn_ends() {
        let mut v = run(&[hook("UserPromptSubmit"), report(ReportKind::Final), tool("PreToolUse", "t")]);
        assert_eq!(v.nodes["s"].state, NodeState::Working);
        v.apply(9, T0 + 9000, &hook("Stop"));
        assert_eq!(v.nodes["s"].state, NodeState::Done);
    }

    #[test]
    fn a_finished_task_is_not_undone_by_what_the_daemon_guesses() {
        let v = run(&[daemon("working", "active", 0, None), hook("UserPromptSubmit"), report(ReportKind::Final), hook("Stop"), daemon("blocked", "blocked", 0, Some("decide"))]);
        assert_eq!(v.nodes["s"].state, NodeState::Done, "the node's own final report finished its task");
    }

    #[test]
    fn a_message_from_the_user_ends_what_the_node_declared() {
        let [queued, handed] = message(MessageFrom::User);
        let v = run(&[hook("UserPromptSubmit"), wait(), hook("Stop"), queued, handed, hook("UserPromptSubmit"), hook("Stop")]);
        assert_eq!(v.nodes["s"].state, NodeState::Idle);
        let [queued, handed] = message(MessageFrom::Alondra);
        let v = run(&[hook("UserPromptSubmit"), wait(), hook("Stop"), queued, handed, hook("UserPromptSubmit"), hook("Stop")]);
        assert_eq!(v.nodes["s"].state, NodeState::WaitingMachine);
    }

    #[test]
    fn an_answered_plan_closes_the_tool_that_asked_it() {
        let mut q = question(false, QuestionOrigin::Plan);
        if let Event::QuestionAsked(q) = &mut q {
            q.tool_use_id = Some("plan".into());
        }
        let mut v = run(&[hook("UserPromptSubmit"), tool("PreToolUse", "plan"), q]);
        assert!(v.nodes["s"].tool.is_some());
        v.apply(9, T0 + 9000, &Event::QuestionClosed { id: 1, status: QuestionStatus::Withdrawn, note: None });
        assert!(v.nodes["s"].tool.is_none());
    }

    #[test]
    fn a_waking_node_without_the_plugin_starts_a_new_turn() {
        let mut v = run(&[daemon("done", "idle", 0, None), report(ReportKind::Notice)]);
        assert_eq!(v.nodes["s"].state, NodeState::Delivered);
        v.apply(9, T0 + 9000, &daemon("working", "active", 0, None));
        v.apply(10, T0 + 10_000, &daemon("done", "idle", 0, None));
        assert_eq!(v.nodes["s"].turn, 1);
        assert_eq!(v.nodes["s"].state, NodeState::Idle);
    }

    fn merge_notice() -> [Event; 2] {
        let m = Message {
            id: 9,
            session_id: "s".into(),
            from: MessageFrom::Alondra,
            text: "[Alondra] Ana merged it.".into(),
            answers_question: None,
            created_at: T0,
            handed_at: None,
            delivered_at: None,
            about: Some(crate::model::MessageAbout::PrMerged { pr_id: 1, task: None }),
            attachments: vec![],
        };
        [Event::MessageQueued(m), Event::MessageHanded { id: 9 }]
    }

    fn merged() -> Event {
        Event::PrMerged { id: 1, method: crate::pr::MergeMethod::Merge }
    }

    const PAGE: &str = "https://claude.ai/artifact/plan";

    fn sent_for_review(id: u64, kind: ReportKind) -> Event {
        let Event::ReportPosted(r) = report(kind) else { unreachable!() };
        Event::ReportPosted(Report { id, created_at: T0 + id as Millis * 10_000, links: vec![crate::model::Link { url: PAGE.into(), title: None }], ..r })
    }

    fn verdict(report: u64, verdict: crate::event::ReviewVerdict, message: u64) -> [Event; 3] {
        let m = Message {
            id: message,
            session_id: "s".into(),
            from: MessageFrom::User,
            text: "the user left comments".into(),
            answers_question: None,
            created_at: T0,
            handed_at: None,
            delivered_at: None,
            about: Some(crate::model::MessageAbout::ArtifactReview { report_id: report }),
            attachments: vec![],
        };
        [Event::ArtifactReviewed { report, verdict }, Event::MessageQueued(m), Event::MessageHanded { id: message }]
    }

    #[test]
    fn an_artifact_sent_for_review_waits_on_david_until_he_approves_it_or_sends_his_comments() {
        use crate::event::ReviewVerdict::{Approved, Comments};
        let sent = |kind| background(&[hook("UserPromptSubmit"), sent_for_review(1, kind), hook("Stop")]);
        let [comments, comments_queued, comments_handed] = verdict(1, Comments, 9);
        let [approved, approved_queued, approved_handed] = verdict(2, Approved, 10);
        let reviewed = |more: &[Event]| [sent(ReportKind::Document), vec![comments.clone(), comments_queued.clone(), comments_handed.clone()], more.to_vec()].concat();
        let cases: Vec<(&str, Vec<Event>, NodeState)> = vec![
            ("a document with the artifact", sent(ReportKind::Document), NodeState::WaitingMerge),
            ("a final report with the artifact is not done", sent(ReportKind::Final), NodeState::WaitingMerge),
            ("a progress report sends nothing for review", background(&[hook("UserPromptSubmit"), sent_for_review(1, ReportKind::Progress), hook("Stop")]), NodeState::Idle),
            ("the comments on their way", [sent(ReportKind::Document), vec![comments.clone(), comments_queued.clone()]].concat(), NodeState::WaitingMerge),
            ("the comments typed in", reviewed(&[]), NodeState::Working),
            ("republished for another round", reviewed(&[hook("UserPromptSubmit"), sent_for_review(2, ReportKind::Document), hook("Stop")]), NodeState::WaitingMerge),
            (
                "approved, then its final report links the page again",
                reviewed(&[hook("UserPromptSubmit"), sent_for_review(2, ReportKind::Document), hook("Stop"), approved.clone(), approved_queued.clone(), approved_handed.clone(), hook("UserPromptSubmit"), sent_for_review(3, ReportKind::Final), hook("Stop")]),
                NodeState::Done,
            ),
            ("the user sets it aside", [sent(ReportKind::Final), vec![Event::ReportRead { id: 1 }]].concat(), NodeState::Done),
            ("inside an open PR card the PR decides", background(&[hook("UserPromptSubmit"), pr_opened(), Event::ReportPosted(Report { body: Some(format!("{PAGE} and https://github.com/a/b/pull/7")), ..match sent_for_review(1, ReportKind::Final) { Event::ReportPosted(r) => r, _ => unreachable!() } }), hook("Stop")]), NodeState::WaitingMerge),
        ];
        for (what, events, state) in cases {
            let v = run(&events);
            assert_eq!(v.nodes["s"].state, state, "{what}");
            assert!(limbo(&v, T0 + events.len() as Millis * 1000).is_empty(), "{what}: {:?}", limbo(&v, T0 + events.len() as Millis * 1000));
        }
        let v = run(&reviewed(&[hook("UserPromptSubmit"), sent_for_review(2, ReportKind::Document), hook("Stop"), approved, approved_queued, approved_handed, hook("UserPromptSubmit"), sent_for_review(3, ReportKind::Final), hook("Stop")]));
        assert!(v.reports[&3].read, "a report about an approved artifact asks nothing of the user");
        let pr = run(&background(&[hook("UserPromptSubmit"), pr_opened(), Event::ReportPosted(Report { body: Some("https://github.com/a/b/pull/7".into()), ..match sent_for_review(1, ReportKind::Final) { Event::ReportPosted(r) => r, _ => unreachable!() } }), hook("Stop")]));
        assert_eq!(pr.nodes["s"].evidence.as_ref().map(|e| e.signal), Some(Signal::PrAwaitingUser), "the PR card, not a review card");
    }

    #[test]
    fn done_is_final_and_nothing_pending_ends_in_done() {
        let finished = || vec![hook("UserPromptSubmit"), report(ReportKind::Final), hook("Stop")];
        let then = |more: &[Event]| background(&[finished(), more.to_vec()].concat());
        let with_pr = |more: &[Event]| background(&[vec![hook("UserPromptSubmit"), pr_opened(), report(ReportKind::Final), hook("Stop")], more.to_vec()].concat());
        let [notice, notice_handed] = merge_notice();
        let [from_user, user_handed] = message(MessageFrom::User);
        let [from_orchestrator, orchestrator_handed] = message(MessageFrom::Orchestrator);
        let [from_alondra, alondra_handed] = message(MessageFrom::Alondra);
        let cases: Vec<(&str, Vec<Event>, NodeState)> = vec![
            ("final report with its PR open", with_pr(&[]), NodeState::WaitingMerge),
            ("final report with a question open", background(&[hook("UserPromptSubmit"), question(false, QuestionOrigin::Ask), report(ReportKind::Final), hook("Stop")]), NodeState::WaitingAnswer),
            ("final report with an action open", background(&[hook("UserPromptSubmit"), question(true, QuestionOrigin::Ask), report(ReportKind::Final), hook("Stop")]), NodeState::YourTurn),
            ("final report with its PR and a question open", background(&[hook("UserPromptSubmit"), pr_opened(), question(false, QuestionOrigin::Ask), report(ReportKind::Final), hook("Stop")]), NodeState::WaitingAnswer),
            ("merged, the notice on its way", with_pr(&[notice.clone(), merged()]), NodeState::WaitingMerge),
            ("merged, the notice typed in", with_pr(&[notice.clone(), merged(), notice_handed.clone()]), NodeState::Working),
            ("merged, acted on it and reported final", with_pr(&[notice.clone(), merged(), notice_handed.clone(), hook("UserPromptSubmit"), report(ReportKind::Final), hook("Stop")]), NodeState::Done),
            ("merged after its final report, acted on the notice all the same", with_pr(&[notice.clone(), merged(), notice_handed.clone(), hook("UserPromptSubmit"), hook("Stop")]), NodeState::Done),
            ("merged with work left after it, stopped without a word", background(&[hook("UserPromptSubmit"), pr_opened(), hook("Stop"), notice.clone(), merged(), notice_handed.clone(), hook("UserPromptSubmit"), hook("Stop")]), NodeState::Idle),
            ("merged on GitHub, nobody to tell", with_pr(&[merged()]), NodeState::Done),
            ("done", then(&[]), NodeState::Done),
            ("done, the user writes to it", then(&[from_user, user_handed]), NodeState::Working),
            ("done, the orchestrator sends it on", then(&[from_orchestrator, orchestrator_handed]), NodeState::Working),
            ("done, the user types in its terminal", then(&typed("one more thing", PromptOrigin::Composer)), NodeState::Working),
            ("done, the user pastes an Alondra message into its terminal", then(&typed("[Alondra] go on with the release", PromptOrigin::Composer)), NodeState::Working),
            ("done, the user writes through Remote Control", then(&typed("one more thing", PromptOrigin::Bridge)), NodeState::Working),
            ("done, a plugin submits a prompt as the user", then(&typed("one more thing", PromptOrigin::Other)), NodeState::Done),
            ("done, a prompt no mod reported the origin of", then(&[harness("one more thing")]), NodeState::Done),
            ("done, an Alondra message typed in all the same", then(&[from_alondra, alondra_handed]), NodeState::Done),
            ("done, Claude Code reports a finished task", then(&[harness("<task-notification>\n<task-id>a1</task-id>"), tool("PreToolUse", "t")]), NodeState::Done),
            ("done, another session's message arrives", then(&[harness("<agent-message from=\"x\">done</agent-message>"), tool("PreToolUse", "t")]), NodeState::Done),
            ("done, a cross-session message arrives", then(&[harness("<cross-session-message from=\"x\">hi</cross-session-message>")]), NodeState::Done),
            ("done, a reminder is typed in", then(&[harness("<system-reminder>check</system-reminder>")]), NodeState::Done),
            ("done, the user types after a harness prompt", then(&[vec![harness("<task-notification>a1"), hook("Stop")], typed("one more thing", PromptOrigin::Composer).to_vec()].concat()), NodeState::Working),
        ];
        let wrong: Vec<String> = cases
            .into_iter()
            .filter_map(|(what, events, state)| {
                let got = run(&events).nodes["s"].state;
                (got != state).then(|| format!("{what}: {got:?}, expected {state:?}"))
            })
            .collect();
        assert!(wrong.is_empty(), "{wrong:#?}");
    }

    #[test]
    fn only_the_terminal_and_remote_control_are_the_user_and_any_other_origin_is_not() {
        let origin = |kind: &str| serde_json::from_value::<PromptOrigin>(serde_json::json!(kind)).unwrap();
        assert_eq!(origin("composer"), PromptOrigin::Composer);
        assert_eq!(origin("bridge"), PromptOrigin::Bridge);
        for kind in ["plugin", "task-notification", "peer", "channel", "scheduled-trigger", "a-kind-from-a-newer-claude"] {
            assert_eq!(origin(kind), PromptOrigin::Other, "{kind}");
        }
    }

    #[test]
    fn what_earlier_builds_deduced_reads_as_a_valid_state_and_an_unknown_one_is_an_error() {
        assert!(serde_json::from_str::<NodeState>("\"being_watched\"").is_err());
        for (old, state) in [("unclear", NodeState::Idle), ("idle", NodeState::Idle), ("stalled", NodeState::Working),
            ("background_suspect", NodeState::Working), ("your_turn", NodeState::YourTurn)] {
            assert_eq!(serde_json::from_str::<NodeState>(&format!("\"{old}\"")).unwrap(), state, "{old}");
        }
        let o: OrchestratorState = serde_json::from_str("\"asleep\"").unwrap();
        assert_eq!(o, OrchestratorState::Unknown);
    }
}
