//! The node state machine: one table of (state, trigger, evidence) → state.
//!
//! A node's state is never set directly. Events update the facts kept on the node (hooks, the
//! Claude daemon's job state, declarations, questions and reports), and then the first row of
//! the table whose columns all match decides the state. Time only enters through the `now` the
//! caller passes: the reducer passes the time of the event it applies, and `Event::Clock` lets the
//! daemon record in the log that time has moved, so a replay reaches exactly the same states.

use crate::model::{
    BackgroundItem, BackgroundKind, Evidence, Message, MessageFrom, Millis, Node, NodeState, OrchestratorState, QuestionStatus, ReportKind, Signal, TurnSignal,
};
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

/// Rows are tried in order; the first one that matches wins. The last row always matches.
pub const NODE_TABLE: &[Row<NodeState>] = &[
    row("an open question waits for the user", Signal::OpenQuestion, NodeState::WaitingAnswer),
    row("a permission prompt is on screen", Signal::PermissionPrompt, NodeState::WaitingAnswer),
    row("an action waits for the user", Signal::ActionRequested, NodeState::YourTurn),
    row("a single call ran far longer than usual", Signal::CallStuck, NodeState::Stalled),
    row("a tool is in flight", Signal::ToolInFlight, NodeState::Working),
    row("a recent hook inside a turn", Signal::Hook, NodeState::Working),
    row("Alondra just typed a message into the session", Signal::MessageHanded, NodeState::Working),
    row("the Claude daemon reports the session active", Signal::DaemonActive, NodeState::Working),
    row("a background task of the node is still running", Signal::BackgroundTask, NodeState::Working),
    row("a usage limit stopped the node", Signal::UsageLimit, NodeState::WaitingMachine),
    row("the node declared it waits for the machine", Signal::DeclaredWait, NodeState::WaitingMachine),
    row("a PR of the node waits for the user on its card", Signal::PrAwaitingUser, NodeState::WaitingAnswer),
    row("the node reported final and nothing contradicts it", Signal::FinalReport, NodeState::Done),
    row("the user reviewed what the node delivered", Signal::Reviewed, NodeState::Done),
    row("a report the node delivered", Signal::Report, NodeState::Delivered),
    row("only possibly hung background tasks remain: handed to the sentinel", Signal::BackgroundSuspect, NodeState::Unclear),
    row("a turn ended without a declaration", Signal::TurnEndedUndeclared, NodeState::Stalled),
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

/// The daemon's job state contradicts a report when, written after it, it says the node is
/// blocked on something.
fn daemon_contradicts(node: &Node, since: Millis) -> bool {
    node.daemon_state.as_deref() == Some("blocked")
        && node.daemon_needs.as_deref().is_some_and(|n| !n.trim().is_empty())
        && node.daemon_updated_at.or(node.daemon_seen_at).is_some_and(|at| at > since)
}

pub fn facts(view: &View, node: &Node, now: Millis) -> Facts {
    facts_with(view, node, now, view.machine_strained)
}

fn facts_with(view: &View, node: &Node, now: Millis, machine_strained: bool) -> Facts {
    let mut f = Facts::default();
    let open = view.questions.values().filter(|q| {
        q.session_id == node.session_id && q.status == QuestionStatus::Open && q.blocking && !q.to_orchestrator
    });
    let (mut question, mut action) = (None, None);
    for q in open {
        let slot = if q.action { &mut action } else { &mut question };
        *slot = Some(slot.map_or(q.asked_at, |at: Millis| at.max(q.asked_at)));
    }
    f.add(Signal::OpenQuestion, question);
    f.add(Signal::PermissionPrompt, (node.waiting_permission || node.menu_waiting.is_some()).then_some(node.last_seen));
    f.add(Signal::ActionRequested, action);

    f.add(Signal::CallStuck, node.tool.as_ref().filter(|t| node.watched() && now - t.since > call_limit(&t.name)).map(|t| t.since));
    f.add(Signal::ToolInFlight, node.tool.as_ref().filter(|t| now - t.since <= call_limit(&t.name)).map(|t| t.since));
    f.add(Signal::Hook, fresh(node.last_work_at.filter(|_| node.busy), now, HOOK_FRESH_MS));
    f.add(Signal::MessageHanded, fresh(node.handed_at.filter(after_stop(node)), now, HOOK_FRESH_MS));
    f.add(Signal::DaemonActive, daemon_active(node, now));
    let tasks_seen = fresh(node.daemon_seen_at.filter(|_| node.alive && node.daemon_tasks > 0), now, BACKGROUND_TASK_FRESH_MS);
    let all_suspect = !node.background.is_empty() && background(node, now).iter().all(|b| b.suspect);
    f.add(Signal::BackgroundTask, tasks_seen.filter(|_| !all_suspect));
    f.add(Signal::BackgroundSuspect, tasks_seen.filter(|_| all_suspect));

    f.add(Signal::UsageLimit, fresh(node.usage_limited_at.filter(|_| node.usage_limited), now, USAGE_LIMIT_WAIT_MS));
    let declared = node.signal_at.unwrap_or(node.episode_since);
    if node.turn_signal == Some(TurnSignal::WaitingMachine) {
        let quiet_since = declared.max(node.last_seen);
        let holds = machine_strained || now - quiet_since <= MACHINE_WAIT_QUIET_MS;
        f.add(Signal::DeclaredWait, holds.then_some(declared));
    }
    f.add(Signal::PrAwaitingUser, view.prs_awaiting_user().filter(|p| p.session_id == node.session_id).map(|p| p.opened_at).max());
    let agreed = !daemon_contradicts(node, declared);
    match node.turn_signal {
        Some(TurnSignal::Finished) if agreed && node.has_plugin => f.add(Signal::FinalReport, Some(declared)),
        Some(TurnSignal::Finished | TurnSignal::Delivered) if agreed => {
            f.add(Signal::Reviewed, node.acknowledged_at.filter(|&read| read >= declared));
            f.add(Signal::Report, Some(declared));
        }
        _ => {}
    }

    let undeclared = node.watched() && node.has_plugin && node.turn_signal.is_none() && node.last_work_at.is_some() && !watching(view, node, now);
    f.add(Signal::TurnEndedUndeclared, node.last_stop_at.filter(|stop| undeclared && *stop >= node.episode_since));

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
    if node.is_orchestrator {
        let current = node.orchestrator_state.unwrap_or(OrchestratorState::OnDuty);
        let (row, at) = first(ORCHESTRATOR_TABLE, &facts, current, trigger);
        return Outcome {
            state: row.to.node_state(),
            orchestrator: Some(row.to),
            evidence: Evidence { row: row.name.to_string(), signal: row.when, at },
            drop_tool,
        };
    }
    let (row, at) = first(NODE_TABLE, &facts, node.state, trigger);
    Outcome { state: row.to, orchestrator: None, evidence: Evidence { row: row.name.to_string(), signal: row.when, at }, drop_tool }
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
        let missing = match node.state {
            NodeState::Working => !works(view, node, now),
            NodeState::WaitingAnswer => !open(false) && !node.waiting_permission && node.menu_waiting.is_none(),
            NodeState::YourTurn => !open(true),
            NodeState::WaitingMachine => !matches!(outcome.evidence.signal, Signal::UsageLimit | Signal::DeclaredWait),
            NodeState::Done => !reported(&[ReportKind::Final]),
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

/// The message a stall move puts in a session.
pub fn stall_message(view: &View, node: &Node, user: &str, at: Millis) -> Option<Message> {
    let stuck = node.evidence.as_ref().is_some_and(|e| e.signal == Signal::CallStuck);
    let (session_id, text) = match stall_move(view, node)? {
        StallMove::Nudge => (node.session_id.clone(), nudge_text(user)),
        StallMove::TellOrchestrator { orchestrator } if stuck => (orchestrator, stuck_text(node, user, at)?),
        StallMove::TellOrchestrator { orchestrator } => (orchestrator, stalled_text(node, user)),
    };
    Some(Message {
        id: view.messages.keys().next_back().copied().unwrap_or(0) + 1,
        session_id,
        from: MessageFrom::Alondra,
        text,
        answers_question: None,
        created_at: at,
        handed_at: None,
        delivered_at: None,
    })
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
    Some(Message {
        id: view.messages.keys().next_back().copied().unwrap_or(0) + 1,
        session_id,
        from: MessageFrom::Alondra,
        text,
        answers_question: None,
        created_at: at,
        handed_at: None,
        delivered_at: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::{DaemonObservation, Event, HookEvent};
    use crate::model::{BackgroundKind, Message, MessageFrom, Question, QuestionOrigin, Report};

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
            related: vec![],
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
        })
    }

    fn pr_opened() -> Event {
        Event::PrOpened { url: "https://github.com/a/b/pull/7".into(), session_id: "s".into() }
    }

    fn report(kind: ReportKind) -> Event {
        Event::ReportPosted(Report {
            related: vec![],
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
            mission: None,
            looks_like_question: false,
        })
    }

    fn usage_limit() -> Event {
        Event::Hook(HookEvent { session_id: "s".into(), event: "StopFailure".into(), error_type: Some("rate_limit".into()), ..Default::default() })
    }

    fn wait() -> Event {
        Event::WaitingMachine { session_id: "s".into(), reason: "RAM".into() }
    }

    fn message(from: MessageFrom) -> [Event; 2] {
        let m = Message { id: 9, session_id: "s".into(), from, text: "go on".into(), answers_question: None, created_at: T0, handed_at: None, delivered_at: None };
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

    fn row_named(to: NodeState, signal: Signal) -> &'static str {
        NODE_TABLE.iter().find(|r| r.to == to && r.when == signal && r.on.is_empty()).map(|r| r.name).unwrap()
    }

    fn cases() -> Vec<Case> {
        let min = 60_000;
        let stale = NODE_TABLE.iter().find(|r| !r.on.is_empty()).unwrap().name;
        let [queued, handed] = message(MessageFrom::Alondra);
        vec![
            Case { what: "an open ask", events: vec![hook("UserPromptSubmit"), question(false, QuestionOrigin::Ask), hook("Stop")], sweep: None, state: NodeState::WaitingAnswer, row: row_named(NodeState::WaitingAnswer, Signal::OpenQuestion) },
            Case { what: "a permission prompt", events: vec![hook("UserPromptSubmit"), hook("PermissionRequest")], sweep: None, state: NodeState::WaitingAnswer, row: row_named(NodeState::WaitingAnswer, Signal::PermissionPrompt) },
            Case { what: "an action for the user", events: vec![daemon("done", "idle", 0, None), question(true, QuestionOrigin::Classifier)], sweep: None, state: NodeState::YourTurn, row: row_named(NodeState::YourTurn, Signal::ActionRequested) },
            Case { what: "a long tool, no hook for 10 min", events: vec![hook("UserPromptSubmit"), tool("PreToolUse", "t")], sweep: Some((10 * min, false)), state: NodeState::Working, row: row_named(NodeState::Working, Signal::ToolInFlight) },
            Case { what: "a hook a minute ago", events: vec![hook("UserPromptSubmit")], sweep: Some((min, false)), state: NodeState::Working, row: row_named(NodeState::Working, Signal::Hook) },
            Case { what: "a nudge typed into an idle session", events: vec![hook("UserPromptSubmit"), hook("Stop"), queued, handed], sweep: None, state: NodeState::Working, row: row_named(NodeState::Working, Signal::MessageHanded) },
            Case { what: "the daemon says active after the Stop", events: vec![hook("UserPromptSubmit"), hook("Stop"), daemon("working", "active", 0, None)], sweep: None, state: NodeState::Working, row: row_named(NodeState::Working, Signal::DaemonActive) },
            Case { what: "a build left running in the background", events: vec![hook("UserPromptSubmit"), hook("Stop"), daemon("working", "idle", 1, None)], sweep: Some((90 * min, false)), state: NodeState::Working, row: row_named(NodeState::Working, Signal::BackgroundTask) },
            Case { what: "a wait loop left alone for 40 min", events: vec![hook("UserPromptSubmit"), hook("Stop"), daemon_running(&[(BackgroundKind::Shell, T0)])], sweep: Some((40 * min, false)), state: NodeState::Unclear, row: row_named(NodeState::Unclear, Signal::BackgroundSuspect) },
            Case { what: "a usage limit", events: vec![hook("UserPromptSubmit"), usage_limit()], sweep: None, state: NodeState::WaitingMachine, row: row_named(NodeState::WaitingMachine, Signal::UsageLimit) },
            Case { what: "a declared wait", events: vec![hook("UserPromptSubmit"), wait(), hook("Stop")], sweep: Some((10 * min, false)), state: NodeState::WaitingMachine, row: row_named(NodeState::WaitingMachine, Signal::DeclaredWait) },
            Case { what: "a declared wait on a strained machine", events: vec![hook("UserPromptSubmit"), wait(), hook("Stop")], sweep: Some((3 * 60 * min, true)), state: NodeState::WaitingMachine, row: row_named(NodeState::WaitingMachine, Signal::DeclaredWait) },
            Case { what: "an open PR card after the turn", events: vec![hook("UserPromptSubmit"), pr_opened(), report(ReportKind::Final), hook("Stop")], sweep: None, state: NodeState::WaitingAnswer, row: row_named(NodeState::WaitingAnswer, Signal::PrAwaitingUser) },
            Case { what: "a final report", events: vec![hook("UserPromptSubmit"), report(ReportKind::Final), hook("Stop")], sweep: None, state: NodeState::Done, row: row_named(NodeState::Done, Signal::FinalReport) },
            Case { what: "a notice", events: vec![hook("UserPromptSubmit"), report(ReportKind::Notice), hook("Stop")], sweep: None, state: NodeState::Delivered, row: row_named(NodeState::Delivered, Signal::Report) },
            Case { what: "a notice the user read", events: vec![hook("UserPromptSubmit"), report(ReportKind::Notice), hook("Stop"), Event::ReportRead { id: 1 }], sweep: None, state: NodeState::Done, row: row_named(NodeState::Done, Signal::Reviewed) },
            Case { what: "a background call past its limit", events: background(&[hook("UserPromptSubmit"), tool("PreToolUse", "t")]), sweep: Some((46 * min, false)), state: NodeState::Stalled, row: row_named(NodeState::Stalled, Signal::CallStuck) },
            Case { what: "a foreground tool nobody closed", events: vec![hook("UserPromptSubmit"), tool("PreToolUse", "t")], sweep: Some((46 * min, false)), state: NodeState::Unclear, row: stale },
            Case { what: "a background node that ended its turn without declaring", events: vec![daemon("working", "active", 0, None), hook("UserPromptSubmit"), hook("Stop")], sweep: Some((30 * min, false)), state: NodeState::Stalled, row: row_named(NodeState::Stalled, Signal::TurnEndedUndeclared) },
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
        for r in NODE_TABLE {
            assert!(cases.iter().any(|c| c.row == r.name), "no case reaches the row {:?}", r.name);
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
            ("opened a PR, which has its card", background(&[hook("UserPromptSubmit"), pr_opened(), hook("Stop")]), NodeState::WaitingAnswer),
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
        let m = Message { id, session_id: session.into(), from: MessageFrom::Alondra, text, answers_question: None, created_at: T0 + seq as Millis * 1000, handed_at: None, delivered_at: None };
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
    fn a_blocked_daemon_after_the_final_report_contradicts_it() {
        let v = run(&[daemon("working", "active", 0, None), hook("UserPromptSubmit"), report(ReportKind::Final), hook("Stop"), daemon("blocked", "blocked", 0, Some("decide"))], None);
        assert_eq!(v.nodes["s"].state, NodeState::Unclear);
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

    #[test]
    fn a_state_this_build_does_not_know_reads_as_unclear() {
        let s: NodeState = serde_json::from_str("\"being_watched\"").unwrap();
        assert_eq!(s, NodeState::Unclear);
        let s: NodeState = serde_json::from_str("\"your_turn\"").unwrap();
        assert_eq!(s, NodeState::YourTurn);
        let o: OrchestratorState = serde_json::from_str("\"asleep\"").unwrap();
        assert_eq!(o, OrchestratorState::Unknown);
    }
}
