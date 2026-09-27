//! Durable, fenced lifecycle of the one logical orchestrator. Process ownership lives in the
//! daemon; this reducer never infers successful work from a process disappearing.
use serde::{Deserialize, Serialize};
use crate::{Event, View, model::{Millis, Node, OrchestratorState}};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Migration { pub session_id: String, pub pid: Option<i32> }

pub const NODE_ID: &str = "alondra-orchestrator";

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Phase { Idle, Starting, Running, Compacting, Failed }
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunKind { Work, Compact }
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Outcome { Success, Retryable, Ambiguous }
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Context {
    pub input_tokens: u64,
    pub window: Option<u64>,
    pub model: Option<String>,
    pub measured_at: Millis,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Run {
    pub id: String,
    pub kind: RunKind,
    pub message_ids: Vec<u64>,
    pub started_at: Millis,
    #[serde(default)]
    pub initialized: bool,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Resolution {
    pub run_id: String,
    pub message_ids: Vec<u64>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Runtime {
    pub session_id: Option<String>,
    pub generation: u64,
    pub phase: Phase,
    pub active_run: Option<Run>,
    pub context: Option<Context>,
    pub last_error: Option<String>,
    pub retry_at: Option<Millis>,
    pub compact_pending: bool,
    pub updated_at: Millis,
    #[serde(default)]
    pub last_run_id: Option<String>,
    #[serde(default)]
    pub consecutive_failures: u32,
    #[serde(default)]
    pub cancel_requested: bool,
    #[serde(default)]
    pub last_report: u64,
    #[serde(default)]
    pub last_question: u64,
    #[serde(default)]
    pub machine_alarm: Option<String>,
    #[serde(default)]
    pub pending_resolutions: Vec<Resolution>,
}
impl Runtime {
    pub fn state(&self) -> OrchestratorState {
        match self.phase {
            Phase::Idle => OrchestratorState::OnDuty,
            Phase::Starting | Phase::Running | Phase::Compacting => OrchestratorState::Handling,
            Phase::Failed => OrchestratorState::Unknown,
        }
    }
    pub fn is_active(&self) -> bool { matches!(self.phase, Phase::Starting | Phase::Running | Phase::Compacting) }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "transition", rename_all = "snake_case")]
pub enum Transition {
    /// Caller must prove the legacy runtime stopped before enabling this consumer.
    Enabled { session_id: Option<String> },
    MigrationStarted { session_id: String, pid: Option<i32> },
    LegacyStopped { session_id: String },
    MigrationBlocked { error: String },
    OwnershipWaiting { error: String },
    /// Append atomically with the notification message, even while another batch runs.
    NotificationsQueued { last_report: u64, last_question: u64, machine_alarm: Option<String> },
    Started { run_id: String, kind: RunKind, message_ids: Vec<u64> },
    SessionBound { run_id: String, session_id: String },
    Running { run_id: String },
    CancelRequested { run_id: String },
    /// Retryable is reserved for failures proven to precede any business action, or compact.
    Finished { run_id: String, outcome: Outcome, context: Option<Context>, error: Option<String>, retry_at: Option<Millis>, compact_pending: bool },
    /// The user reconciled possible effects and explicitly authorized replay of the batch.
    RetryApproved { run_id: String },
    /// The user reconciled the execution and chooses to continue without replaying its batch.
    Acknowledged { run_id: String },
    /// Only an explicit missing-session response before init permits resetting identity.
    SessionMissing { run_id: String },
}

pub fn pending_message_ids(view: &View) -> Vec<u64> {
    let claimed = view.orchestrator.as_ref().and_then(|r| r.active_run.as_ref());
    view.pending_messages(NODE_ID).filter(|m| claimed.is_none_or(|r| !r.message_ids.contains(&m.id)) && view.message_attempt(m.id).is_none_or(|a| !a.blocks())).map(|m| m.id).collect()
}
pub fn can_start(view: &View, now: Millis) -> bool {
    view.orchestrator.as_ref().is_some_and(|r| r.active_run.is_none() && r.retry_at.is_none_or(|at| at <= now) && (r.phase == Phase::Idle || r.phase == Phase::Failed))
}
pub fn integrity_issues(view: &View) -> Vec<String> {
    let Some(runtime) = &view.orchestrator else { return vec![]; };
    let mut issues = vec![];
    if !view.nodes.get(NODE_ID).is_some_and(|n| n.is_orchestrator) { issues.push("logical orchestrator node missing".into()); }
    if runtime.is_active() != runtime.active_run.is_some() && runtime.phase != Phase::Failed { issues.push("orchestrator execution/phase mismatch".into()); }
    if let Some(run) = &runtime.active_run {
        if run.kind == RunKind::Compact && !run.message_ids.is_empty() { issues.push("compaction owns business messages".into()); }
        if run.message_ids.iter().any(|id| view.messages.get(id).is_none_or(|m| m.session_id != NODE_ID || m.handed_at.is_some())) { issues.push("orchestrator claim lost its payload".into()); }
    }
    issues
}

pub fn logical_node(view: &View) -> Option<&Node> { view.orchestrator_node() }

/// Call inside the hub's append_with transaction; checking an earlier snapshot is not a claim.
pub fn checked_transition(view: &View, transition: Transition, at: Millis) -> Result<Event, String> {
    validate(view, &transition, at)?;
    Ok(Event::Orchestrator(transition))
}

pub fn migration_blocker(view: &View, node: &Node) -> Option<&'static str> {
    if node.busy || node.tool.is_some() || node.waiting_permission || node.menu_waiting.is_some() || node.attached || node.daemon_tasks > 0 { return Some("legacy orchestrator is busy, attached or waiting for an interactive response"); }
    if view.questions.values().any(|q| q.session_id == node.session_id && q.status.is_open() && q.origin != crate::model::QuestionOrigin::Ask) { return Some("legacy orchestrator has a pending interactive permission or plan"); }
    if view.board.tasks.values().any(|t| t.assignee.as_ref().is_some_and(|a| a.session_id == node.session_id) && !t.state.finished()) { return Some("legacy orchestrator has unfinished assigned tasks"); }
    if view.delivery_attempts.values().any(|a| a.session_id == node.session_id && a.blocks()) { return Some("legacy orchestrator delivery needs reconciliation"); }
    None
}

pub fn validate(view: &View, transition: &Transition, at: Millis) -> Result<(), String> {
    if let Transition::MigrationBlocked { error } = transition {
        return if view.orchestrator.is_none() && !error.is_empty() { Ok(()) } else { Err("migration is not blocked".into()) };
    }
    if let Transition::MigrationStarted { session_id, pid } = transition {
        if view.orchestrator.is_some() || view.orchestrator_migration.is_some() { return Err("migration already started or completed".into()); }
        let node = view.nodes.get(session_id).filter(|n| n.is_orchestrator).ok_or("legacy orchestrator missing")?;
        if node.pid != *pid { return Err("legacy process changed".into()); }
        if view.nodes.values().filter(|n| n.is_orchestrator).count() != 1 { return Err("multiple legacy orchestrators require reconciliation".into()); }
        if let Some(error) = migration_blocker(view, node) { return Err(error.into()); }
        return Ok(());
    }
    if let Transition::LegacyStopped { session_id } = transition {
        return if view.orchestrator.is_none() && view.orchestrator_migration.as_ref().is_some_and(|m| m.session_id == *session_id) { Ok(()) } else { Err("migration ownership changed".into()) };
    }
    if let Transition::Enabled { session_id } = transition {
        if view.orchestrator.is_some() { return Err("orchestrator already enabled".into()); }
        if view.orchestrator_migration.as_ref().is_some_and(|m| Some(&m.session_id) != session_id.as_ref()) { return Err("migration session identity changed".into()); }
        if session_id.as_ref().is_some_and(|s| s.is_empty() || s == NODE_ID) { return Err("invalid Claude session ID".into()); }
        if view.nodes.values().any(|n| n.is_orchestrator && (n.alive || n.in_daemon)) { return Err("legacy orchestrator must stop before migration".into()); }
        if view.delivery_attempts.values().any(|a| a.blocks() && (Some(&a.session_id) == session_id.as_ref() || view.nodes.get(&a.session_id).is_some_and(|n| n.is_orchestrator))) { return Err("legacy orchestrator delivery needs reconciliation".into()); }
        return Ok(());
    }
    let runtime = view.orchestrator.as_ref().ok_or("orchestrator not enabled")?;
    let active = |id: &str| runtime.active_run.as_ref().filter(|r| r.id == id).ok_or_else(|| "stale orchestrator execution".to_string());
    match transition {
        Transition::Enabled { .. } | Transition::MigrationStarted { .. } | Transition::LegacyStopped { .. } | Transition::MigrationBlocked { .. } => unreachable!(),
        Transition::OwnershipWaiting { .. } => {},
        Transition::NotificationsQueued { last_report, last_question, .. } => {
            if *last_report < runtime.last_report || *last_question < runtime.last_question { return Err("notification checkpoint cannot go backwards".into()); }
        }
        Transition::Started { run_id, kind, message_ids } => {
            if !can_start(view, at) || run_id.is_empty() || runtime.last_run_id.as_ref() == Some(run_id) { return Err("orchestrator not ready for a new execution".into()); }
            if *kind == RunKind::Compact {
                if !message_ids.is_empty() || runtime.session_id.is_none() || !runtime.compact_pending { return Err("invalid compaction claim".into()); }
            } else {
                if message_ids.is_empty() { return Err("empty work claim".into()); }
                let unique: std::collections::BTreeSet<_> = message_ids.iter().collect();
                if unique.len() != message_ids.len() || message_ids.iter().any(|id| view.messages.get(id).is_none_or(|m| m.session_id != NODE_ID || m.handed_at.is_some() || view.message_attempt(*id).is_some_and(|a| a.blocks()))) { return Err("invalid inbox claim".into()); }
            }
        }
        Transition::SessionBound { run_id, session_id } => {
            active(run_id)?;
            if runtime.phase != Phase::Starting || session_id.is_empty() || session_id == NODE_ID || runtime.session_id.as_ref().is_some_and(|s| s != session_id) { return Err("cannot replace active Claude session".into()); }
        }
        Transition::CancelRequested { run_id } => {
            active(run_id)?;
            if !runtime.is_active() { return Err("execution is not active".into()); }
        }
        Transition::Running { run_id } => {
            active(run_id)?;
            if !matches!(runtime.phase, Phase::Starting | Phase::Compacting) { return Err("execution is not starting".into()); }
        }
        Transition::SessionMissing { run_id } => {
            let run = active(run_id)?;
            if !matches!(runtime.phase, Phase::Starting | Phase::Compacting) || run.initialized { return Err("session absence must be established before execution".into()); }
        }
        Transition::Finished { run_id, outcome, retry_at, .. } => {
            let run = active(run_id)?;
            if *outcome == Outcome::Retryable && run.kind == RunKind::Work && runtime.phase != Phase::Starting { return Err("initialized business work cannot be retried without reconciliation".into()); }
            if runtime.phase == Phase::Failed { return Err("execution already failed; explicit resolution required".into()); }
            if *outcome == Outcome::Retryable && retry_at.is_none() { return Err("retryable failure requires backoff".into()); }
        }
        Transition::RetryApproved { run_id } | Transition::Acknowledged { run_id } => {
            active(run_id)?;
            if runtime.phase != Phase::Failed { return Err("execution does not need reconciliation".into()); }
        }
    }
    Ok(())
}

pub(crate) fn apply(view: &mut View, transition: &Transition, at: Millis) {
    // Stale processes and duplicated delivery must not mutate a newer execution on replay.
    if validate(view, transition, at).is_err() { return; }
    if let Transition::MigrationBlocked { error } = transition { view.orchestrator_migration_error = Some(error.clone()); return; }
    if let Transition::MigrationStarted { session_id, pid } = transition {
        view.orchestrator_migration = Some(Migration { session_id: session_id.clone(), pid: *pid });
        return;
    }
    if let Transition::LegacyStopped { session_id } = transition {
        if let Some(node) = view.nodes.get_mut(session_id) {
            node.alive = false; node.in_daemon = false; node.busy = false; node.ended = true;
            node.pid = None; node.tool = None; node.waiting_permission = false; node.exited_at = Some(at);
        }
        return;
    }
    if let Transition::Enabled { session_id } = transition {
        view.orchestrator_migration = None;
        view.orchestrator_migration_error = None;
        let legacy: std::collections::BTreeSet<String> = view.nodes.values().filter(|n| n.is_orchestrator).map(|n| n.session_id.clone()).chain(session_id.iter().cloned()).collect();
        for q in view.questions.values_mut().filter(|q| legacy.contains(&q.session_id)) { q.session_id = NODE_ID.into(); }
        for r in view.reports.values_mut().filter(|r| legacy.contains(&r.session_id)) { r.session_id = NODE_ID.into(); }
        for m in view.messages.values_mut().filter(|m| legacy.contains(&m.session_id)) { m.session_id = NODE_ID.into(); }
        for attempt in view.delivery_attempts.values_mut().filter(|a| legacy.contains(&a.session_id)) { attempt.session_id = NODE_ID.into(); }
        for old in &legacy { view.removed.insert(old.clone(), at); view.nodes.remove(old); }
        view.removed.remove(NODE_ID);
        view.left_out.remove(NODE_ID);
        // What existed before enabling was already seen by David or a legacy orchestrator.
        let (last_report, last_question) = (view.next_report_id() - 1, view.next_question_id() - 1);
        view.orchestrator = Some(Runtime { session_id: session_id.clone(), generation: 1, phase: Phase::Idle, active_run: None, context: None, last_error: None, retry_at: None, compact_pending: false, updated_at: at, last_run_id: None, consecutive_failures: 0, cancel_requested: false, last_report, last_question, machine_alarm: None, pending_resolutions: vec![] });
    } else {
        let runtime = view.orchestrator.as_mut().expect("validated runtime");
        runtime.updated_at = at;
        match transition {
            Transition::OwnershipWaiting { error } => runtime.last_error = Some(error.clone()),
            Transition::NotificationsQueued { last_report, last_question, machine_alarm } => {
                runtime.last_report = *last_report;
                runtime.last_question = *last_question;
                runtime.machine_alarm = machine_alarm.clone();
            }
            Transition::Started { run_id, kind, message_ids } => {
                runtime.phase = if *kind == RunKind::Compact { Phase::Compacting } else { Phase::Starting };
                runtime.active_run = Some(Run { id: run_id.clone(), kind: *kind, message_ids: message_ids.clone(), started_at: at, initialized: false });
                runtime.last_run_id = Some(run_id.clone());
                runtime.cancel_requested = false;
                runtime.last_error = None;
                runtime.retry_at = None;
            }
            Transition::SessionBound { session_id, .. } => runtime.session_id = Some(session_id.clone()),
            Transition::CancelRequested { .. } => runtime.cancel_requested = true,
            Transition::Running { .. } => {
                runtime.active_run.as_mut().expect("validated active run").initialized = true;
                if runtime.phase != Phase::Compacting { runtime.phase = Phase::Running; }
            }
            Transition::SessionMissing { .. } => { runtime.session_id = None; runtime.generation += 1; runtime.context = None; runtime.compact_pending = false; }
            Transition::Finished { outcome, context, error, retry_at, compact_pending, .. } => {
                runtime.consecutive_failures = if *outcome == Outcome::Success { 0 } else { runtime.consecutive_failures.saturating_add(1) };
                runtime.cancel_requested = false;
                runtime.context = context.clone().or_else(|| runtime.context.clone());
                runtime.last_error = error.clone();
                runtime.retry_at = *retry_at;
                runtime.compact_pending = *compact_pending;
                runtime.phase = if *outcome == Outcome::Success { Phase::Idle } else { Phase::Failed };
                if *outcome == Outcome::Success {
                    if runtime.active_run.as_ref().is_some_and(|run| run.kind == RunKind::Work) { runtime.pending_resolutions.clear(); }
                    acknowledge_messages(&mut view.messages, &mut view.questions, &runtime.active_run.as_ref().expect("validated active run").message_ids, at);
                }
                if *outcome != Outcome::Ambiguous { runtime.active_run = None; }
            }
            Transition::Acknowledged { .. } => {
                let run = runtime.active_run.take().expect("validated active run");
                if run.kind == RunKind::Work {
                    runtime.pending_resolutions.push(Resolution { run_id: run.id.clone(), message_ids: run.message_ids.clone() });
                }
                acknowledge_messages(&mut view.messages, &mut view.questions, &run.message_ids, at);
                if run.kind == RunKind::Compact { runtime.compact_pending = false; }
                runtime.consecutive_failures = 0;
                runtime.cancel_requested = false;
                runtime.phase = Phase::Idle;
                runtime.retry_at = None;
                runtime.last_error = None;
            }
            Transition::RetryApproved { .. } => {
                runtime.consecutive_failures = 0;
                runtime.cancel_requested = false;
                runtime.active_run = None;
                runtime.phase = Phase::Idle;
                runtime.retry_at = None;
                runtime.last_error = None;
            }
            Transition::Enabled { .. } | Transition::MigrationStarted { .. } | Transition::LegacyStopped { .. } | Transition::MigrationBlocked { .. } => unreachable!(),
        }
    }
    project_node(view, at);
}

fn acknowledge_messages(
    messages: &mut std::collections::BTreeMap<u64, crate::model::Message>,
    questions: &mut std::collections::BTreeMap<u64, crate::model::Question>,
    ids: &[u64],
    at: Millis,
) {
    for id in ids {
        if let Some(message) = messages.get_mut(id) {
            message.handed_at = Some(at);
            message.delivered_at = Some(at);
            if let Some(question) = message.answers_question.and_then(|id| questions.get_mut(&id)) {
                if question.status == crate::model::QuestionStatus::Answered { question.status = crate::model::QuestionStatus::Delivered; }
            }
        }
    }
}

pub(crate) fn project_node(view: &mut View, at: Millis) {
    let Some(runtime) = view.orchestrator.as_ref() else { return; };
    let node = view.nodes.entry(NODE_ID.into()).or_insert_with(|| Node::new(NODE_ID, at));
    node.name = Some("Orchestrator".into());
    node.is_orchestrator = true;
    node.in_daemon = false;
    node.has_plugin = true;
    node.alive = runtime.is_active();
    node.busy = runtime.is_active();
    node.ended = !runtime.is_active();
    node.orchestrator_state = Some(runtime.state());
    node.state = runtime.state().node_state();
    node.last_seen = runtime.updated_at;
    if !runtime.is_active() { node.pid = None; node.tool = None; node.waiting_permission = false; }
    node.cwd = None;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Message, MessageFrom};
    fn apply(v: &mut View, t: Transition) {
        let at = v.seq as Millis + 1;
        let e = checked_transition(v, t, at).expect("valid transition");
        v.apply(v.seq + 1, at, &e);
        assert_eq!(v.integrity_issues(), Vec::<String>::new());
    }
    fn enabled() -> View {
        let mut v = View::default();
        apply(&mut v, Transition::Enabled { session_id: Some("claude-session".into()) });
        v
    }
    fn queue(v: &mut View, id: u64) {
        v.apply(v.seq + 1, 2, &Event::MessageQueued(Message { id, session_id: NODE_ID.into(), from: MessageFrom::User, text: format!("request {id}"), answers_question: None, created_at: 2, handed_at: None, delivered_at: None, attachments: vec![], about: None }));
    }
    fn start(v: &mut View, id: &str, ids: &[u64]) {
        apply(v, Transition::Started { run_id: id.into(), kind: RunKind::Work, message_ids: ids.to_vec() });
    }
    fn finished(id: &str, outcome: Outcome) -> Transition {
        Transition::Finished { run_id: id.into(), outcome, context: None, error: None, retry_at: if outcome == Outcome::Retryable { Some(100) } else { None }, compact_pending: false }
    }
    #[test]
    fn cancellation_is_durable_and_fenced_to_its_execution() {
        let mut v = enabled(); queue(&mut v, 1); start(&mut v, "a", &[1]);
        assert!(checked_transition(&v, Transition::CancelRequested { run_id: "stale".into() }, 5).is_err());
        apply(&mut v, Transition::CancelRequested { run_id: "a".into() });
        let restored: View = serde_json::from_str(&serde_json::to_string(&v).unwrap()).unwrap();
        assert!(restored.orchestrator.unwrap().cancel_requested);
        apply(&mut v, finished("a", Outcome::Ambiguous));
        assert!(checked_transition(&v, Transition::CancelRequested { run_id: "a".into() }, 8).is_err());
        apply(&mut v, Transition::RetryApproved { run_id: "a".into() });
        start(&mut v, "b", &[1]);
        assert!(!v.orchestrator.as_ref().unwrap().cancel_requested);
    }
    #[test]
    fn migration_reservation_survives_replay_and_fences_late_hooks() {
        let mut v = View::default();
        let mut n = Node::new("old", 1); n.is_orchestrator = true; n.alive = false;
        v.nodes.insert("old".into(), n);
        apply(&mut v, Transition::MigrationStarted { session_id: "old".into(), pid: None });
        let mut v: View = serde_json::from_str(&serde_json::to_string(&v).unwrap()).unwrap();
        v.apply(3, 3, &Event::Hook(crate::HookEvent { session_id: "old".into(), event: "UserPromptSubmit".into(), ..Default::default() }));
        assert!(!v.nodes["old"].busy);
        v.prune(1_000_000);
        assert!(v.nodes.contains_key("old"));
        apply(&mut v, Transition::LegacyStopped { session_id: "old".into() });
        apply(&mut v, Transition::Enabled { session_id: Some("old".into()) });
        assert!(v.orchestrator_migration.is_none());
    }
    #[test]
    fn snapshots_remain_backwards_compatible_and_idle_is_available() {
        let old = serde_json::to_value(View::default()).unwrap();
        let mut old = old.as_object().unwrap().clone(); old.remove("orchestrator");
        let old: View = serde_json::from_value(old.into()).unwrap();
        assert!(old.orchestrator.is_none());
        let v = enabled();
        assert!(!v.nodes[NODE_ID].alive);
        assert_eq!(v.nodes[NODE_ID].orchestrator_state, Some(OrchestratorState::OnDuty));
        assert!(crate::keeper::has_orchestrator(&v));
        let restored: View = serde_json::from_str(&serde_json::to_string(&v).unwrap()).unwrap();
        assert_eq!(restored, v);
    }
    #[test]
    fn exclusive_claim_and_completion_do_not_consume_work_arriving_during_run() {
        let mut v = enabled(); queue(&mut v, 1); start(&mut v, "a", &[1]);
        queue(&mut v, 2);
        assert_eq!(pending_message_ids(&v), vec![2]);
        assert!(checked_transition(&v, Transition::Started { run_id: "b".into(), kind: RunKind::Work, message_ids: vec![2] }, 5).is_err());
        apply(&mut v, Transition::Running { run_id: "a".into() });
        apply(&mut v, finished("a", Outcome::Success));
        assert!(v.messages[&1].delivered_at.is_some());
        assert_eq!(pending_message_ids(&v), vec![2]);
        start(&mut v, "b", &[2]);
        let before = v.orchestrator.clone();
        v.apply(20, 20, &Event::Orchestrator(finished("a", Outcome::Success)));
        assert_eq!(v.orchestrator, before, "old process cannot complete a newer run");
        assert!(v.messages[&2].handed_at.is_none());
    }
    #[test]
    fn malformed_claim_and_foreign_delivery_cannot_acknowledge_inbox() {
        let mut v = enabled(); queue(&mut v, 1);
        for ids in [vec![1,1], vec![2], vec![]] {
            assert!(checked_transition(&v, Transition::Started { run_id: "a".into(), kind: RunKind::Work, message_ids: ids }, 3).is_err());
        }
        v.apply(3, 3, &Event::MessageHanded { id: 1 });
        assert!(v.messages[&1].handed_at.is_none());
        assert_eq!(v.deliverable_messages(NODE_ID).count(), 0, "terminal delivery never owns this inbox");
    }
    #[test]
    fn ambiguous_crash_requires_fenced_explicit_reconciliation() {
        let mut v = enabled(); queue(&mut v, 1); start(&mut v, "a", &[1]);
        apply(&mut v, finished("a", Outcome::Ambiguous));
        let restored: View = serde_json::from_str(&serde_json::to_string(&v).unwrap()).unwrap();
        assert!(!can_start(&restored, 100000));
        assert!(checked_transition(&v, Transition::RetryApproved { run_id: "old".into() }, 10).is_err());
        apply(&mut v, Transition::RetryApproved { run_id: "a".into() });
        assert_eq!(pending_message_ids(&v), vec![1]);
        assert!(can_start(&v, 100000));
    }
    #[test]
    fn proven_pre_action_failure_waits_before_retry_without_losing_session() {
        let mut v = enabled(); queue(&mut v, 1); start(&mut v, "a", &[1]);
        apply(&mut v, finished("a", Outcome::Retryable));
        assert!(!can_start(&v, 99)); assert!(can_start(&v, 100));
        assert_eq!(v.orchestrator.as_ref().unwrap().session_id.as_deref(), Some("claude-session"));
        assert_eq!(pending_message_ids(&v), vec![1]);
    }
    #[test]
    fn session_replacement_requires_explicit_absence_before_model_init() {
        let mut v = enabled(); queue(&mut v, 1); start(&mut v, "a", &[1]);
        assert!(checked_transition(&v, Transition::SessionBound { run_id: "a".into(), session_id: "other".into() }, 5).is_err());
        apply(&mut v, Transition::SessionMissing { run_id: "a".into() });
        apply(&mut v, Transition::SessionBound { run_id: "a".into(), session_id: "other".into() });
        apply(&mut v, Transition::Running { run_id: "a".into() });
        assert!(checked_transition(&v, Transition::SessionMissing { run_id: "a".into() }, 9).is_err());
        assert_eq!(v.orchestrator.as_ref().unwrap().generation, 2);
    }
    #[test]
    fn compaction_never_claims_or_completes_business_messages() {
        let mut v = enabled(); queue(&mut v, 1); start(&mut v, "a", &[1]);
        let mut done = finished("a", Outcome::Success);
        if let Transition::Finished { compact_pending, .. } = &mut done { *compact_pending = true; }
        apply(&mut v, done);
        apply(&mut v, Transition::Started { run_id: "compact".into(), kind: RunKind::Compact, message_ids: vec![] });
        queue(&mut v, 2);
        apply(&mut v, finished("compact", Outcome::Success));
        assert_eq!(pending_message_ids(&v), vec![2]);
        assert!(!v.orchestrator.as_ref().unwrap().compact_pending, "successful no-op does not loop");
    }
    #[test]
    fn pending_and_ambiguous_payloads_survive_retention() {
        let mut v = enabled(); queue(&mut v, 1); queue(&mut v, 2); start(&mut v, "a", &[1]);
        apply(&mut v, finished("a", Outcome::Ambiguous));
        v.prune(1_000_000);
        assert_eq!(v.messages.len(), 2);
        assert!(v.nodes.contains_key(NODE_ID));
        assert!(v.integrity_issues().is_empty());
        apply(&mut v, Transition::RetryApproved { run_id: "a".into() });
        start(&mut v, "b", &[1]); apply(&mut v, finished("b", Outcome::Success));
        v.prune(2_000_000);
        assert!(!v.messages.contains_key(&1)); assert!(v.messages.contains_key(&2));
        assert_eq!(v.next_message_id(), 3);
    }
    #[test]
    fn migration_refuses_live_legacy_then_moves_inbox_and_tombstones_identity() {
        let mut v = View::default();
        let mut old = Node::new("legacy", 1); old.is_orchestrator = true;
        v.nodes.insert("legacy".into(), old);
        assert!(checked_transition(&v, Transition::Enabled { session_id: Some("legacy".into()) }, 2).is_err());
        queue(&mut v, 1); v.messages.get_mut(&1).unwrap().session_id = "legacy".into();
        v.nodes.get_mut("legacy").unwrap().alive = false;
        apply(&mut v, Transition::Enabled { session_id: Some("legacy".into()) });
        assert_eq!(v.messages[&1].session_id, NODE_ID);
        assert!(v.removed.contains_key("legacy"));
        assert!(!v.nodes.contains_key("legacy"));
    }
    #[test]
    fn normal_session_end_does_not_make_logical_orchestrator_offline() {
        let mut v = enabled();
        v.apply(3, 3, &Event::Hook(crate::HookEvent { session_id: NODE_ID.into(), event: "SessionEnd".into(), ..Default::default() }));
        assert_eq!(v.nodes[NODE_ID].orchestrator_state, Some(OrchestratorState::OnDuty));
        assert!(v.integrity_issues().is_empty());
    }
    #[test]
    fn acknowledgement_releases_only_reconciled_batch_without_replaying_it() {
        let mut view = enabled(); queue(&mut view, 1); start(&mut view, "old", &[1]);
        queue(&mut view, 2);
        assert!(checked_transition(&view, Transition::Acknowledged { run_id: "old".into() }, 20).is_err(), "cannot acknowledge live work");
        apply(&mut view, Transition::CancelRequested { run_id: "old".into() });
        apply(&mut view, finished("old", Outcome::Ambiguous));
        assert!(checked_transition(&view, Transition::Acknowledged { run_id: "stale".into() }, 20).is_err());
        let mut view: View = serde_json::from_str(&serde_json::to_string(&view).unwrap()).unwrap();
        apply(&mut view, Transition::Acknowledged { run_id: "old".into() });
        assert!(view.messages[&1].handed_at.is_some());
        assert_eq!(pending_message_ids(&view), vec![2]);
        assert!(!view.orchestrator.as_ref().unwrap().cancel_requested);
        assert!(can_start(&view, 100));
        start(&mut view, "new", &[2]);
        assert_eq!(view.orchestrator.as_ref().unwrap().active_run.as_ref().unwrap().message_ids, vec![2]);
        assert!(checked_transition(&view, Transition::Acknowledged { run_id: "old".into() }, 20).is_err(), "stale UI cannot consume newer batch");
    }
    #[test]
    fn acknowledging_failed_compaction_does_not_discard_pending_messages_or_repeat_compact() {
        let mut view = enabled(); queue(&mut view, 1); start(&mut view, "work", &[1]);
        let mut done = finished("work", Outcome::Success);
        if let Transition::Finished { compact_pending, .. } = &mut done { *compact_pending = true; }
        apply(&mut view, done);
        apply(&mut view, Transition::Started { run_id: "compact".into(), kind: RunKind::Compact, message_ids: vec![] });
        queue(&mut view, 2);
        let mut failed = finished("compact", Outcome::Ambiguous);
        if let Transition::Finished { compact_pending, .. } = &mut failed { *compact_pending = true; }
        apply(&mut view, failed);
        apply(&mut view, Transition::Acknowledged { run_id: "compact".into() });
        assert_eq!(pending_message_ids(&view), vec![2]);
        assert!(!view.orchestrator.as_ref().unwrap().compact_pending);
        start(&mut view, "new", &[2]);
    }

    #[test]
    fn compact_session_absence_is_allowed_only_before_initialization() {
        let mut view = enabled(); queue(&mut view, 1); start(&mut view, "work", &[1]);
        let mut done = finished("work", Outcome::Success);
        if let Transition::Finished { compact_pending, .. } = &mut done { *compact_pending = true; }
        apply(&mut view, done);
        apply(&mut view, Transition::Started { run_id: "compact".into(), kind: RunKind::Compact, message_ids: vec![] });
        let mut absent = view.clone();
        apply(&mut absent, Transition::SessionMissing { run_id: "compact".into() });
        assert!(absent.orchestrator.as_ref().unwrap().session_id.is_none());
        assert!(!absent.orchestrator.as_ref().unwrap().compact_pending);
        apply(&mut view, Transition::Running { run_id: "compact".into() });
        assert!(checked_transition(&view, Transition::SessionMissing { run_id: "compact".into() }, 50).is_err());
    }

    #[test]
    fn user_resolutions_accumulate_until_successful_work_consumes_them() {
        let mut view = enabled(); queue(&mut view, 1); start(&mut view, "old", &[1]);
        apply(&mut view, finished("old", Outcome::Ambiguous));
        apply(&mut view, Transition::Acknowledged { run_id: "old".into() });
        queue(&mut view, 2); start(&mut view, "second", &[2]);
        let mut failure = finished("second", Outcome::Ambiguous);
        if let Transition::Finished { compact_pending, .. } = &mut failure { *compact_pending = true; }
        apply(&mut view, failure);
        apply(&mut view, Transition::Acknowledged { run_id: "second".into() });
        assert_eq!(view.orchestrator.as_ref().unwrap().pending_resolutions.len(), 2);
        apply(&mut view, Transition::Started { run_id: "compact".into(), kind: RunKind::Compact, message_ids: vec![] });
        apply(&mut view, finished("compact", Outcome::Success));
        assert_eq!(view.orchestrator.as_ref().unwrap().pending_resolutions.len(), 2);
        let mut view: View = serde_json::from_str(&serde_json::to_string(&view).unwrap()).unwrap();
        queue(&mut view, 3); start(&mut view, "new", &[3]);
        apply(&mut view, finished("new", Outcome::Retryable));
        assert_eq!(view.orchestrator.as_ref().unwrap().pending_resolutions.len(), 2);
        // A later retry still carries both user resolutions.
        let event = checked_transition(&view, Transition::Started { run_id: "retry".into(), kind: RunKind::Work, message_ids: vec![3] }, 100).unwrap();
        view.apply(view.seq + 1, 100, &event);
        apply(&mut view, finished("retry", Outcome::Success));
        assert!(view.orchestrator.as_ref().unwrap().pending_resolutions.is_empty());
    }

}
