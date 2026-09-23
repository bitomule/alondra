//! The node state machine: one table of (state, trigger, evidence) → state.
//!
//! A node's state is never set directly. Events update the facts kept on the node (hooks, the
//! Claude daemon's job state, declarations, questions and reports), and then the first row of
//! the table whose columns all match decides the state. Time only enters through the `now` the
//! caller passes: the reducer passes the time of the event it applies, and `Event::Clock` lets the
//! daemon record in the log that time has moved, so a replay reaches exactly the same states.

use crate::model::{
    Evidence, Millis, Node, NodeState, OrchestratorState, QuestionStatus, ReportKind, Signal, TurnSignal,
};
use crate::view::View;

const MINUTE: Millis = 60 * 1000;

/// A hook that shows the node inside a turn counts as work for this long.
pub const HOOK_FRESH_MS: Millis = 5 * MINUTE;
/// A tool opened by PreToolUse and never closed stops counting after this long.
pub const TOOL_FRESH_MS: Millis = 15 * MINUTE;
/// The Claude daemon saying "active" counts for this long after it was last observed.
pub const DAEMON_FRESH_MS: Millis = 30 * MINUTE;
/// A background task (a Bash run in the background, a subagent, a monitor) reported by the
/// Claude daemon counts for this long after it was last observed.
pub const BACKGROUND_TASK_FRESH_MS: Millis = 120 * MINUTE;
/// A declared wait for the machine lapses when nothing happens for this long and the machine
/// is fine.
pub const MACHINE_WAIT_QUIET_MS: Millis = 20 * MINUTE;
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
    row("a tool is in flight", Signal::ToolInFlight, NodeState::Working),
    row("a recent hook inside a turn", Signal::Hook, NodeState::Working),
    row("Alondra just typed a message into the session", Signal::MessageHanded, NodeState::Working),
    row("the Claude daemon reports the session active", Signal::DaemonActive, NodeState::Working),
    row("a background task of the node is still running", Signal::BackgroundTask, NodeState::Working),
    row("a usage limit stopped the node", Signal::UsageLimit, NodeState::WaitingMachine),
    row("the node declared it waits for the machine", Signal::DeclaredWait, NodeState::WaitingMachine),
    row("the node reported final and nothing contradicts it", Signal::FinalReport, NodeState::Done),
    row("the user reviewed what the node delivered", Signal::Reviewed, NodeState::Done),
    row("a report the node delivered", Signal::Report, NodeState::Delivered),
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
    f.add(Signal::PermissionPrompt, node.waiting_permission.then_some(node.last_seen));
    f.add(Signal::ActionRequested, action);

    f.add(Signal::ToolInFlight, fresh(node.tool.as_ref().map(|t| t.since), now, TOOL_FRESH_MS));
    f.add(Signal::Hook, fresh(node.last_work_at.filter(|_| node.busy), now, HOOK_FRESH_MS));
    let after_stop = |at: &Millis| node.last_stop_at.is_none_or(|stop| *at > stop);
    f.add(Signal::MessageHanded, fresh(node.handed_at.filter(after_stop), now, HOOK_FRESH_MS));
    let daemon_active = node.alive && node.daemon_tempo.as_deref() == Some("active");
    f.add(Signal::DaemonActive, fresh(node.daemon_seen_at.filter(|_| daemon_active).filter(after_stop), now, DAEMON_FRESH_MS));
    f.add(
        Signal::BackgroundTask,
        fresh(node.daemon_seen_at.filter(|_| node.alive && node.daemon_tasks > 0), now, BACKGROUND_TASK_FRESH_MS),
    );

    f.add(Signal::UsageLimit, fresh(node.usage_limited_at.filter(|_| node.usage_limited), now, USAGE_LIMIT_WAIT_MS));
    let declared = node.signal_at.unwrap_or(node.episode_since);
    if node.turn_signal == Some(TurnSignal::WaitingMachine) {
        let quiet_since = declared.max(node.last_seen);
        let holds = machine_strained || now - quiet_since <= MACHINE_WAIT_QUIET_MS;
        f.add(Signal::DeclaredWait, holds.then_some(declared));
    }
    let agreed = !daemon_contradicts(node, declared);
    match node.turn_signal {
        Some(TurnSignal::Finished) if agreed && node.has_plugin => f.add(Signal::FinalReport, Some(declared)),
        Some(TurnSignal::Finished | TurnSignal::Delivered) if agreed => {
            f.add(Signal::Reviewed, node.acknowledged_at.filter(|&read| read >= declared));
            f.add(Signal::Report, Some(declared));
        }
        _ => {}
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
            NodeState::WaitingAnswer => !open(false) && !node.waiting_permission,
            NodeState::YourTurn => !open(true),
            NodeState::WaitingMachine => !matches!(outcome.evidence.signal, Signal::UsageLimit | Signal::DeclaredWait),
            NodeState::Done => !reported(&[ReportKind::Final]),
            NodeState::Delivered => !reported(&[ReportKind::Final, ReportKind::Notice, ReportKind::Document]),
            NodeState::Unclear => false,
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::{DaemonObservation, Event, HookEvent};
    use crate::model::{Message, MessageFrom, Question, QuestionOrigin, Report};

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
        })
    }

    fn report(kind: ReportKind) -> Event {
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
            Case { what: "a usage limit", events: vec![hook("UserPromptSubmit"), usage_limit()], sweep: None, state: NodeState::WaitingMachine, row: row_named(NodeState::WaitingMachine, Signal::UsageLimit) },
            Case { what: "a declared wait", events: vec![hook("UserPromptSubmit"), wait(), hook("Stop")], sweep: Some((10 * min, false)), state: NodeState::WaitingMachine, row: row_named(NodeState::WaitingMachine, Signal::DeclaredWait) },
            Case { what: "a declared wait on a strained machine", events: vec![hook("UserPromptSubmit"), wait(), hook("Stop")], sweep: Some((3 * 60 * min, true)), state: NodeState::WaitingMachine, row: row_named(NodeState::WaitingMachine, Signal::DeclaredWait) },
            Case { what: "a final report", events: vec![hook("UserPromptSubmit"), report(ReportKind::Final), hook("Stop")], sweep: None, state: NodeState::Done, row: row_named(NodeState::Done, Signal::FinalReport) },
            Case { what: "a notice", events: vec![hook("UserPromptSubmit"), report(ReportKind::Notice), hook("Stop")], sweep: None, state: NodeState::Delivered, row: row_named(NodeState::Delivered, Signal::Report) },
            Case { what: "a notice the user read", events: vec![hook("UserPromptSubmit"), report(ReportKind::Notice), hook("Stop"), Event::ReportRead { id: 1 }], sweep: None, state: NodeState::Done, row: row_named(NodeState::Done, Signal::Reviewed) },
            Case { what: "a tool nobody closed", events: vec![hook("UserPromptSubmit"), tool("PreToolUse", "t")], sweep: Some((16 * min, false)), state: NodeState::Unclear, row: stale },
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
    fn working_without_fresh_evidence_is_demoted() {
        let min = 60_000;
        assert_eq!(run(&[hook("UserPromptSubmit")], Some((6 * min, false))).nodes["s"].state, NodeState::Unclear);
        assert_eq!(run(&[hook("UserPromptSubmit"), hook("Stop"), daemon("working", "active", 0, None)], Some((31 * min, false))).nodes["s"].state, NodeState::Unclear);
        assert_eq!(run(&[hook("UserPromptSubmit"), hook("Stop"), daemon("working", "idle", 1, None)], Some((121 * min, false))).nodes["s"].state, NodeState::Unclear);
    }

    #[test]
    fn a_daemon_active_before_the_stop_does_not_count() {
        let v = run(&[hook("UserPromptSubmit"), daemon("working", "active", 0, None), hook("Stop")], None);
        assert_eq!(v.nodes["s"].state, NodeState::Unclear);
    }

    #[test]
    fn a_stale_tool_is_no_longer_shown() {
        let v = run(&[hook("UserPromptSubmit"), tool("PreToolUse", "t")], Some((16 * 60_000, false)));
        assert!(v.nodes["s"].tool.is_none());
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
        v.apply(9, T0 + 9000, &Event::QuestionClosed { id: 1, status: QuestionStatus::Withdrawn });
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
