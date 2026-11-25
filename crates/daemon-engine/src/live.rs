//! What the daemon observes of each Claude session through its hooks and its node mod. The
//! domain never records it: it is the live state a client shows next to the projection, kept in
//! memory with the legacy reducer so a node keeps the shape every client already reads.
use alondra_engine_kernel::domain_runtime::RuntimeRecords as _;
use crate::locks::{Poisonable as _, AfterPanic as _};
use alondra_core::{Event, HookEvent, Node, View, now_millis};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::sync::{Mutex, atomic::{AtomicU64, Ordering}};

const CLOSED_FILE: &str = "closed-sessions.json";
/// Where older daemons kept the nodes of released tasks, which the domain keeps with each release now.
const CLOSED_TASKS_FILE: &str = "closed-task-nodes.json";
const ABORTED_REPLACES_FILE: &str = "aborted-replaces.json";

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum LiveChangeKey {
    Node(String),
}

pub use alondra_engine_ports::sdk::{ClosedNode, Relaunch};

fn save<T: serde::Serialize>(file: &std::path::Path, value: &T) {
    let next = file.with_extension("next");
    let written = serde_json::to_vec(value).map_err(std::io::Error::from)
        .and_then(|bytes| std::fs::write(&next, bytes))
        .and_then(|()| std::fs::rename(&next, file));
    if let Err(error) = written {
        log!("could not save {}: {error}", file.display());
    }
}

#[derive(Default)]
pub struct Live {
    /// A holder that panics can leave it half-changed, so the daemon exits: `View::apply` changes
    /// several maps in a row, and the hooks rebuild the whole view after a fresh start.
    view: Mutex<View>,
    sequence: AtomicU64,
    pub changed: crate::node_reports::LiveChanges,
    /// Recorded while the changed data is locked, before its subscribers are woken.
    changes: alondra_foundation::changes::Changes<LiveChangeKey>,
    /// A holder that panics leaves it whole, so it is recovered: one insert.
    presence: Mutex<HashMap<String, i64>>,
    pub mods: crate::node_reports::Reports,
    pub pushes: crate::node_push::Pushes,
    /// A sandbox daemon registers the Agent of any session whose mod connects, so a hand-launched
    /// probe node can be sent to.
    pub sandbox: std::sync::atomic::AtomicBool,
    pub usage_pause: crate::usage_pause::UsagePause,
    pub account_wait: crate::accounts::AccountWait,
    /// Asks the accounts check to run now.
    pub account_check: tokio::sync::Notify,
    pub(crate) initial_account_check_pending: std::sync::atomic::AtomicBool,
    /// Sign-in checks wait for Claude to end instead of its deadline: a test's Claude is a script
    /// it just wrote, whose first run waits while macOS checks the new executable.
    pub(crate) unbounded_claude_status: std::sync::atomic::AtomicBool,
    pub(crate) measured: crate::usage_probe::Measured,
    pub api_watch: crate::api_failures::ApiWatch,
    /// The one clock over what live tasks are owed. A holder that panics leaves it whole: each pass
    /// replaces whole entries.
    pub(crate) obligations: Mutex<crate::obligation_watch::Watch>,
    /// When each task's node process last wrote anything. A holder that panics leaves it whole:
    /// one insert.
    pub outputs: Mutex<std::collections::BTreeMap<alondra_domain_rules::TaskId, alondra_core::Millis>>,
    /// A holder that panics leaves it whole, so it is recovered: one insert or remove of a marker.
    watching: Mutex<std::collections::HashSet<String>>,
    launching: tokio::sync::watch::Sender<std::collections::HashSet<String>>,
    /// When Alondra last cut the turn of a session it has to close, by session.
    /// A holder that panics leaves it whole, so it is recovered: one insert or remove.
    close_interrupts: Mutex<HashMap<String, i64>>,
    /// A holder that panics leaves it whole, so it is recovered: one insert, then a save whose
    /// failure memory already outlives.
    closed: Mutex<std::collections::BTreeSet<String>>,
    closed_file: Option<std::path::PathBuf>,
    /// A holder that panics leaves it whole, so it is recovered: as `closed`.
    aborted_replaces: Mutex<std::collections::BTreeMap<String, String>>,
    aborted_replaces_file: Option<std::path::PathBuf>,
}

