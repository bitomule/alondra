//! What a node shows. The node itself is only free, busy or waiting for the machine: the first
//! row of `NODE_TABLE` whose evidence is present decides it. Whatever else today's clients read in
//! its `state` (waiting for an answer, in review, done, stalled…) comes from its tasks
//! (`from_tasks`). Time only enters through the `now` the caller passes: the reducer passes the
//! time of the event it applies, and `Event::Clock` lets the daemon record in the log that time
//! has moved, so a replay reaches exactly the same states.

use crate::model::{
    BackgroundItem, BackgroundKind, Evidence, Message, MessageFrom, Millis, Node, NodeState, NodeStatus, OrchestratorState, QuestionStatus, ReportKind, Signal,
    TurnSignal,
};
use crate::pr::PrState;
use crate::tasks::{BlockerKind, Task, TaskState};
use crate::view::View;

const MINUTE: Millis = 60 * 1000;

/// A hook that shows the node inside a turn counts as work for this long.
pub const HOOK_FRESH_MS: Millis = 5 * MINUTE;
/// A single call counts as work for this long; past it, a background node is stuck in it.
pub const CALL_LIMIT_MS: Millis = 45 * MINUTE;
/// Subagents legitimately run longer than any other call.
pub const SUBAGENT_CALL_LIMIT_MS: Millis = 120 * MINUTE;

pub fn call_limit(tool: &str) -> Millis {
    if matches!(tool, "Agent" | "Task") { SUBAGENT_CALL_LIMIT_MS } else { CALL_LIMIT_MS }
}
/// The Claude daemon saying "active" counts for this long after it was last observed.
pub const DAEMON_FRESH_MS: Millis = 30 * MINUTE;
/// A background task (a Bash run in the background, a subagent, a monitor) reported by the
/// Claude daemon counts for this long after it was last observed.
pub const BACKGROUND_TASK_FRESH_MS: Millis = 120 * MINUTE;
/// A background task running this long, while the node shows no other work for as long, may be
/// hung (a wait loop that never ends): it is flagged and no longer counts as work.
pub const BACKGROUND_SUSPECT_MS: Millis = 30 * MINUTE;
/// A declared wait for the machine lapses when nothing happens for this long and the machine
/// is fine.
pub const MACHINE_WAIT_QUIET_MS: Millis = 20 * MINUTE;
/// After David types into a node's terminal, he counts as watching it for this long.
pub const WATCH_WINDOW_MS: Millis = 10 * MINUTE;
/// A usage limit (StopFailure) keeps the node waiting for this long at most.
pub const USAGE_LIMIT_WAIT_MS: Millis = 5 * 60 * MINUTE;

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
    Clock,
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

const WORKING: &[NodeState] = &[NodeState::Working];

/// The node's own evidence, tried in order; the first row that matches wins and the last one
/// always does. A row that ends in `unclear` leaves the node free.
pub const NODE_TABLE: &[Row<NodeState>] = &[
    row("a permission prompt is on screen", Signal::PermissionPrompt, NodeState::WaitingAnswer),
    row("a single call ran far longer than usual", Signal::CallStuck, NodeState::Stalled),
    row("a tool is in flight", Signal::ToolInFlight, NodeState::Working),
    row("a recent hook inside a turn", Signal::Hook, NodeState::Working),
    row("Alondra just typed a message into the session", Signal::MessageHanded, NodeState::Working),
    row("the Claude daemon reports the session active", Signal::DaemonActive, NodeState::Working),
    row("a background task of the node is still running", Signal::BackgroundTask, NodeState::Working),
    row("a usage limit stopped the node", Signal::UsageLimit, NodeState::WaitingMachine),
    row("the node declared it waits for the machine", Signal::DeclaredWait, NodeState::WaitingMachine),
    row("only possibly hung background tasks remain: handed to the sentinel", Signal::BackgroundSuspect, NodeState::Unclear),
    Row {
        name: "working evidence went stale: handed to the sentinel",
        from: WORKING,
        on: &[Trigger::Clock],
        when: Signal::Nothing,
        to: NodeState::Unclear,
    },
    row("idle with nothing to show: handed to the sentinel", Signal::Nothing, NodeState::Unclear),
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

fn fresh(at: Option<Millis>, now: Millis, window: Millis) -> Option<Millis> {
    at.filter(|at| now - at <= window)
}

fn after_stop(node: &Node) -> impl Fn(&Millis) -> bool + '_ {
    |at| node.last_stop_at.is_none_or(|stop| *at > stop)
}

fn daemon_active(node: &Node, now: Millis) -> Option<Millis> {
    let active = node.alive && node.daemon_tempo.as_deref() == Some("active");
    fresh(node.daemon_seen_at.filter(|_| active).filter(after_stop(node)), now, DAEMON_FRESH_MS)
}

/// Work the node shows besides its background tasks: a call in flight, a hook or a typed
/// message within the suspect window, or the daemon reporting it active.
fn other_work(node: &Node, now: Millis) -> bool {
    node.tool.as_ref().is_some_and(|t| now - t.since <= call_limit(&t.name))
        || fresh(node.last_work_at, now, BACKGROUND_SUSPECT_MS).is_some()
        || fresh(node.handed_at.filter(after_stop(node)), now, BACKGROUND_SUSPECT_MS).is_some()
        || daemon_active(node, now).is_some()
}

/// What the node has running in the background at `now`, each flagged when it may be hung.
pub fn background(node: &Node, now: Millis) -> Vec<BackgroundItem> {
    let idle = !other_work(node, now);
    node.background
        .iter()
        .map(|b| BackgroundItem { suspect: idle && now - b.started_at > BACKGROUND_SUSPECT_MS, ..b.clone() })
        .collect()
}

pub fn facts(view: &View, node: &Node, now: Millis) -> Facts {
    facts_with(view, node, now, view.machine_strained)
}

/// Only David or the orchestrator reopen a done node: a message of theirs typed in after it was
/// done, or David typing in its terminal. What Alondra or Claude Code type into it moves nothing.
fn reopened(view: &View, node: &Node) -> bool {
    if node.state != NodeState::Done {
        return true;
    }
    let done = node.done_at.unwrap_or(node.state_since);
    let written = view.messages.values().any(|m| m.session_id == node.session_id && m.from != MessageFrom::Alondra && m.handed_at.is_some_and(|at| at > done));
    let typed = talking_with_user(view, node) && node.last_prompt_at.is_some_and(|at| at > done);
    written || typed
}

fn facts_with(view: &View, node: &Node, now: Millis, machine_strained: bool) -> Facts {
    let mut f = Facts::default();
    f.add(Signal::OpenQuestion, open_questions(view, node, false).map(|q| q.asked_at).max());
    f.add(Signal::PermissionPrompt, (node.waiting_permission || node.menu_waiting.is_some()).then_some(node.last_seen));

    let live = reopened(view, node);
    f.add(Signal::CallStuck, node.tool.as_ref().filter(|t| live && node.watched() && now - t.since > call_limit(&t.name)).map(|t| t.since));
    f.add(Signal::ToolInFlight, node.tool.as_ref().filter(|t| live && now - t.since <= call_limit(&t.name)).map(|t| t.since));
    f.add(Signal::Hook, fresh(node.last_work_at.filter(|_| live && node.busy), now, HOOK_FRESH_MS));
    f.add(Signal::MessageHanded, fresh(node.handed_at.filter(after_stop(node)).filter(|_| live), now, HOOK_FRESH_MS));
    f.add(Signal::DaemonActive, daemon_active(node, now).filter(|_| live));
    let tasks_seen = if node.background.is_empty() {
        fresh(node.daemon_tasks_since.filter(|_| node.alive && node.daemon_tasks > 0), now, BACKGROUND_TASK_FRESH_MS)
    } else {
        fresh(node.daemon_seen_at.filter(|_| node.alive), now, BACKGROUND_TASK_FRESH_MS)
    };
    let all_suspect = !node.background.is_empty() && background(node, now).iter().all(|b| b.suspect);
    f.add(Signal::BackgroundTask, tasks_seen.filter(|_| live && !all_suspect));
    f.add(Signal::BackgroundSuspect, tasks_seen.filter(|_| all_suspect));

    f.add(Signal::UsageLimit, fresh(node.usage_limited_at.filter(|_| node.usage_limited), now, USAGE_LIMIT_WAIT_MS));
    let declared = node.signal_at.unwrap_or(node.episode_since);
    if node.turn_signal == Some(TurnSignal::WaitingMachine) {
        let quiet_since = declared.max(node.last_seen);
        let holds = machine_strained || now - quiet_since <= MACHINE_WAIT_QUIET_MS;
        f.add(Signal::DeclaredWait, holds.then_some(declared));
    }
    f.add(Signal::NoProcess, (!node.alive).then_some(node.last_seen));
    f.add(Signal::Nothing, Some(node.last_seen));
    f
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
    /// The tool the node opened is stale and nothing says it still runs: stop showing it.
    pub drop_tool: bool,
}