impl Live {
    pub fn change_cursor(&self) -> alondra_foundation::changes::Cursor {
        self.with_changes(|changes| changes.cursor())
    }

    pub fn changes_since(&self, cursor: &alondra_foundation::changes::Cursor) -> alondra_foundation::changes::Delta<LiveChangeKey> {
        self.with_changes(|changes| changes.since(cursor))
    }

    fn tracked<'a, T>(&self, source: &'a Mutex<T>, what: &str) -> std::sync::MutexGuard<'a, T> {
        source.lock().or_recover(what, || {
            self.changes.reset();
            source.clear_poison();
        })
    }

    fn with_changes<T>(&self, read: impl FnOnce(&alondra_foundation::changes::Changes<LiveChangeKey>) -> T) -> T {
        let _view = self.view.lock_or_exit("the live sessions");
        let _closed = self.tracked(&self.closed, "the closed sessions");
        let _aborted = self.tracked(&self.aborted_replaces, "the closed sessions");
        let _interrupts = self.tracked(&self.close_interrupts, "the close interrupts");
        read(&self.changes)
    }

    pub fn persisted_in(home: &std::path::Path) -> Self {
        let file = home.join(CLOSED_FILE);
        let closed = std::fs::read(&file).ok().and_then(|bytes| serde_json::from_slice(&bytes).ok()).unwrap_or_default();
        if let Err(error) = std::fs::remove_file(home.join(CLOSED_TASKS_FILE)) && error.kind() != std::io::ErrorKind::NotFound {
            log!("could not remove {CLOSED_TASKS_FILE}, which nothing reads any more: {error}");
        }
        let aborted_replaces_file = home.join(ABORTED_REPLACES_FILE);
        let aborted_replaces = std::fs::read(&aborted_replaces_file).ok().and_then(|bytes| serde_json::from_slice(&bytes).ok()).unwrap_or_default();
        Self {
            aborted_replaces: Mutex::new(aborted_replaces),
            aborted_replaces_file: Some(aborted_replaces_file),
            usage_pause: crate::usage_pause::UsagePause::default(),
            outputs: Default::default(),
            closed: Mutex::new(closed),
            closed_file: Some(file),
            ..Self::default()
        }
    }

    /// The user closed the session: it is not brought back and closes, also after a restart.
    pub fn mark_closed(&self, session_id: &str) {
        let mut closed = self.tracked(&self.closed, "the closed sessions");
        if closed.insert(session_id.to_owned()) {
            self.changes.record(LiveChangeKey::Node(session_id.to_owned()));
        }
        if let Some(file) = &self.closed_file {
            save(file, &*closed);
        }
        drop(closed);
        self.changed.publish();
    }

    /// The orchestrator's replacement of this session's node stopped short because the node could
    /// not be stopped: if it ends later, that is why, not the user closing it.
    pub fn note_aborted_replace(&self, session_id: &str, why: &str) {
        let mut aborted = self.tracked(&self.aborted_replaces, "the closed sessions");
        let changed = aborted.get(session_id).map(String::as_str) != Some(why);
        aborted.insert(session_id.to_owned(), why.to_owned());
        if changed { self.changes.record(LiveChangeKey::Node(session_id.to_owned())); }
        if let Some(file) = &self.aborted_replaces_file {
            save(file, &*aborted);
        }
    }

    pub fn take_aborted_replace(&self, session_id: &str) -> Option<String> {
        let mut aborted = self.tracked(&self.aborted_replaces, "the closed sessions");
        let why = aborted.remove(session_id)?;
        self.changes.record(LiveChangeKey::Node(session_id.to_owned()));
        if let Some(file) = &self.aborted_replaces_file {
            save(file, &*aborted);
        }
        Some(why)
    }

    pub fn closed_sessions(&self) -> Vec<String> {
        self.tracked(&self.closed, "the closed sessions").iter().cloned().collect()
    }

    pub fn is_closed(&self, session_id: &str) -> bool {
        self.tracked(&self.closed, "the closed sessions").contains(session_id)
    }

    /// False when something is watching the session's connection back already.
    /// Takes the right to launch a node for a task; false while another launch of it runs.
    pub fn begin_launch(&self, task_id: &str) -> bool {
        self.launching.send_if_modified(|launching| launching.insert(task_id.to_owned()))
    }

    pub fn is_launching(&self, task_id: &str) -> bool {
        self.launching.borrow().contains(task_id)
    }

    pub fn end_launch(&self, task_id: &str) {
        self.launching.send_if_modified(|launching| launching.remove(task_id));
    }

    pub async fn until_launch_ends(&self, task_id: &str) {
        let mut launching = self.launching.subscribe();
        launching.wait_for(|launching| !launching.contains(task_id)).await.expect("Live owns the sender");
    }

    pub fn begin_watch(&self, session_id: &str) -> bool {
        self.watching.lock_or_recover("the closed sessions").insert(session_id.to_owned())
    }

    pub fn end_watch(&self, session_id: &str) {
        self.watching.lock_or_recover("the closed sessions").remove(session_id);
    }

    /// When Alondra last cut the session's turn to close it, if it did.
    pub fn close_interrupted_at(&self, session_id: &str) -> Option<i64> {
        self.tracked(&self.close_interrupts, "the close interrupts").get(session_id).copied()
    }

    pub fn set_close_interrupted(&self, session_id: &str, at: Option<i64>) {
        let mut interrupts = self.tracked(&self.close_interrupts, "the close interrupts");
        let previous = match at {
            Some(at) => interrupts.insert(session_id.to_owned(), at),
            None => interrupts.remove(session_id),
        };
        if previous != at { self.changes.record(LiveChangeKey::Node(session_id.to_owned())); }
    }

    /// A hook a session sent. What a node of the Agent SDK does is its task's lifecycle, so its
    /// hooks never feed the deduction kept for `claude --bg` nodes.
    pub(crate) fn observe_hook(&self, state: &alondra_domain_rules::DomainState, payload: &Value) {
        let session = payload.get("session_id").and_then(Value::as_str).unwrap_or_default();
        if !through_sdk(state, &alondra_domain_rules::AgentId(session.into())) {
            self.observe(payload);
        }
    }

    pub fn observe(&self, payload: &Value) {
        let Some(event) = hook_event(payload) else { return };
        let session_id = event.session_id.clone();
        let mut view = self.view.lock_or_exit("the live sessions");
        let now = now_millis();
        if !view.nodes.contains_key(&session_id) {
            view.apply(self.next(), now, &Event::Joined { session_id: session_id.clone() });
        }
        let limit = event.event == "StopFailure" && event.error_type.as_deref() == Some("rate_limit");
        if event.event == "StopFailure" && matches!(event.error_type.as_deref(), Some("authentication_failed" | "billing_error")) {
            self.account_check.notify_one();
        }
        let event = Event::Hook(event);
        view.apply(self.next(), now, &event);
        self.record_view_change(&event);
        drop(view);
        if limit {
            self.usage_pause.exhaust(self.measured.reset_after_limit(&alondra_core::accounts::AccountId::default_account(), now), now);
        }
        self.changed.publish();
    }

    pub(crate) fn observe_prompt(&self, session_id: &str, origin: alondra_core::PromptOrigin) {
        let mut view = self.view.lock_or_exit("the live sessions");
        let now = now_millis();
        if !view.nodes.contains_key(session_id) {
            view.apply(self.next(), now, &Event::Joined { session_id: session_id.to_owned() });
        }
        view.apply(self.next(), now, &Event::PromptSubmitted { session_id: session_id.to_owned(), origin });
        self.changes.record(LiveChangeKey::Node(session_id.to_owned()));
        drop(view);
        self.changed.publish();
    }

    fn next(&self) -> u64 {
        self.sequence.fetch_add(1, Ordering::Relaxed) + 1
    }

    pub fn touch(&self, client: &str) {
        self.presence.lock_or_recover("the client presence").insert(client.to_owned(), now_millis());
    }

    /// Whether the user was at any of these clients at or after `since`.
    pub fn touched_since(&self, clients: &[&str], since: i64) -> bool {
        let presence = self.presence.lock_or_recover("the client presence");
        clients.iter().any(|client| presence.get(*client).is_some_and(|at| *at >= since))
    }

    pub fn last_touched(&self, client: &str) -> Option<i64> {
        self.presence.lock_or_recover("the client presence").get(client).copied()
    }

    pub fn apply(&self, event: &Event) {
        let mut view = self.view.lock_or_exit("the live sessions");
        view.apply(self.next(), now_millis(), event);
        self.record_view_change(event);
        drop(view);
        self.changed.publish();
    }

    fn record_view_change(&self, event: &Event) {
        let session = match event {
            Event::Hook(hook) if matches!(hook.event.as_str(),
                "SessionStart" | "SessionEnd" | "UserPromptSubmit" | "PreToolUse"
                | "PostToolUse" | "PostToolUseFailure" | "PermissionRequest" | "PermissionDenied"
                | "Elicitation" | "ElicitationResult" | "Notification" | "Stop" | "StopFailure"
                | "SubagentStart" | "SubagentStop" | "PostCompact" | "PreCompact"
                | "CwdChanged" | "MessageDisplay") => Some(hook.session_id.as_str()),
            Event::DaemonSeen(seen) => Some(seen.session_id.as_str()),
            Event::ProcessExited { session_id, .. } | Event::NodeRemoved { session_id }
            | Event::Acknowledged { session_id } | Event::MarkedOrchestrator { session_id }
            | Event::Joined { session_id } | Event::Classified { session_id, .. }
            | Event::WaitingMachine { session_id, .. } | Event::AttachChanged { session_id, .. }
            | Event::PromptSubmitted { session_id, .. } | Event::MenuSeen { session_id, .. }
            | Event::Resumed { session_id, .. } | Event::ContextMeasured { session_id, .. }
            | Event::Compaction { session_id, .. } | Event::Focused { session_id, .. } => Some(session_id.as_str()),
            _ => None,
        };
        match session {
            Some(session) => self.changes.record(LiveChangeKey::Node(session.to_owned())),
            None => self.changes.reset(),
        }
    }

    pub fn node(&self, session_id: &str) -> Option<Node> {
        self.view.lock_or_exit("the live sessions").nodes.get(session_id).cloned()
    }

    pub fn nodes(&self) -> Vec<Node> {
        self.view.lock_or_exit("the live sessions").nodes.values().cloned().collect()
    }

    pub fn threads(&self, session_id: &str) -> Option<Vec<alondra_core::activity::Activity>> {
        if let Some(activity) = self.mods.session(session_id).as_ref().and_then(|session| crate::codex_live::activity(session).or_else(|| crate::antigravity_live::activity(session)).or_else(|| crate::sdk_live::activity(session))) {
            return Some(activity);
        }
        let view = self.view.lock_or_exit("the live sessions");
        let node = view.nodes.get(session_id)?;
        Some(view.activity(node, now_millis()))
    }
}

fn hook_event(payload: &Value) -> Option<HookEvent> {
    use crate::hook_text::{hook_detail, named_task, str_field, tool_summary};
    let session_id = str_field(payload, "session_id")?;
    let event = str_field(payload, "hook_event_name").unwrap_or_default();
    if event == "MessageDisplay" && payload.get("final") != Some(&Value::Bool(true)) {
        return None;
    }
    let tool = str_field(payload, "tool_name");
    Some(HookEvent {
        session_id,
        tool_summary: tool_summary(tool.as_deref(), payload.get("tool_input")),
        last_message: str_field(payload, "last_assistant_message"),
        notification_type: str_field(payload, "notification_type"),
        error_type: str_field(payload, "error_type").or_else(|| str_field(payload, "error")),
        cwd: str_field(payload, "cwd"),
        agent_id: str_field(payload, "agent_id"),
        agent_type: str_field(payload, "agent_type"),
        task: named_task(tool.as_deref(), payload.get("tool_input")),
        detail: hook_detail(&event, payload),
        prompt: str_field(payload, "prompt").map(|text| text.chars().take(2000).collect()),
        tool_use_id: str_field(payload, "tool_use_id"),
        tool_name: tool,
        event,
        ..Default::default()
    })
}

/// The nodes `/runtime` lists. A node of the Agent SDK reads its state from its task's lifecycle,
/// never from what the deduction kept for `claude --bg` nodes made of it, and is listed even when
/// that deduction never saw it. `since` is when a task last changed state.
pub fn runtime_nodes(deduced: Vec<Node>, state: &alondra_domain_rules::DomainState, since: impl Fn(&alondra_domain_rules::TaskId) -> Option<i64>) -> Vec<Value> {
    use alondra_domain_rules::AgentId;
    let sdk = |agent: &AgentId| through_sdk(state, agent);
    let mut nodes: Vec<Node> = deduced.into_iter().filter(|node| state.agents.contains_key(&AgentId(node.session_id.clone()))).collect();
    let unseen: Vec<Node> = state.agents.keys()
        .filter(|agent| sdk(agent) && state.sdk_live(agent).is_some() && !nodes.iter().any(|node| node.session_id == agent.0))
        .map(|agent| Node::new(&agent.0, state.node_task(agent).and_then(|task| since(&task.id)).unwrap_or_default()))
        .collect();
    nodes.extend(unseen);
    nodes.iter_mut().filter(|node| sdk(&AgentId(node.session_id.clone()))).for_each(|node| from_lifecycle(node, state));
    nodes.iter().map(|node| client_node(node, state.agents.get(&AgentId(node.session_id.clone())).map(|agent| agent.name.as_str()))).collect()
}