pub fn evaluate(view: &View, node: &Node, now: Millis, trigger: Trigger) -> Outcome {
    evaluate_with(view, node, now, trigger, view.machine_strained)
}

/// What the node would show at `now` if the machine were (or were not) strained.
pub fn evaluate_with(view: &View, node: &Node, now: Millis, trigger: Trigger, machine_strained: bool) -> Outcome {
    let facts = facts_with(view, node, now, machine_strained);
    let drop_tool = node.tool.is_some()
        && facts.at(Signal::ToolInFlight).is_none()
        && facts.at(Signal::CallStuck).is_none()
        && facts.at(Signal::DaemonActive).is_none()
        && facts.at(Signal::BackgroundTask).is_none();
    let (own, own_at) = first(NODE_TABLE, &facts, node.state, trigger);
    let status = match own.to {
        NodeState::Working | NodeState::Stalled | NodeState::WaitingAnswer => NodeStatus::Busy,
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
pub const TASK_IDLE: &str = "a task of the node should move and its turn ended without a declaration";
pub const TASK_AUTO_MERGING: &str = "Alondra waits on GitHub to merge a PR of its task";
pub const TASK_DELIVERED: &str = "its task runs and it delivered a report";
pub const TASK_IN_REVIEW: &str = "a task of the node waits for the user's review";
pub const TASK_WAITING: &str = "its task waits for another task";
pub const TASKS_FINISHED: &str = "every task of the node is finished";
/// Every row `from_tasks` can give, in the order it tries them.
pub const TASK_ROWS: &[&str] =
    &[TASK_ASKS, TASK_ACTION, MERGE_NOTICE_DUE, REVIEW_REPLY_DUE, TASK_IDLE, TASK_AUTO_MERGING, TASK_DELIVERED, TASK_IN_REVIEW, TASK_WAITING, TASKS_FINISHED];

/// What the node's tasks make of it, over what it shows of its own: waiting for the user's
/// answer or action (its task's blocker, or an open card the machine could not take), and, when
/// it is free, in review, done, or stalled when a task of it should move and it said nothing.
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
    if own.to != NodeState::Unclear {
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
        return if idle_task(view, node, now).is_some() {
            said(NodeState::Stalled, TASK_IDLE, Signal::TurnEndedUndeclared, node.last_stop_at.unwrap_or(at))
        } else if tasks.iter().any(|t| view.auto_merge_pending(t, now)) {
            said(NodeState::WaitingMerge, TASK_AUTO_MERGING, Signal::PrAwaitingUser, at)
        } else if matches!(node.turn_signal, Some(TurnSignal::Delivered | TurnSignal::Finished)) {
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
pub fn works(view: &View, node: &Node, now: Millis) -> bool {
    let f = facts(view, node, now);
    [Signal::ToolInFlight, Signal::Hook, Signal::MessageHanded, Signal::DaemonActive, Signal::BackgroundTask].iter().any(|s| f.at(*s).is_some())
}

/// Nodes in limbo at `now`: a background node whose state is out of date, or that is not
/// working and has no inbox item for its state and is not in the sentinel's hands, or that has
/// an open question while showing another state. Empty after every sweep.
pub fn limbo(view: &View, now: Millis) -> Vec<(String, &'static str)> {
    let mut found = vec![];
    for node in view.nodes.values().filter(|n| n.watched()) {
        let sid = &node.session_id;
        let outcome = evaluate(view, node, now, Trigger::Clock);
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
            NodeState::Working => !works(view, node, now),
            NodeState::WaitingAnswer => !open(false) && !blocked(false) && !node.waiting_permission && node.menu_waiting.is_none(),
            NodeState::YourTurn => !open(true) && !blocked(true),
            NodeState::WaitingMerge => !matches!(outcome.evidence.signal, Signal::PrAwaitingUser | Signal::ReviewAwaitingUser),
            NodeState::WaitingMachine => !matches!(outcome.evidence.signal, Signal::UsageLimit | Signal::DeclaredWait),
            NodeState::Done => !view.tasks_of(sid).all(|t| t.state.finished()),
            NodeState::Delivered => !reported(&[ReportKind::Final, ReportKind::Notice, ReportKind::Document]),
            NodeState::Unclear | NodeState::Stalled => false,
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

/// What Claude Code itself types into a session: a background task or subagent finishing, a
/// message from another session, a reminder. Nobody is at the terminal because of them.
const HARNESS_PROMPTS: &[&str] = &["<task-notification>", "<agent-message", "<cross-session-message", "<system-reminder>"];

/// The user typed the node's last prompt in its own terminal: they are talking with it and
/// have its answer on screen. Prompts Alondra or Claude Code typed, and the launch mission, do
/// not count.
pub fn talking_with_user(v: &View, node: &Node) -> bool {
    let squash = |t: &str| t.split_whitespace().collect::<Vec<_>>().join(" ");
    let Some(prompt) = node.last_prompt.as_deref().map(squash).filter(|p| !p.is_empty()) else { return false };
    if prompt.starts_with("[Alondra]")
        || HARNESS_PROMPTS.iter().any(|h| prompt.starts_with(h))
        || node.mission.as_deref().map(squash).is_some_and(|m| m == prompt)
    {
        return false;
    }
    let typed_by_alondra = v.messages.values().filter(|m| m.session_id == node.session_id).any(|m| {
        let head: String = squash(&m.text).chars().take(60).collect();
        !head.is_empty() && prompt.contains(&head)
    });
    !typed_by_alondra
}

/// David is handling the node himself: a terminal is attached to it, or he typed into it a
/// moment ago. While that lasts the node raises nothing in Alondra.
pub fn watching(v: &View, node: &Node, now: Millis) -> bool {
    node.attached || (talking_with_user(v, node) && node.last_prompt_at.is_some_and(|at| now - at <= WATCH_WINDOW_MS))
}

/// The node's terminal is parked on a menu (a permission, a question, a dialog) that no Alondra
/// question stands for, and nobody is at that terminal: it has to reach the clients as a card.
pub fn unseen_menu(v: &View, node: &Node, now: Millis) -> bool {
    let on_menu = node.waiting_permission || node.menu_waiting.is_some();
    let carded = v.questions.values().any(|q| q.session_id == node.session_id && q.status.is_open() && !q.to_orchestrator);
    node.in_daemon && on_menu && !carded && !watching(v, node, now)
}

pub const NUDGE_PREFIX: &str = "[Alondra] Your turn ended without telling";
const STALLED_PREFIX: &str = "[Alondra] Stalled node";

/// What Alondra does about a stalled node: nudge it once, and if it stalls again before anyone
/// writes to it, hand it to the orchestrator.
#[derive(Debug, Clone, PartialEq)]
pub enum StallMove {
    Nudge,
    TellOrchestrator { orchestrator: String },
}

pub fn stall_move(view: &View, node: &Node) -> Option<StallMove> {
    if node.state != NodeState::Stalled || node.attached {
        return None;
    }
    let orchestrator = || view.nodes.values().find(|n| n.is_orchestrator && n.alive);
    let told = |orchestrator: &Node, tag: &str| {
        view.messages.values().any(|m| m.session_id == orchestrator.session_id && m.from == MessageFrom::Alondra && m.text.starts_with(STALLED_PREFIX) && m.text.contains(tag))
    };
    if let Some(tool) = node.tool.as_ref().filter(|_| node.evidence.as_ref().is_some_and(|e| e.signal == Signal::CallStuck)) {
        let o = orchestrator()?;
        return (!told(o, &stuck_tag(node, tool.since))).then(|| StallMove::TellOrchestrator { orchestrator: o.session_id.clone() });
    }
    nudge_then_orchestrator(view, node)
}

/// The running (or closing: after its merge) task of a node idle when it should work: its turn
/// ended, it shows no work and it declared nothing. A task whose auto-merge PR Alondra is waiting on GitHub to merge waits on
/// GitHub, not on its node; a node whose process died is restarted instead (`upkeep`).
pub fn idle_task<'a>(view: &'a View, node: &Node, now: Millis) -> Option<&'a Task> {
    let turn_ended = node.last_stop_at.is_some_and(|stop| stop >= node.episode_since && stop >= node.turn_since);
    let idle = node.watched()
        && node.has_plugin
        && turn_ended
        && node.turn_signal.is_none()
        && !node.usage_limited
        && !node.waiting_permission
        && node.menu_waiting.is_none()
        && node.background.is_empty()
        && !crate::upkeep::died_working(node)
        && !crate::upkeep::stopped_on_purpose(node)
        && !watching(view, node, now)
        && !works(view, node, now)
        && !view.deliverable_messages(&node.session_id).any(|m| m.from != MessageFrom::Alondra || view.is_merge_notice(m));
    if !idle {
        return None;
    }
    view.tasks_of(&node.session_id).find(|t| matches!(t.state, TaskState::Running | TaskState::Closing) && !view.auto_merge_pending(t, now))
}

/// What Alondra does about a node idle when it should work: what it does about a stalled one,
/// keyed on its running task instead of its state.
pub fn idle_move(view: &View, node: &Node, now: Millis) -> Option<StallMove> {
    idle_task(view, node, now)?;
    nudge_then_orchestrator(view, node)
}

/// Nudge the node once in its episode; if it stops again undeclared after the nudge reached it,
/// hand it to the orchestrator.
fn nudge_then_orchestrator(view: &View, node: &Node) -> Option<StallMove> {
    let since = node.episode_since;
    let alondra_wrote = |session: &str, prefix: &str, about: Option<&str>| {
        view.messages.values().any(|m| {
            m.session_id == session && m.from == MessageFrom::Alondra && m.created_at >= since && m.text.starts_with(prefix) && about.is_none_or(|a| m.text.contains(a))
        })
    };
    let nudge = view.messages.values().filter(|m| m.session_id == node.session_id && m.from == MessageFrom::Alondra && m.created_at >= since && m.text.starts_with(NUDGE_PREFIX)).last();
    let Some(nudge) = nudge else { return Some(StallMove::Nudge) };
    let stopped_after_nudge = nudge.handed_at.zip(node.last_stop_at).is_some_and(|(handed, stop)| stop > handed);
    if !stopped_after_nudge {
        return None;
    }
    let orchestrator = view.nodes.values().find(|n| n.is_orchestrator && n.alive)?;
    let tag = stalled_tag(node);
    (!alondra_wrote(&orchestrator.session_id, STALLED_PREFIX, Some(&tag))).then(|| StallMove::TellOrchestrator { orchestrator: orchestrator.session_id.clone() })
}

fn stalled_tag(node: &Node) -> String {
    format!("({})", node.short_id)
}

fn stuck_tag(node: &Node, since: Millis) -> String {
    format!("({} call@{since})", node.short_id)
}

pub fn stuck_text(node: &Node, user: &str, now: Millis) -> Option<String> {
    let tool = node.tool.as_ref()?;
    let minutes = (now - tool.since) / MINUTE;
    let call = format!("{} {}", tool.name, tool.summary.as_deref().unwrap_or("")).trim().to_string();
    Some(format!(
        "{STALLED_PREFIX}: {} {} has been inside one call for {minutes} min: {call}. If it is hung, `interrupt` \
         stops the call and `send` moves the node on; a legitimately long call is left alone. When you cannot tell, it is a question for {user}.",
        node.display_name(),
        stuck_tag(node, tool.since)
    ))
}

pub fn nudge_text(user: &str) -> String {
    format!(
        "{NUDGE_PREFIX} {user} anything through Alondra, so they cannot tell whether you are \
         waiting on them; your terminal does not reach them. The Alondra tools declare your state: `ask` for a decision \
         (one question per decision, with options), `request_action` for something {user} does outside Alondra, \
         `report` with kind final when you are done, `wait_for_machine` when you are waiting for resources. If you are \
         still working on your own (a build, an upload, a review, a long command), there is nothing to send."
    )
}

pub fn stalled_text(node: &Node, user: &str) -> String {
    let said: String = node.last_message.as_deref().unwrap_or("(nothing)").chars().take(1500).collect();
    format!(
        "{STALLED_PREFIX}: {} {} ended its turn twice without declaring anything, and Alondra already nudged it once. \
         Its last message:\n\n{said}\n\n`send` moves it on when that message tells you what comes next; otherwise it is \
         a question for {user}.",
        node.display_name(),
        stalled_tag(node)
    )
}

/// Shares the stalled prefix and tag, so the orchestrator hears about a node once whichever way
/// it was found idle.
pub fn idle_text(node: &Node, task: &Task, user: &str) -> String {
    let said: String = node.last_message.as_deref().unwrap_or("(nothing)").chars().take(1500).collect();
    format!(
        "{STALLED_PREFIX}: {} {} ended its turn twice without declaring anything while its task “{}” is running, and \
         Alondra already nudged it once. Its last message:\n\n{said}\n\n`send` moves it on when that message tells you what \
         comes next; otherwise it is a question for {user}.",
        node.display_name(),
        stalled_tag(node),
        task.title
    )
}

/// The message a stall move, or a move on a node idle when it should work, puts in a session.
pub fn stall_message(view: &View, node: &Node, user: &str, at: Millis) -> Option<Message> {
    let stuck = node.evidence.as_ref().is_some_and(|e| e.signal == Signal::CallStuck);
    let task = idle_task(view, node, at);
    let (session_id, text) = match stall_move(view, node).or_else(|| task.and_then(|_| nudge_then_orchestrator(view, node)))? {
        StallMove::Nudge => (node.session_id.clone(), nudge_text(user)),
        StallMove::TellOrchestrator { orchestrator } if stuck => (orchestrator, stuck_text(node, user, at)?),
        StallMove::TellOrchestrator { orchestrator } => (orchestrator, task.map_or_else(|| stalled_text(node, user), |t| idle_text(node, t, user))),
    };
    view.alondra_message(&session_id, text, at)
}

pub const HUNG_PREFIX: &str = "[Alondra] A background task may be hung";
/// How long a node has, after Alondra's message reached it, to stop a hung task before the
/// orchestrator hears about it.
pub const HUNG_REPLY_MS: Millis = 20 * MINUTE;

fn hung_tag(node: &Node, item: &BackgroundItem) -> String {
    format!("({} bg@{})", node.short_id, item.started_at)
}

fn kind_word(kind: BackgroundKind) -> &'static str {
    match kind {
        BackgroundKind::Agent => "subagent",
        BackgroundKind::Shell => "shell",
        BackgroundKind::Other => "task",
    }
}

fn hung_node_text(item: &BackgroundItem, tag: &str, now: Millis) -> String {
    format!(
        "{HUNG_PREFIX}: your background {} \"{}\" {tag} has been running for {} min while you do nothing else. If it is \
         stuck (a wait loop that can never end, a watcher on something that already finished), stop it with the tool that \
         stops background tasks and carry on. If it is legitimately long, leave it.",
        kind_word(item.kind),
        item.label,
        (now - item.started_at) / MINUTE
    )
}

fn hung_orchestrator_text(node: &Node, item: &BackgroundItem, tag: &str, user: &str, now: Millis) -> String {
    format!(
        "{HUNG_PREFIX}: {} {tag} still has its background {} \"{}\" running after {} min, and Alondra asked it {} min ago \
         to stop it if it is stuck. `send` tells the node what to do; `interrupt` does not stop background tasks. When you \
         cannot tell whether it is legitimate, it is a question for {user}.",
        node.display_name(),
        kind_word(item.kind),
        item.label,
        (now - item.started_at) / MINUTE,
        HUNG_REPLY_MS / MINUTE
    )
}

/// The next message about a background task that may be hung: first to the node, then, if the
/// task is still hung `HUNG_REPLY_MS` after that message reached the node, to the orchestrator.
pub fn hung_message(view: &View, node: &Node, user: &str, at: Millis) -> Option<Message> {
    if !node.alive || node.attached || node.is_orchestrator || watching(view, node, at) {
        return None;
    }
    let wrote = |session: &str, tag: &str| {
        view.messages.values().filter(|m| m.session_id == session && m.from == MessageFrom::Alondra && m.text.starts_with(HUNG_PREFIX) && m.text.contains(tag)).last()
    };
    let orchestrator = view.nodes.values().find(|n| n.is_orchestrator && n.alive);
    let (session_id, text) = background(node, at).into_iter().filter(|b| b.suspect).find_map(|b| {
        let tag = hung_tag(node, &b);
        match wrote(&node.session_id, &tag) {
            None => Some((node.session_id.clone(), hung_node_text(&b, &tag, at))),
            Some(told) => {
                let o = orchestrator?;
                let ignored = told.handed_at.is_some_and(|handed| at - handed > HUNG_REPLY_MS);
                (ignored && wrote(&o.session_id, &tag).is_none()).then(|| (o.session_id.clone(), hung_orchestrator_text(node, &b, &tag, user, at)))
            }
        }
    })?;
    view.alondra_message(&session_id, text, at)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::{DaemonObservation, Event, HookEvent};
    use crate::model::{BackgroundKind, Message, MessageFrom, Question, QuestionOrigin, Report};
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
        let background = items.iter().map(|&(kind, started_at)| BackgroundItem { kind, label: "wait for the build".into(), started_at, suspect: false }).collect();
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
            replaces: None,
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

    /// Events one second apart from T0, then an optional sweep `after` ms past the last one.
    fn run(events: &[Event], after: Option<(Millis, bool)>) -> View {
        let mut v = View::default();
        let mut at = T0;
        for (i, e) in events.iter().enumerate() {
            at = T0 + i as Millis * 1000;
            v.apply(i as u64 + 1, at, e);
        }
        if let Some((later, strained)) = after {
            v.apply(events.len() as u64 + 1, at + later, &Event::Clock { machine_strained: strained });
        }
        v
    }

    struct Case {
        what: &'static str,
        events: Vec<Event>,
        sweep: Option<(Millis, bool)>,
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
        let stale = NODE_TABLE.iter().find(|r| !r.on.is_empty()).unwrap().name;
        let [queued, handed] = message(MessageFrom::Alondra);
        let [notice, _] = merge_notice();
        let comments = verdict(1, crate::event::ReviewVerdict::Comments, 9);
        vec![
            Case { what: "an open ask", events: vec![hook("UserPromptSubmit"), question(false, QuestionOrigin::Ask), hook("Stop")], sweep: None, state: NodeState::WaitingAnswer, row: TASK_ASKS },
            Case { what: "a permission prompt", events: vec![hook("UserPromptSubmit"), hook("PermissionRequest")], sweep: None, state: NodeState::WaitingAnswer, row: row_named(NodeState::WaitingAnswer, Signal::PermissionPrompt) },
            Case { what: "an action for the user", events: vec![daemon("done", "idle", 0, None), question(true, QuestionOrigin::Classifier)], sweep: None, state: NodeState::YourTurn, row: TASK_ACTION },
            Case { what: "a long tool, no hook for 10 min", events: vec![hook("UserPromptSubmit"), tool("PreToolUse", "t")], sweep: Some((10 * min, false)), state: NodeState::Working, row: row_named(NodeState::Working, Signal::ToolInFlight) },
            Case { what: "a hook a minute ago", events: vec![hook("UserPromptSubmit")], sweep: Some((min, false)), state: NodeState::Working, row: row_named(NodeState::Working, Signal::Hook) },
            Case { what: "a nudge typed into an idle session", events: vec![hook("UserPromptSubmit"), hook("Stop"), queued, handed], sweep: None, state: NodeState::Working, row: row_named(NodeState::Working, Signal::MessageHanded) },
            Case { what: "the daemon says active after the Stop", events: vec![hook("UserPromptSubmit"), hook("Stop"), daemon("working", "active", 0, None)], sweep: None, state: NodeState::Working, row: row_named(NodeState::Working, Signal::DaemonActive) },
            Case { what: "a build left running in the background", events: vec![hook("UserPromptSubmit"), hook("Stop"), daemon_running(&[(BackgroundKind::Shell, T0 + 80 * min)])], sweep: Some((90 * min, false)), state: NodeState::Working, row: row_named(NodeState::Working, Signal::BackgroundTask) },
            Case { what: "a wait loop left alone for 40 min", events: vec![hook("UserPromptSubmit"), hook("Stop"), daemon_running(&[(BackgroundKind::Shell, T0)])], sweep: Some((40 * min, false)), state: NodeState::Unclear, row: row_named(NodeState::Unclear, Signal::BackgroundSuspect) },
            Case { what: "a usage limit", events: vec![hook("UserPromptSubmit"), usage_limit()], sweep: None, state: NodeState::WaitingMachine, row: row_named(NodeState::WaitingMachine, Signal::UsageLimit) },
            Case { what: "a declared wait", events: vec![hook("UserPromptSubmit"), wait(), hook("Stop")], sweep: Some((10 * min, false)), state: NodeState::WaitingMachine, row: row_named(NodeState::WaitingMachine, Signal::DeclaredWait) },
            Case { what: "a declared wait on a strained machine", events: vec![hook("UserPromptSubmit"), wait(), hook("Stop")], sweep: Some((3 * 60 * min, true)), state: NodeState::WaitingMachine, row: row_named(NodeState::WaitingMachine, Signal::DeclaredWait) },
            Case { what: "an open PR card after the turn", events: vec![hook("UserPromptSubmit"), pr_opened(), report(ReportKind::Final), hook("Stop")], sweep: None, state: NodeState::WaitingMerge, row: TASK_IN_REVIEW },
            Case { what: "an artifact sent for review", events: vec![hook("UserPromptSubmit"), sent_for_review(1, ReportKind::Document), hook("Stop")], sweep: None, state: NodeState::WaitingMerge, row: TASK_IN_REVIEW },
            Case { what: "a final report", events: vec![hook("UserPromptSubmit"), report(ReportKind::Final), hook("Stop")], sweep: None, state: NodeState::Done, row: TASKS_FINISHED },
            Case { what: "a notice", events: vec![hook("UserPromptSubmit"), report(ReportKind::Notice), hook("Stop")], sweep: None, state: NodeState::Delivered, row: TASK_DELIVERED },
            Case { what: "a notice the user read", events: vec![hook("UserPromptSubmit"), report(ReportKind::Notice), hook("Stop"), Event::ReportRead { id: 1 }], sweep: None, state: NodeState::Delivered, row: TASK_DELIVERED },
            Case { what: "a handoff final report, nothing else pending", events: vec![hook("UserPromptSubmit"), report_to_orchestrator(ReportKind::Final), hook("Stop")], sweep: None, state: NodeState::Done, row: TASKS_FINISHED },
            Case { what: "a handoff notice, still working", events: vec![hook("UserPromptSubmit"), report_to_orchestrator(ReportKind::Notice), hook("Stop")], sweep: None, state: NodeState::Delivered, row: TASK_DELIVERED },
            Case { what: "a background call past its limit", events: background(&[hook("UserPromptSubmit"), tool("PreToolUse", "t")]), sweep: Some((46 * min, false)), state: NodeState::Stalled, row: row_named(NodeState::Stalled, Signal::CallStuck) },
            Case { what: "a foreground tool nobody closed", events: vec![hook("UserPromptSubmit"), tool("PreToolUse", "t")], sweep: Some((46 * min, false)), state: NodeState::Unclear, row: stale },
            Case { what: "a background node that ended its turn without declaring", events: vec![daemon("working", "active", 0, None), hook("UserPromptSubmit"), hook("Stop")], sweep: Some((30 * min, false)), state: NodeState::Stalled, row: TASK_IDLE },
            Case { what: "merged, the notice on its way", events: background(&[hook("UserPromptSubmit"), pr_opened(), hook("Stop"), notice, merged()]), sweep: None, state: NodeState::WaitingMerge, row: MERGE_NOTICE_DUE },
            Case { what: "the user's comments on their way", events: [&[hook("UserPromptSubmit"), sent_for_review(1, ReportKind::Document), hook("Stop")][..], &comments[..2]].concat(), sweep: None, state: NodeState::WaitingMerge, row: REVIEW_REPLY_DUE },
            Case { what: "an auto-merge PR waiting on its checks", events: auto_merging(CheckState::Running), sweep: None, state: NodeState::WaitingMerge, row: TASK_AUTO_MERGING },
            Case { what: "a turn that ended with nothing", events: vec![hook("UserPromptSubmit"), hook("Stop")], sweep: None, state: NodeState::Unclear, row: row_named(NodeState::Unclear, Signal::Nothing) },
        ]
    }

    #[test]
    fn every_row_of_the_table_is_reached_and_asserted() {
        let cases = cases();
        for c in &cases {
            let v = run(&c.events, c.sweep);
            let n = &v.nodes["s"];
            assert_eq!(n.state, c.state, "{}", c.what);
            assert_eq!(n.evidence.as_ref().map(|e| e.row.as_str()), Some(c.row), "{}", c.what);
        }
        let mut waiting = run(&background(&[hook("UserPromptSubmit"), hook("Stop")]), None);
        waiting.board.tasks.get_mut("t-s").expect("the node's task").state = TaskState::WaitingDependency;
        waiting.apply(99, T0 + 60_000, &Event::Clock { machine_strained: false });
        let n = &waiting.nodes["s"];
        assert_eq!((n.state, n.evidence.as_ref().map(|e| e.row.as_str())), (NodeState::WaitingMerge, Some(TASK_WAITING)), "a task waiting for another");
        let reached = |name: &str| name == TASK_WAITING || cases.iter().any(|c| c.row == name);
        for name in NODE_TABLE.iter().map(|r| r.name).chain(TASK_ROWS.iter().copied()) {
            assert!(reached(name), "no case reaches the row {name:?}");
        }
    }

    #[test]
    fn the_node_itself_is_only_free_busy_or_waiting_for_the_machine() {
        let min = 60_000;
        let cases: Vec<(&str, Vec<Event>, Option<(Millis, bool)>, NodeStatus, NodeState)> = vec![
            ("inside a turn", vec![hook("UserPromptSubmit")], None, NodeStatus::Busy, NodeState::Working),
            ("on a permission prompt", vec![hook("UserPromptSubmit"), hook("PermissionRequest")], None, NodeStatus::Busy, NodeState::WaitingAnswer),
            ("inside a call past its limit", background(&[hook("UserPromptSubmit"), tool("PreToolUse", "t")]), Some((46 * min, false)), NodeStatus::Busy, NodeState::Stalled),
            ("stopped by a usage limit", vec![hook("UserPromptSubmit"), usage_limit()], None, NodeStatus::WaitingMachine, NodeState::WaitingMachine),
            ("its question waits for the user", background(&[hook("UserPromptSubmit"), question(false, QuestionOrigin::Ask), hook("Stop")]), None, NodeStatus::Free, NodeState::WaitingAnswer),
            ("its task is done", vec![hook("UserPromptSubmit"), report(ReportKind::Final), hook("Stop")], None, NodeStatus::Free, NodeState::Done),
            ("its task should move", background(&[hook("UserPromptSubmit"), hook("Stop")]), None, NodeStatus::Free, NodeState::Stalled),
        ];
        for (what, events, sweep, status, state) in cases {
            let n = run(&events, sweep).nodes["s"].clone();
            assert_eq!((n.status, n.state), (status, state), "{what}");
        }
    }

    #[test]
    fn a_background_task_is_suspect_only_when_old_and_nothing_else_moves() {
        let min = 60_000;
        let started = [hook("UserPromptSubmit"), hook("Stop"), daemon_running(&[(BackgroundKind::Shell, T0), (BackgroundKind::Agent, T0 + 25 * min)])];
        let suspects = |v: &View, at: Millis| super::background(&v.nodes["s"], at).iter().map(|b| b.suspect).collect::<Vec<_>>();
        let v = run(&started, None);
        assert_eq!(suspects(&v, T0 + 40 * min), vec![true, false], "only the one past 30 min");
        assert_eq!(suspects(&v, T0 + 20 * min), vec![false, false], "nothing is old yet");
        let mut hooked = v.clone();
        hooked.apply(9, T0 + 35 * min, &hook("PreToolUse"));
        assert_eq!(suspects(&hooked, T0 + 40 * min), vec![false, false], "a hook 5 min ago is work");
        let swept = run(&started, Some((40 * min, false)));
        assert_eq!(swept.nodes["s"].state, NodeState::Working, "a young task still counts as work");
        let v = run(&[hook("UserPromptSubmit"), hook("Stop"), daemon_running(&[(BackgroundKind::Shell, T0)]), daemon("working", "idle", 0, None)], None);
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

    #[test]
    fn a_hung_background_task_is_raised_with_the_node_first_and_then_with_the_orchestrator() {
        let min = 60_000;
        let mut v = run(&[hook("UserPromptSubmit"), hook("Stop"), daemon_running(&[(BackgroundKind::Shell, T0)])], None);
        v.apply(50, T0, &Event::MarkedOrchestrator { session_id: "o".into() });
        v.apply(51, T0, &Event::Hook(HookEvent { session_id: "o".into(), event: "SessionStart".into(), ..Default::default() }));
        let next = |v: &View, at: Millis| hung_message(v, &v.nodes["s"], "Ana", at);

        assert_eq!(next(&v, T0 + 20 * min), None, "not hung yet");
        let to_node = next(&v, T0 + 40 * min).expect("hung after 30 idle min");
        assert_eq!(to_node.session_id, "s");
        assert!(to_node.text.starts_with(HUNG_PREFIX) && to_node.text.contains("shell \"wait for the build\""), "{}", to_node.text);
        v.apply(52, T0 + 40 * min, &Event::MessageQueued(to_node.clone()));
        assert_eq!(next(&v, T0 + 40 * min), None, "the node is told once");

        v.apply(53, T0 + 41 * min, &Event::MessageHanded { id: to_node.id });
        v.apply(54, T0 + 41 * min, &hook("UserPromptSubmit"));
        v.apply(55, T0 + 42 * min, &hook("Stop"));
        assert_eq!(next(&v, T0 + 60 * min), None, "the node worked on it recently");

        let to_orchestrator = next(&v, T0 + 73 * min).expect("still hung 30 min after the node's last move");
        assert_eq!(to_orchestrator.session_id, "o");
        assert!(to_orchestrator.text.contains("Ana") && to_orchestrator.text.contains("bg@"), "{}", to_orchestrator.text);
        v.apply(56, T0 + 73 * min, &Event::MessageQueued(to_orchestrator));
        assert_eq!(next(&v, T0 + 90 * min), None, "the orchestrator is told once");

        let fresh_start = run(&[hook("UserPromptSubmit"), hook("Stop"), daemon_running(&[(BackgroundKind::Shell, T0)]), daemon("working", "idle", 0, None)], None);
        assert_eq!(next(&fresh_start, T0 + 40 * min), None, "a stopped task raises nothing");
    }

    fn background(events: &[Event]) -> Vec<Event> {
        let mut all = vec![daemon("working", "active", 0, None)];
        all.extend_from_slice(events);
        all
    }

    #[test]
    fn only_an_undeclared_end_of_turn_of_a_background_node_is_stalled() {
        let [queued, handed] = message(MessageFrom::User);
        let typed = Event::Hook(HookEvent { session_id: "s".into(), event: "UserPromptSubmit".into(), prompt: Some("what is left?".into()), ..Default::default() });
        let cases: Vec<(&str, Vec<Event>, NodeState)> = vec![
            ("ended its turn saying what it will do next", background(&[hook("UserPromptSubmit"), hook("Stop")]), NodeState::Stalled),
            ("asked first", background(&[hook("UserPromptSubmit"), question(false, QuestionOrigin::Ask), hook("Stop")]), NodeState::WaitingAnswer),
            ("handed the user an action", background(&[hook("UserPromptSubmit"), question(true, QuestionOrigin::Ask), hook("Stop")]), NodeState::YourTurn),
            ("reported final", background(&[hook("UserPromptSubmit"), report(ReportKind::Final), hook("Stop")]), NodeState::Done),
            ("declared a machine wait", background(&[hook("UserPromptSubmit"), wait(), hook("Stop")]), NodeState::WaitingMachine),
            ("opened a PR, which has its card", background(&[hook("UserPromptSubmit"), pr_opened(), hook("Stop")]), NodeState::WaitingMerge),
            ("its PR was merged", background(&[hook("UserPromptSubmit"), pr_opened(), Event::PrMerged { id: 1, method: crate::pr::MergeMethod::Merge }, hook("Stop")]), NodeState::Stalled),
            ("changes were requested on its PR", background(&[hook("UserPromptSubmit"), pr_opened(), Event::PrChangesRequested { id: 1 }, hook("Stop")]), NodeState::Stalled),
            ("the user is typing in its terminal", background(&[typed, hook("Stop")]), NodeState::Unclear),
            ("Claude Code typed a subagent's notice", background(&[harness("<task-notification>\n<task-id>a1</task-id>"), hook("Stop")]), NodeState::Stalled),
            ("another session's message arrived", background(&[harness("<agent-message from=\"x\">done</agent-message>"), hook("Stop")]), NodeState::Stalled),
            ("a foreground session", vec![hook("UserPromptSubmit"), hook("Stop")], NodeState::Unclear),
            ("declared, then got a new message and stopped silently", background(&[hook("UserPromptSubmit"), wait(), hook("Stop"), queued, handed, hook("UserPromptSubmit"), hook("Stop")]), NodeState::Stalled),
        ];
        for (what, events, state) in cases {
            assert_eq!(run(&events, None).nodes["s"].state, state, "{what}");
        }
    }

    fn alondra_says(v: &mut View, session: &str, text: String, seq: u64) {
        let id = v.messages.keys().next_back().copied().unwrap_or(0) + 1;
        let m = Message { id, session_id: session.into(), from: MessageFrom::Alondra, text, answers_question: None, created_at: T0 + seq as Millis * 1000, handed_at: None, delivered_at: None, about: None, attachments: vec![] };
        v.apply(seq, T0 + seq as Millis * 1000, &Event::MessageQueued(m));
    }

    #[test]
    fn a_stalled_node_is_nudged_once_and_then_handed_to_the_orchestrator() {
        let with_orchestrator = |v: &mut View| {
            v.apply(50, T0, &Event::MarkedOrchestrator { session_id: "o".into() });
            v.apply(51, T0, &Event::Hook(HookEvent { session_id: "o".into(), event: "SessionStart".into(), ..Default::default() }));
        };
        let stalled = || run(&background(&[hook("UserPromptSubmit"), hook("Stop")]), None);

        let v = stalled();
        assert_eq!(stall_move(&v, &v.nodes["s"]), Some(StallMove::Nudge), "first stall");

        let mut v = stalled();
        alondra_says(&mut v, "s", nudge_text("Ana"), 60);
        assert_eq!(stall_move(&v, &v.nodes["s"]), None, "nudged, no orchestrator alive");

        let mut v = stalled();
        with_orchestrator(&mut v);
        alondra_says(&mut v, "s", nudge_text("Ana"), 60);
        assert_eq!(stall_move(&v, &v.nodes["s"]), None, "the nudge has not reached the node yet");
        let nudge = *v.messages.keys().next_back().unwrap();
        v.apply(61, T0 + 61_000, &Event::MessageHanded { id: nudge });
        assert_eq!(stall_move(&v, &v.nodes["s"]), None, "the nudge reached it and it has not stopped since");
        v.apply(62, T0 + 62_000, &typed(&nudge_text("Ana")));
        v.apply(63, T0 + 63_000, &hook("Stop"));
        assert_eq!(stall_move(&v, &v.nodes["s"]), Some(StallMove::TellOrchestrator { orchestrator: "o".into() }), "stalled again after the nudge");
        let text = stalled_text(&v.nodes["s"], "Ana");
        alondra_says(&mut v, "o", text, 64);
        assert_eq!(stall_move(&v, &v.nodes["s"]), None, "the orchestrator already has it");

        let mut v = stalled();
        with_orchestrator(&mut v);
        alondra_says(&mut v, "s", nudge_text("Ana"), 60);
        let [queued, handed] = message(MessageFrom::Orchestrator);
        for (i, e) in [queued, handed, hook("UserPromptSubmit"), hook("Stop")].iter().enumerate() {
            v.apply(70 + i as u64, T0 + (70 + i as Millis) * 1000, e);
        }
        assert_eq!(stall_move(&v, &v.nodes["s"]), Some(StallMove::Nudge), "the orchestrator sent it on and it stalled again");

        let v = run(&background(&[hook("UserPromptSubmit"), report(ReportKind::Final), hook("Stop")]), None);
        assert_eq!(stall_move(&v, &v.nodes["s"]), None, "not stalled");
    }

    fn typed(text: &str) -> Event {
        Event::Hook(HookEvent { session_id: "s".into(), event: "UserPromptSubmit".into(), prompt: Some(text.into()), ..Default::default() })
    }

    fn attach(on: bool) -> Event {
        Event::AttachChanged { session_id: "s".into(), attached: on }
    }

    #[test]
    fn the_user_watches_a_node_while_attached_or_right_after_typing_into_it() {
        let min = 60_000;
        let [queued, handed] = message(MessageFrom::Alondra);
        let cases: Vec<(&str, Vec<Event>, Millis, bool)> = vec![
            ("a terminal attached", background(&[attach(true)]), 60 * min, true),
            ("the terminal left", background(&[attach(true), attach(false)]), 0, false),
            ("typed a moment ago", background(&[typed("what is left?"), hook("Stop")]), 5 * min, true),
            ("typed 11 minutes ago", background(&[typed("what is left?"), hook("Stop")]), 11 * min, false),
            ("the prompt was Alondra's", background(&[queued, handed, typed("go on"), hook("Stop")]), min, false),
            ("nothing", background(&[hook("UserPromptSubmit"), hook("Stop")]), 0, false),
        ];
        for (what, events, later, expected) in cases {
            let v = run(&events, None);
            let at = T0 + (events.len() as Millis - 1) * 1000 + later;
            assert_eq!(watching(&v, &v.nodes["s"], at), expected, "{what}");
        }
    }

    #[test]
    fn a_watched_node_is_never_stalled_nor_handed_on_until_the_user_leaves() {
        let min = 60_000;
        let v = run(&background(&[typed("what is left?"), hook("Stop")]), Some((5 * min, false)));
        assert_ne!(v.nodes["s"].state, NodeState::Stalled, "typed 5 min ago");
        let v = run(&background(&[typed("what is left?"), hook("Stop")]), Some((11 * min, false)));
        assert_eq!(v.nodes["s"].state, NodeState::Stalled, "left 11 min ago: back to Alondra");

        let mut v = run(&background(&[hook("UserPromptSubmit"), call("Bash")]), Some((50 * min, false)));
        v.apply(50, T0, &Event::MarkedOrchestrator { session_id: "o".into() });
        v.apply(51, T0, &Event::Hook(HookEvent { session_id: "o".into(), event: "SessionStart".into(), ..Default::default() }));
        v.apply(52, T0 + 50 * min, &attach(true));
        assert_eq!(stall_move(&v, &v.nodes["s"]), None, "a stuck call the user is looking at");
        v.apply(53, T0 + 51 * min, &attach(false));
        assert!(stall_move(&v, &v.nodes["s"]).is_some(), "after the user leaves");
    }

    fn menu(waiting: Option<&str>) -> Event {
        Event::MenuSeen { session_id: "s".into(), waiting_for: waiting.map(str::to_string) }
    }

    #[test]
    fn a_menu_nobody_is_looking_at_must_reach_the_clients() {
        let cases: Vec<(&str, Vec<Event>, bool, NodeState)> = vec![
            ("the daemon says it waits on a permission", background(&[hook("UserPromptSubmit"), menu(Some("permission prompt"))]), true, NodeState::WaitingAnswer),
            ("a permission request Alondra did not hold", background(&[hook("UserPromptSubmit"), hook("PermissionRequest")]), true, NodeState::WaitingAnswer),
            ("the menu is already a card", background(&[hook("UserPromptSubmit"), menu(Some("permission prompt")), question(false, QuestionOrigin::Permission)]), false, NodeState::WaitingAnswer),
            ("the user is at the terminal", background(&[hook("UserPromptSubmit"), attach(true), menu(Some("dialog open"))]), false, NodeState::WaitingAnswer),
            ("the menu was answered and work goes on", background(&[hook("UserPromptSubmit"), menu(Some("permission prompt")), tool("PreToolUse", "t")]), false, NodeState::Working),
            ("the daemon says nothing is pending", background(&[hook("UserPromptSubmit"), menu(Some("permission prompt")), menu(None)]), false, NodeState::Working),
            ("a foreground session", vec![hook("UserPromptSubmit"), hook("PermissionRequest")], false, NodeState::WaitingAnswer),
        ];
        for (what, events, expected, state) in cases {
            let v = run(&events, None);
            let at = T0 + events.len() as Millis * 1000;
            assert_eq!(unseen_menu(&v, &v.nodes["s"], at), expected, "{what}");
            assert_eq!(v.nodes["s"].state, state, "{what}");
        }
    }

    /// alondra/sin-dormidos (45b58bd9), 24 Sep: idle for hours, `daemon_tasks: 1`, `background: []`,
    /// and `daemon_seen_at` refreshed on every poll. The count alone kept it working forever.
    #[test]
    fn a_task_count_with_nothing_listed_ages_from_its_last_change_not_the_last_poll() {
        let min = 60_000;
        let cases = [
            ("listed task, polled for 3 h", true, 180 * min, NodeState::Working),
            ("count only, 30 min after it appeared", false, 30 * min, NodeState::Working),
            ("count only, polled for 3 h", false, 180 * min, NodeState::Stalled),
        ];
        for (what, listed, after, expected) in cases {
            let mut v = run(&background(&[hook("UserPromptSubmit"), hook("Stop")]), None);
            let seen = |at: Millis| if listed { daemon_running(&[(BackgroundKind::Shell, at)]) } else { daemon("working", "idle", 1, None) };
            for (i, at) in (0..=after).step_by((10 * min) as usize).enumerate() {
                v.apply(100 + i as u64, T0 + 10_000 + at, &seen(T0 + 10_000 + at));
            }
            assert_eq!(v.nodes["s"].state, expected, "{what}");
        }
    }

    #[test]
    fn working_without_fresh_evidence_is_demoted() {
        let min = 60_000;
        assert_eq!(run(&[hook("UserPromptSubmit")], Some((6 * min, false))).nodes["s"].state, NodeState::Unclear);
        assert_eq!(run(&[hook("UserPromptSubmit"), hook("Stop"), daemon("working", "active", 0, None)], Some((31 * min, false))).nodes["s"].state, NodeState::Stalled);
        assert_eq!(run(&[hook("UserPromptSubmit"), hook("Stop"), daemon("working", "idle", 1, None)], Some((121 * min, false))).nodes["s"].state, NodeState::Stalled);
    }

    #[test]
    fn a_daemon_active_before_the_stop_does_not_count() {
        let v = run(&[hook("UserPromptSubmit"), daemon("working", "active", 0, None), hook("Stop")], None);
        assert_eq!(v.nodes["s"].state, NodeState::Stalled);
    }

    #[test]
    fn a_stale_tool_is_no_longer_shown() {
        let v = run(&[hook("UserPromptSubmit"), tool("PreToolUse", "t")], Some((46 * 60_000, false)));
        assert!(v.nodes["s"].tool.is_none());
    }

    fn call(name: &str) -> Event {
        Event::Hook(HookEvent { session_id: "s".into(), event: "PreToolUse".into(), tool_name: Some(name.into()), tool_use_id: Some("c".into()), ..Default::default() })
    }

    #[test]
    fn a_call_is_stuck_only_past_the_limit_for_its_kind() {
        let min = 60_000;
        let cases = [
            ("a build at 40 min", "Bash", 40 * min, NodeState::Working),
            ("a build at 46 min", "Bash", 46 * min, NodeState::Stalled),
            ("a subagent at 90 min", "Agent", 90 * min, NodeState::Working),
            ("a subagent at 121 min", "Task", 121 * min, NodeState::Stalled),
        ];
        for (what, name, after, state) in cases {
            let v = run(&background(&[hook("UserPromptSubmit"), call(name)]), Some((after, false)));
            assert_eq!(v.nodes["s"].state, state, "{what}");
            assert!(v.nodes["s"].tool.is_some(), "{what}: the call stays on show");
        }
    }

    #[test]
    fn a_stuck_call_goes_to_the_orchestrator_once() {
        let mut v = run(&background(&[hook("UserPromptSubmit"), call("Bash")]), Some((50 * 60_000, false)));
        assert_eq!(stall_move(&v, &v.nodes["s"]), None, "no orchestrator alive");
        v.apply(50, T0, &Event::MarkedOrchestrator { session_id: "o".into() });
        v.apply(51, T0, &Event::Hook(HookEvent { session_id: "o".into(), event: "SessionStart".into(), ..Default::default() }));
        assert_eq!(stall_move(&v, &v.nodes["s"]), Some(StallMove::TellOrchestrator { orchestrator: "o".into() }));
        let m = stall_message(&v, &v.nodes["s"], "Ana", T0 + 50 * 60_000).unwrap();
        assert!(m.text.contains("inside one call for 49 min: Bash"), "{}", m.text);
        v.apply(52, T0 + 50 * 60_000, &Event::MessageQueued(m));
        assert_eq!(stall_move(&v, &v.nodes["s"]), None);
    }

    #[test]
    fn a_declared_wait_lapses_when_nothing_happens_and_the_machine_is_fine() {
        let v = run(&[hook("UserPromptSubmit"), wait(), hook("Stop")], Some((21 * 60_000, false)));
        assert_eq!(v.nodes["s"].state, NodeState::Unclear);
    }

    #[test]
    fn a_sweep_records_the_clock_only_when_time_changed_something() {
        let min = 60_000;
        let mut v = run(&[hook("UserPromptSubmit"), wait(), hook("Stop")], None);
        let later = T0 + 25 * min;
        assert!(!v.due(T0 + 5 * min, false));
        assert!(!v.due(later, true), "a strained machine keeps the wait");
        assert!(v.due(later, false));
        v.apply(9, later, &Event::Clock { machine_strained: false });
        assert_eq!(v.nodes["s"].state, NodeState::Unclear);
        assert!(!v.due(later + min, false));
    }

    #[test]
    fn a_final_report_while_tools_run_is_working_until_the_turn_ends() {
        let mut v = run(&[hook("UserPromptSubmit"), report(ReportKind::Final), tool("PreToolUse", "t")], None);
        assert_eq!(v.nodes["s"].state, NodeState::Working);
        v.apply(9, T0 + 9000, &hook("Stop"));
        assert_eq!(v.nodes["s"].state, NodeState::Done);
    }

    #[test]
    fn a_finished_task_is_not_undone_by_what_the_daemon_guesses() {
        let v = run(&[daemon("working", "active", 0, None), hook("UserPromptSubmit"), report(ReportKind::Final), hook("Stop"), daemon("blocked", "blocked", 0, Some("decide"))], None);
        assert_eq!(v.nodes["s"].state, NodeState::Done, "the node's own final report finished its task");
    }

    #[test]
    fn a_message_from_the_user_ends_what_the_node_declared() {
        let [queued, handed] = message(MessageFrom::User);
        let v = run(&[hook("UserPromptSubmit"), wait(), hook("Stop"), queued, handed, hook("UserPromptSubmit"), hook("Stop")], None);
        assert_eq!(v.nodes["s"].state, NodeState::Unclear);
        let [queued, handed] = message(MessageFrom::Alondra);
        let v = run(&[hook("UserPromptSubmit"), wait(), hook("Stop"), queued, handed, hook("UserPromptSubmit"), hook("Stop")], None);
        assert_eq!(v.nodes["s"].state, NodeState::WaitingMachine);
    }

    #[test]
    fn an_answered_plan_closes_the_tool_that_asked_it() {
        let mut q = question(false, QuestionOrigin::Plan);
        if let Event::QuestionAsked(q) = &mut q {
            q.tool_use_id = Some("plan".into());
        }
        let mut v = run(&[hook("UserPromptSubmit"), tool("PreToolUse", "plan"), q], None);
        assert!(v.nodes["s"].tool.is_some());
        v.apply(9, T0 + 9000, &Event::QuestionClosed { id: 1, status: QuestionStatus::Withdrawn, note: None });
        assert!(v.nodes["s"].tool.is_none());
    }

    #[test]
    fn a_waking_node_without_the_plugin_starts_a_new_turn() {
        let mut v = run(&[daemon("done", "idle", 0, None), report(ReportKind::Notice)], None);
        assert_eq!(v.nodes["s"].state, NodeState::Delivered);
        v.apply(9, T0 + 9000, &daemon("working", "active", 0, None));
        v.apply(10, T0 + 10_000, &daemon("done", "idle", 0, None));
        assert_eq!(v.nodes["s"].turn, 1);
        assert_eq!(v.nodes["s"].state, NodeState::Unclear);
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
            about: Some(crate::model::MessageAbout::PrMerged { pr_id: 1 }),
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
            text: "David left comments".into(),
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
            ("a progress report sends nothing for review", background(&[hook("UserPromptSubmit"), sent_for_review(1, ReportKind::Progress), hook("Stop")]), NodeState::Stalled),
            ("the comments on their way", [sent(ReportKind::Document), vec![comments.clone(), comments_queued.clone()]].concat(), NodeState::WaitingMerge),
            ("the comments typed in", reviewed(&[]), NodeState::Working),
            ("republished for another round", reviewed(&[hook("UserPromptSubmit"), sent_for_review(2, ReportKind::Document), hook("Stop")]), NodeState::WaitingMerge),
            (
                "approved, then its final report links the page again",
                reviewed(&[hook("UserPromptSubmit"), sent_for_review(2, ReportKind::Document), hook("Stop"), approved.clone(), approved_queued.clone(), approved_handed.clone(), hook("UserPromptSubmit"), sent_for_review(3, ReportKind::Final), hook("Stop")]),
                NodeState::Done,
            ),
            ("David sets it aside", [sent(ReportKind::Final), vec![Event::ReportRead { id: 1 }]].concat(), NodeState::Done),
            ("inside an open PR card the PR decides", background(&[hook("UserPromptSubmit"), pr_opened(), Event::ReportPosted(Report { body: Some(format!("{PAGE} and https://github.com/a/b/pull/7")), ..match sent_for_review(1, ReportKind::Final) { Event::ReportPosted(r) => r, _ => unreachable!() } }), hook("Stop")]), NodeState::WaitingMerge),
        ];
        for (what, events, state) in cases {
            let v = run(&events, None);
            assert_eq!(v.nodes["s"].state, state, "{what}");
            assert!(limbo(&v, T0 + events.len() as Millis * 1000).is_empty(), "{what}: {:?}", limbo(&v, T0 + events.len() as Millis * 1000));
        }
        let v = run(&reviewed(&[hook("UserPromptSubmit"), sent_for_review(2, ReportKind::Document), hook("Stop"), approved, approved_queued, approved_handed, hook("UserPromptSubmit"), sent_for_review(3, ReportKind::Final), hook("Stop")]), None);
        assert!(v.reports[&3].read, "a report about an approved artifact asks nothing of David");
        let pr = run(&background(&[hook("UserPromptSubmit"), pr_opened(), Event::ReportPosted(Report { body: Some("https://github.com/a/b/pull/7".into()), ..match sent_for_review(1, ReportKind::Final) { Event::ReportPosted(r) => r, _ => unreachable!() } }), hook("Stop")]), None);
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
            ("merged with work left after it, stopped without a word", background(&[hook("UserPromptSubmit"), pr_opened(), hook("Stop"), notice.clone(), merged(), notice_handed.clone(), hook("UserPromptSubmit"), hook("Stop")]), NodeState::Stalled),
            ("merged on GitHub, nobody to tell", with_pr(&[merged()]), NodeState::Done),
            ("done", then(&[]), NodeState::Done),
            ("done, David writes to it", then(&[from_user, user_handed]), NodeState::Working),
            ("done, the orchestrator sends it on", then(&[from_orchestrator, orchestrator_handed]), NodeState::Working),
            ("done, David types in its terminal", then(&[typed("one more thing")]), NodeState::Working),
            ("done, an Alondra message typed in all the same", then(&[from_alondra, alondra_handed]), NodeState::Done),
            ("done, Claude Code reports a finished task", then(&[harness("<task-notification>\n<task-id>a1</task-id>"), tool("PreToolUse", "t")]), NodeState::Done),
            ("done, another session's message arrives", then(&[harness("<agent-message from=\"x\">done</agent-message>"), tool("PreToolUse", "t")]), NodeState::Done),
            ("done, a cross-session message arrives", then(&[harness("<cross-session-message from=\"x\">hi</cross-session-message>")]), NodeState::Done),
            ("done, a reminder is typed in", then(&[harness("<system-reminder>check</system-reminder>")]), NodeState::Done),
            ("done, David types after a harness prompt", then(&[harness("<task-notification>a1"), hook("Stop"), typed("one more thing")]), NodeState::Working),
        ];
        let wrong: Vec<String> = cases
            .into_iter()
            .filter_map(|(what, events, state)| {
                let got = run(&events, None).nodes["s"].state;
                (got != state).then(|| format!("{what}: {got:?}, expected {state:?}"))
            })
            .collect();
        assert!(wrong.is_empty(), "{wrong:#?}");
    }

    #[test]
    fn every_alondra_message_refuses_a_done_node() {
        let min = 60_000;
        let mut v = run(&background(&[daemon_running(&[(BackgroundKind::Shell, T0)]), hook("UserPromptSubmit"), report(ReportKind::Final), hook("Stop")]), Some((40 * min, false)));
        v.apply(50, T0, &Event::MarkedOrchestrator { session_id: "o".into() });
        v.apply(51, T0, &Event::Hook(HookEvent { session_id: "o".into(), event: "SessionStart".into(), ..Default::default() }));
        let at = T0 + 40 * min;
        let node = v.nodes["s"].clone();
        assert_eq!(node.state, NodeState::Done);
        assert!(super::background(&node, at).iter().all(|b| b.suspect), "a hung task would be raised on a live node");
        assert!(!v.may_alondra_write(&node));
        assert_eq!(v.alondra_message("s", nudge_text("Ana"), at), None, "a nudge");
        assert_eq!(hung_message(&v, &node, "Ana", at), None, "a hung task");
        assert_eq!(stall_message(&v, &node, "Ana", at), None, "a stall move");
        assert!(v.alondra_message("o", "[Alondra] Fleet updates".into(), at).is_some(), "the orchestrator still hears from Alondra");

        let mut v = run(&background(&[hook("UserPromptSubmit"), hook("Stop")]), None);
        assert_eq!(v.nodes["s"].state, NodeState::Stalled);
        let nudge = v.alondra_message("s", nudge_text("Ana"), T0 + 10_000).expect("a stalled node is nudged");
        v.apply(10, T0 + 10_000, &Event::MessageQueued(nudge));
        v.apply(11, T0 + 11_000, &report(ReportKind::Final));
        assert_eq!(v.nodes["s"].state, NodeState::Done);
        assert_eq!(v.deliverable_messages("s").count(), 0, "the nudge queued before it finished is not typed in");
        let [from_user, user_handed] = message(MessageFrom::User);
        v.apply(12, T0 + 12_000, &from_user);
        assert_eq!(v.deliverable_messages("s").map(|m| m.from).collect::<Vec<_>>(), [MessageFrom::User]);
        v.apply(13, T0 + 13_000, &user_handed);
        assert_eq!(v.nodes["s"].state, NodeState::Working, "David reopened it");
        assert_eq!(v.deliverable_messages("s").count(), 0, "the old nudge stays void once it is reopened");
    }

    #[test]
    fn a_state_this_build_does_not_know_reads_as_unclear() {
        let s: NodeState = serde_json::from_str("\"being_watched\"").unwrap();
        assert_eq!(s, NodeState::Unclear);
        let s: NodeState = serde_json::from_str("\"your_turn\"").unwrap();
        assert_eq!(s, NodeState::YourTurn);
        let o: OrchestratorState = serde_json::from_str("\"asleep\"").unwrap();
        assert_eq!(o, OrchestratorState::Unknown);
    }

    fn seen_without_process(state: &str) -> Event {
        Event::DaemonSeen(DaemonObservation { session_id: "s".into(), short_id: "s".into(), state: Some(state.into()), tempo: Some("idle".into()), ..Default::default() })
    }

    fn exited() -> Event {
        Event::ProcessExited { session_id: "s".into(), pid: 7 }
    }

    #[test]
    fn a_node_is_idle_when_it_should_work_only_with_a_running_task_no_work_and_nothing_declared() {
        let turn = |events: &[Event]| background(&[&[hook("UserPromptSubmit")], events].concat());
        let cases: Vec<(&str, Vec<Event>, bool)> = vec![
            ("ended its turn saying nothing", turn(&[hook("Stop")]), true),
            ("reported final", turn(&[report(ReportKind::Final), hook("Stop")]), false),
            ("sent a notice", turn(&[report(ReportKind::Notice), hook("Stop")]), false),
            ("asked", turn(&[question(false, QuestionOrigin::Ask), hook("Stop")]), false),
            ("declared a machine wait", turn(&[wait(), hook("Stop")]), false),
            ("still in its turn", turn(&[tool("PreToolUse", "t")]), false),
            ("opened a PR, now in review", turn(&[pr_opened(), hook("Stop")]), false),
            ("a background task still runs", turn(&[hook("Stop"), daemon_running(&[(BackgroundKind::Shell, T0)])]), false),
            ("the user is typing in it", background(&[typed("what is left?"), hook("Stop")]), false),
            ("a foreground session", vec![hook("UserPromptSubmit"), hook("Stop")], false),
            ("its process died while working: restarted instead", turn(&[hook("Stop"), exited(), seen_without_process("working")]), false),
            ("someone stopped it on purpose", turn(&[hook("Stop"), exited(), seen_without_process("stopped")]), false),
            ("the daemon let it go after its turn: the nudge brings it back", turn(&[hook("Stop"), exited(), seen_without_process("done")]), true),
        ];
        for (what, events, idle) in cases {
            let v = run(&events, None);
            let now = T0 + events.len() as Millis * 1000;
            assert_eq!(idle_task(&v, &v.nodes["s"], now).map(|t| t.id.as_str()), idle.then_some("t-s"), "{what}");
        }
    }

    #[test]
    fn an_idle_node_is_nudged_then_handed_on_by_its_running_task_whatever_its_state() {
        let named = Event::DaemonSeen(DaemonObservation { session_id: "s".into(), short_id: "s".into(), name: Some("n".into()), pid: Some(7), ..Default::default() });
        let launched = Event::Launched { name: "n".into(), request_id: None, auto_merge: true, task: None, title: None, session: None };
        let mut v = run(&[launched, named, hook("UserPromptSubmit"), pr_opened(), hook("Stop")], None);
        v.apply(50, T0, &Event::MarkedOrchestrator { session_id: "o".into() });
        v.apply(51, T0, &Event::Hook(HookEvent { session_id: "o".into(), event: "SessionStart".into(), ..Default::default() }));
        let checks = |v: &mut View, seq: u64, state: CheckState| {
            let mut pr = v.prs[&1].clone();
            pr.fetched = true;
            pr.checks = vec![crate::pr::Check { name: "ci".into(), state, url: None }];
            v.apply(seq, T0 + seq as Millis * 1000, &Event::PrUpdated(pr));
        };
        let at = |seq: u64| T0 + seq as Millis * 1000;
        assert_eq!(v.session_task("s").map(|t| t.state), Some(TaskState::Running), "an auto-merge PR leaves its task running");

        checks(&mut v, 52, CheckState::Running);
        assert_eq!(idle_task(&v, &v.nodes["s"], at(52)), None, "its PR waits on its checks, not on the node");
        assert_eq!(v.nodes["s"].state, NodeState::WaitingMerge, "Alondra waits on GitHub");
        checks(&mut v, 53, CheckState::Failed);
        assert_eq!(v.nodes["s"].state, NodeState::Stalled, "its task should move, and what the apps read says so");
        assert_eq!(stall_move(&v, &v.nodes["s"]), Some(StallMove::Nudge));
        assert_eq!(idle_move(&v, &v.nodes["s"], at(53)), Some(StallMove::Nudge), "failed checks are the node's to fix");
        let nudge = stall_message(&v, &v.nodes["s"], "Ana", at(53)).unwrap();
        assert_eq!((nudge.session_id.as_str(), nudge.text.clone()), ("s", nudge_text("Ana")));

        v.apply(54, at(54), &Event::MessageQueued(nudge.clone()));
        assert_eq!(stall_message(&v, &v.nodes["s"], "Ana", at(54)), None, "one nudge until it reaches the node");
        v.apply(55, at(55), &Event::MessageHanded { id: nudge.id });
        v.apply(56, at(56), &typed(&nudge_text("Ana")));
        v.apply(57, at(57), &hook("Stop"));
        let handed_on = stall_message(&v, &v.nodes["s"], "Ana", at(57)).unwrap();
        assert_eq!(handed_on.session_id, "o", "idle again after the nudge");
        assert!(handed_on.text.starts_with(STALLED_PREFIX) && handed_on.text.contains("while its task “n” is running"), "{}", handed_on.text);
        v.apply(58, at(58), &Event::MessageQueued(handed_on));
        assert_eq!(stall_message(&v, &v.nodes["s"], "Ana", at(58)), None, "the orchestrator already has it");
    }
}