fn from_lifecycle(node: &mut Node, state: &alondra_domain_rules::DomainState) {
    use alondra_core::{NodeState, NodeStatus, StateSource};
    use alondra_domain::LiveState;
    let agent = alondra_domain_rules::AgentId(node.session_id.clone());
    let result = state.node_task(&agent).and_then(|task| task.final_presentation.as_ref());
    node.state = match state.sdk_live(&agent) {
        Some(LiveState::Working(_)) => NodeState::Working,
        Some(LiveState::WaitingUser(cards)) if cards.iter().all(|card| Some(card) == result) => NodeState::WaitingMerge,
        Some(LiveState::Paused) => NodeState::Idle,
        Some(LiveState::WaitingUser(_) | LiveState::WaitingTask(_)) => NodeState::WaitingAnswer,
        Some(LiveState::WaitingCapacity(_)) => NodeState::WaitingMachine,
        None => NodeState::Done,
    };
    node.status = match node.state {
        NodeState::Working => NodeStatus::Busy,
        NodeState::WaitingMachine => NodeStatus::WaitingMachine,
        _ => NodeStatus::Free,
    };
    node.busy = node.state == NodeState::Working;
    node.state_source = StateSource::Daemon;
    node.evidence = None;
}

fn through_sdk(state: &alondra_domain_rules::DomainState, agent: &alondra_domain_rules::AgentId) -> bool {
    state.through_sdk(agent) || state.node_phase(agent) == Some(&alondra_domain_rules::NodePhase::ThroughSdk)
}

/// A node as a client reads it: what the daemon keeps for itself stays out.
pub(crate) fn client_node(node: &Node, name: Option<&str>) -> Value {
    let mut value = serde_json::to_value(node).unwrap_or(Value::Null);
    if let Some(fields) = value.as_object_mut() {
        fields.remove("alive");
        fields.remove("ledger");
        if node.name.is_none() {
            fields.insert("name".into(), json!(name));
        }
        fields.insert("project".into(), json!(node.project()));
    }
    value
}

#[cfg(test)]
mod change_tests {
    use super::*;
    use alondra_foundation::changes::{Changed, Cursor};
    use std::collections::BTreeSet;

    fn keys(live: &Live, cursor: &Cursor, expected: impl IntoIterator<Item = LiveChangeKey>) -> Cursor {
        let delta = live.changes_since(cursor);
        let expected: BTreeSet<_> = expected.into_iter().collect();
        assert!(matches!(delta.changed, Changed::Keys(actual) if actual == expected));
        delta.through
    }

    #[test]
    fn hooks_and_prompts_name_the_session_before_publishing_the_wake() {
        let live = Live::default();
        let cursor = live.change_cursor();
        let mut wake = live.changed.subscribe();
        live.observe(&json!({"session_id": "node", "hook_event_name": "Stop", "last_assistant_message": "Ready"}));
        assert!(wake.has_changed().unwrap());
        wake.borrow_and_update();
        let cursor = keys(&live, &cursor, [LiveChangeKey::Node("node".into())]);
        assert_eq!(live.node("node").unwrap().last_message.as_deref(), Some("Ready"));
        live.observe_prompt("other", alondra_core::PromptOrigin::Composer);
        assert!(wake.has_changed().unwrap());
        keys(&live, &cursor, [LiveChangeKey::Node("other".into())]);
        assert!(live.node("other").is_some());
    }

    #[test]
    fn removals_keep_the_node_key_after_its_data_has_gone() {
        let live = Live::default();
        live.apply(&Event::Joined { session_id: "gone".into() });
        let cursor = live.change_cursor();
        live.apply(&Event::NodeRemoved { session_id: "gone".into() });
        let cursor = keys(&live, &cursor, [LiveChangeKey::Node("gone".into())]);
        assert!(live.node("gone").is_none());
        live.observe(&json!({"session_id": "gone", "hook_event_name": "SessionStart"}));
        keys(&live, &cursor, [LiveChangeKey::Node("gone".into())]);
        assert!(live.node("gone").is_none(), "a late hook cannot undo the reducer's tombstone");
    }

    #[test]
    fn close_bookkeeping_records_actual_updates_and_deletions() {
        let live = Live::default();
        let cursor = live.change_cursor();
        live.note_aborted_replace("node", "Still running");
        live.set_close_interrupted("node", Some(1));
        let cursor = keys(&live, &cursor, [LiveChangeKey::Node("node".into())]);
        live.note_aborted_replace("node", "Still running");
        live.set_close_interrupted("node", Some(1));
        keys(&live, &cursor, []);
        assert_eq!(live.take_aborted_replace("node").as_deref(), Some("Still running"));
        live.set_close_interrupted("node", None);
        let cursor = keys(&live, &cursor, [LiveChangeKey::Node("node".into())]);
        assert!(live.take_aborted_replace("node").is_none());
        live.set_close_interrupted("node", None);
        keys(&live, &cursor, []);
    }

    #[test]
    fn unknown_hooks_and_global_events_reset_the_delta() {
        let live = Live::default();
        let cursor = live.change_cursor();
        live.observe(&json!({"session_id": "node", "hook_event_name": "FutureHook"}));
        let delta = live.changes_since(&cursor);
        assert!(matches!(delta.changed, Changed::Reset));
        assert!(live.node("node").is_some());
        live.apply(&Event::LaunchedOnly);
        assert!(matches!(live.changes_since(&delta.through).changed, Changed::Reset));
    }

    #[test]
    fn recovering_each_source_resets_even_when_a_getter_reads_before_the_cursor() {
        fn interrupted<T>(source: &Mutex<T>, mutate: impl FnOnce(&mut T)) {
            let failed = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                let mut value = source.lock().unwrap();
                mutate(&mut value);
                panic!("the mutation did not reach its journal");
            }));
            assert!(failed.is_err());
            assert!(source.is_poisoned());
        }

        for source in 0..3 {
            for read_first in [false, true] {
                let live = Live::default();
                let cursor = live.change_cursor();
                match source {
                    0 => interrupted(&live.closed, |closed| { closed.insert("node".into()); }),
                    1 => interrupted(&live.aborted_replaces, |aborted| { aborted.insert("node".into(), "Still running".into()); }),
                    2 => interrupted(&live.close_interrupts, |interrupts| { interrupts.insert("node".into(), 1); }),
                    _ => unreachable!(),
                }
                let read = || match source {
                    0 => assert!(live.is_closed("node")),
                    1 => assert_eq!(live.take_aborted_replace("node").as_deref(), Some("Still running")),
                    2 => assert_eq!(live.close_interrupted_at("node"), Some(1)),
                    _ => unreachable!(),
                };
                if read_first { read(); }
                let delta = live.changes_since(&cursor);
                assert!(matches!(delta.changed, Changed::Reset), "source {source}, read first {read_first}");
                if !read_first { read(); }
                let recovered = live.change_cursor();
                keys(&live, &recovered, []);
                assert!(!live.closed.is_poisoned());
                assert!(!live.aborted_replaces.is_poisoned());
                assert!(!live.close_interrupts.is_poisoned());
            }
        }
    }
}

#[cfg(test)]
mod sdk_node_tests {
    use super::*;
    use alondra_domain_rules::{Actor, Command, CommandEnvelope, DomainState, LaunchPlan, PROTOCOL_VERSION};
    use std::collections::BTreeSet;

    fn apply(state: &mut DomainState, actor: Actor, command: Command) {
        let envelope = CommandEnvelope { protocol_version: PROTOCOL_VERSION, data_epoch: state.data_epoch.clone(),
            command_id: format!("c-{}", state.revision).into(), expected_revision: state.revision, command };
        state.execute(&actor, &envelope).unwrap();
    }

    /// Agents `sdk-seen` and `sdk-unseen` run through the Agent SDK, each on its own task being
    /// started; `bg` is a node of `claude --bg` an older release stored, which reads as closed.
    fn world() -> DomainState {
        let mut state = DomainState::new("epoch");
        for agent in ["sdk-seen", "sdk-unseen", "bg"] {
            apply(&mut state, Actor::System, Command::RegisterAgent { agent_id: agent.into(), name: agent.into(), at: 0 });
        }
        state.agents.get_mut(&"bg".into()).unwrap().phase = alondra_domain_rules::NodePhase::Closed;
        for agent in ["sdk-seen", "sdk-unseen"] {
            let (request, task) = (format!("r-{agent}"), format!("t-{agent}"));
            apply(&mut state, Actor::User { user_id: "u".into() }, Command::CreateRequest { request_id: request.clone().into(), body: "Notes".into(), auto_merge: false, maintenance: false, message: false });
            apply(&mut state, Actor::Orchestrator { generation: 0 }, Command::CreateTask { priority: Default::default(), plan_items: None, continues: None, task_id: task.clone().into(), request_id: request.into(), title: "Notes".into(),
                agent_id: None, review_prs: false, dependencies: BTreeSet::new(), stack: None, launch: Some(LaunchPlan::new("node", "/tmp")) });
            apply(&mut state, Actor::System, Command::AdmitExecution { provider: alondra_core::accounts::Provider::Claude, task_id: task.clone().into(), agent_id: agent.into(),
                conversation: format!("conversation-{task}"), instance: format!("instance-{task}"), host: crate::sdk_live::HOST.into(), model: None, account: None, leaves_limit: None });
        }
        state
    }

    #[test]
    fn a_hook_of_a_node_of_the_agent_sdk_never_reaches_the_deduction_of_claude_bg_nodes() {
        let state = world();
        let live = Live::default();
        let cursor = live.change_cursor();
        live.observe_hook(&state, &json!({"session_id": "sdk-unseen", "hook_event_name": "Stop"}));
        assert!(matches!(live.changes_since(&cursor).changed,
            alondra_foundation::changes::Changed::Keys(keys) if keys.is_empty()));
        for agent in ["sdk-seen", "bg"] {
            live.observe_hook(&state, &json!({"session_id": agent, "hook_event_name": "Stop", "last_assistant_message": "Ready"}));
        }
        assert!(live.node("sdk-seen").is_none(), "{:?}", live.node("sdk-seen"));
        assert!(live.node("bg").is_some(), "a claude --bg node is still deduced from its hooks");
    }

    #[tokio::test]
    async fn a_billing_or_sign_in_failure_checks_the_account_and_an_overload_does_not() {
        for (error, checks) in [("billing_error", true), ("authentication_failed", true), ("overloaded", false)] {
            let live = Live::default();
            live.observe(&json!({"session_id": "bg", "hook_event_name": "StopFailure", "error_type": error}));
            let checked = tokio::time::timeout(std::time::Duration::ZERO, live.account_check.notified()).await.is_ok();
            assert_eq!(checked, checks, "{error}");
            assert!(!live.node("bg").unwrap().usage_limited, "{error} is no usage limit");
        }
    }

    /// `/runtime` lists a node of the Agent SDK with the state of its task's lifecycle, whether or
    /// not the deduction kept for `claude --bg` nodes saw it, and whatever that deduction made of it.
    #[test]
    fn the_runtime_reads_a_node_of_the_agent_sdk_from_its_task_s_lifecycle() {
        let state = world();
        let deduced: Vec<Node> = ["sdk-seen", "bg"].into_iter().map(|agent| Node {
            state: alondra_core::NodeState::Idle, status: alondra_core::NodeStatus::Free, ..Node::new(agent, 5)
        }).collect();
        let nodes = runtime_nodes(deduced, &state, |_| Some(7));
        let node = |id: &str| nodes.iter().find(|node| node["session_id"] == id).cloned().unwrap_or_else(|| panic!("{id} is listed: {nodes:?}"));
        for agent in ["sdk-seen", "sdk-unseen"] {
            assert_eq!((node(agent)["state"].as_str(), node(agent)["status"].as_str()), (Some("working"), Some("busy")), "{agent}: Alondra is starting its session");
        }
        assert_eq!(node("sdk-unseen")["state_since"], json!(7), "since its task last changed state");
        assert_eq!(node("bg")["state"], json!("idle"), "a claude --bg node still reads the deduction");
        assert_eq!(nodes.len(), 3);
    }
}

#[cfg(test)]
mod poison_tests {
    use super::*;
    use crate::locks::{assert_exited_for, child_home, in_child};

    #[test]
    #[ignore = "run by a_read_after_a_panic_holding_the_live_view_exits_the_daemon"]
    fn view_child() {
        if child_home().is_none() { return; }
        let live = Live::default();
        crate::locks::poison(&live.view);
        let _ = live.nodes();
        unreachable!("the live view was read while poisoned");
    }

    #[test]
    fn a_read_after_a_panic_holding_the_live_view_exits_the_daemon() {
        let home = crate::test_support::Directory::new();
        assert_exited_for(&in_child("live::poison_tests::view_child", &home), "the live sessions");
    }

    #[test]
    fn presence_goes_on_after_a_panic_holding_it() {
        let live = Live::default();
        crate::locks::poison(&live.presence);
        live.touch("phone");
        assert!(live.last_touched("phone").is_some());
    }
}

crate::extension!(Live, |runtime: &dyn alondra_engine_ports::Runtime| Live::persisted_in(runtime.home()));

pub trait LiveOf {
    fn live(&self) -> &Live;

    /// A node mod connected.
    fn connect_node(&self, hello: crate::node_reports::Hello, at: alondra_core::Millis) -> u64;

    /// A node mod's report: what it says is kept for the views.
    fn report(&self, envelope: crate::node_reports::Envelope, at: alondra_core::Millis);

    /// Whether Alondra launched this session: a recorded launch, or a scheduled run it started.
    fn launched_by_alondra(&self, agent: &alondra_domain_rules::AgentId) -> anyhow::Result<bool>;

    /// What Alondra kept about a session only while it lives: the worktree it inherited and why
    /// its replacement stopped short.
    fn forget_ended_session(&self, agent: &str) -> anyhow::Result<()>;
}

impl<R: alondra_engine_ports::Runtime + ?Sized> LiveOf for R {
    fn live(&self) -> &Live {
        alondra_engine_ports::extension(self.as_runtime())
    }

    fn connect_node(&self, hello: crate::node_reports::Hello, at: alondra_core::Millis) -> u64 {
        self.live().mods.connect(hello, at)
    }

    fn report(&self, envelope: crate::node_reports::Envelope, at: alondra_core::Millis) {
        if let crate::node_reports::Report::PromptSubmit { origin } = envelope.report {
            self.live().observe_prompt(&envelope.session_id, origin);
        }
        self.live().mods.apply(envelope, at);
    }

    fn launched_by_alondra(&self, agent: &alondra_domain_rules::AgentId) -> anyhow::Result<bool> {
        if self.store().lock_or_exit("the domain store").launched_by_alondra(&agent.0)? || self.live().mods.started_with_a_launch_key(&agent.0) {
            return Ok(true);
        }
        Ok(self.state().schedule.runs.values().any(|run| run.launch.as_ref().is_some_and(|launch| launch.agent_id.as_ref() == Some(agent))))
    }

    fn forget_ended_session(&self, agent: &str) -> anyhow::Result<()> {
        self.live().take_aborted_replace(agent);
        self.forget_inherited_worktree(agent)
    }
}
