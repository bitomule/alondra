//! What the daemon observes of each Claude session through its hooks and its node mod. The
//! domain never records it: it is the live state a client shows next to the projection, kept in
//! memory with the legacy reducer so a node keeps the shape every client already reads.
use crate::locks::Poisonable as _;
use alondra_core::{Event, HookEvent, Node, View, now_millis};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::sync::{Mutex, atomic::{AtomicU64, Ordering}};

const CLOSED_FILE: &str = "closed-sessions.json";
const CLOSED_TASKS_FILE: &str = "closed-task-nodes.json";
const ABORTED_REPLACES_FILE: &str = "aborted-replaces.json";

/// What a relaunch needs of the node the user closed while it worked on a task.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub(crate) struct ClosedNode {
    pub name: String,
    pub cwd: Option<String>,
}

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
pub(crate) struct Live {
    /// A holder that panics can leave it half-changed, so the daemon exits: `View::apply` changes
    /// several maps in a row, and the hooks rebuild the whole view after a fresh start.
    view: Mutex<View>,
    sequence: AtomicU64,
    pub(crate) changed: crate::node_reports::LiveChanges,
    /// A holder that panics leaves it whole, so it is recovered: one insert.
    presence: Mutex<HashMap<String, i64>>,
    pub(crate) mods: crate::node_reports::Reports,
    pub(crate) pushes: crate::node_push::Pushes,
    /// A sandbox daemon registers the Agent of any session whose mod connects, so a hand-launched
    /// probe node can be sent to.
    pub(crate) sandbox: std::sync::atomic::AtomicBool,
    pub(crate) losses: crate::node_revival::Losses,
    pub(crate) usage_pause: crate::usage_pause::UsagePause,
    pub(crate) account_wait: crate::accounts::AccountWait,
    /// Committed orchestrator work only; unrelated domain activity cannot hide a stuck batch.
    pub(crate) orchestrator_progress: AtomicU64,
    /// Recovered after panic: each holder only reads or replaces one complete host lease.
    pub(crate) orchestrator_host: Mutex<Option<(String, alondra_agent_runtime::owned_host::HostStop)>>,
    /// Retain a logical stop while storage is unavailable, until its finalization commits.
    /// Recovered after panic: one complete stop is assigned, cloned or cleared.
    pub(crate) orchestrator_stop: Mutex<Option<(crate::domain_orchestrator::Run, alondra_core::orchestrator_batch::BatchProblem)>>,
    /// Recovered after panic: each holder inserts or removes one completed-run marker.
    pub(crate) orchestrator_runs: Mutex<std::collections::BTreeSet<String>>,
    /// Asks the accounts check to run now.
    pub(crate) account_check: tokio::sync::Notify,
    pub(crate) initial_account_check_pending: std::sync::atomic::AtomicBool,
    pub(crate) measured: crate::usage_probe::Measured,
    pub(crate) api_watch: crate::api_failures::ApiWatch,
    pub(crate) machine_watch: crate::machine_watch::MachineWatch,
    pub(crate) gate: crate::admission::Admission,
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
    closed_tasks: Mutex<std::collections::BTreeMap<String, ClosedNode>>,
    closed_tasks_file: Option<std::path::PathBuf>,
    /// A holder that panics leaves it whole, so it is recovered: as `closed`.
    aborted_replaces: Mutex<std::collections::BTreeMap<String, String>>,
    aborted_replaces_file: Option<std::path::PathBuf>,
}

impl Live {
    pub(crate) fn persisted_in(home: &std::path::Path) -> Self {
        let file = home.join(CLOSED_FILE);
        let closed = std::fs::read(&file).ok().and_then(|bytes| serde_json::from_slice(&bytes).ok()).unwrap_or_default();
        let closed_tasks_file = home.join(CLOSED_TASKS_FILE);
        let closed_tasks = std::fs::read(&closed_tasks_file).ok().and_then(|bytes| serde_json::from_slice(&bytes).ok()).unwrap_or_default();
        let aborted_replaces_file = home.join(ABORTED_REPLACES_FILE);
        let aborted_replaces = std::fs::read(&aborted_replaces_file).ok().and_then(|bytes| serde_json::from_slice(&bytes).ok()).unwrap_or_default();
        Self {
            aborted_replaces: Mutex::new(aborted_replaces),
            aborted_replaces_file: Some(aborted_replaces_file),
            closed_tasks: Mutex::new(closed_tasks),
            closed_tasks_file: Some(closed_tasks_file),
            usage_pause: crate::usage_pause::UsagePause::default(),
            machine_watch: crate::machine_watch::MachineWatch::persisted_in(home),
            gate: crate::admission::Admission::persisted_in(home),
            closed: Mutex::new(closed),
            closed_file: Some(file),
            ..Self::default()
        }
    }

    /// The user closed the session: it is not brought back and closes, also after a restart.
    pub(crate) fn mark_closed(&self, session_id: &str) {
        let mut closed = self.closed.lock_or_recover("the closed sessions");
        closed.insert(session_id.to_owned());
        if let Some(file) = &self.closed_file {
            save(file, &*closed);
        }
        drop(closed);
        self.changed.publish();
    }

    /// The task the user's closed node worked on waits for a relaunch, which needs this.
    pub(crate) fn remember_closed_task(&self, task_id: &str, node: ClosedNode) {
        let mut tasks = self.closed_tasks.lock_or_recover("the closed sessions");
        tasks.insert(task_id.to_owned(), node);
        if let Some(file) = &self.closed_tasks_file {
            save(file, &*tasks);
        }
        drop(tasks);
        self.changed.publish();
    }

    /// The orchestrator's replacement of this session's node stopped short because the node could
    /// not be stopped: if it ends later, that is why, not the user closing it.
    pub(crate) fn note_aborted_replace(&self, session_id: &str, why: &str) {
        let mut aborted = self.aborted_replaces.lock_or_recover("the closed sessions");
        aborted.insert(session_id.to_owned(), why.to_owned());
        if let Some(file) = &self.aborted_replaces_file {
            save(file, &*aborted);
        }
    }

    pub(crate) fn take_aborted_replace(&self, session_id: &str) -> Option<String> {
        let mut aborted = self.aborted_replaces.lock_or_recover("the closed sessions");
        let why = aborted.remove(session_id)?;
        if let Some(file) = &self.aborted_replaces_file {
            save(file, &*aborted);
        }
        Some(why)
    }

    pub(crate) fn closed_task(&self, task_id: &str) -> Option<ClosedNode> {
        self.closed_tasks.lock_or_recover("the closed sessions").get(task_id).cloned()
    }

    pub(crate) fn forget_closed_task(&self, task_id: &str) {
        let mut tasks = self.closed_tasks.lock_or_recover("the closed sessions");
        if tasks.remove(task_id).is_none() { return; }
        if let Some(file) = &self.closed_tasks_file {
            save(file, &*tasks);
        }
        drop(tasks);
        self.changed.publish();
    }

    pub(crate) fn closed_task_ids(&self) -> Vec<String> {
        self.closed_tasks.lock_or_recover("the closed sessions").keys().cloned().collect()
    }

    pub(crate) fn closed_sessions(&self) -> Vec<String> {
        self.closed.lock_or_recover("the closed sessions").iter().cloned().collect()
    }

    pub(crate) fn is_closed(&self, session_id: &str) -> bool {
        self.closed.lock_or_recover("the closed sessions").contains(session_id)
    }

    /// False when something is watching the session's connection back already.
    /// Takes the right to launch a node for a task; false while another launch of it runs.
    pub(crate) fn begin_launch(&self, task_id: &str) -> bool {
        self.launching.send_if_modified(|launching| launching.insert(task_id.to_owned()))
    }

    pub(crate) fn is_launching(&self, task_id: &str) -> bool {
        self.launching.borrow().contains(task_id)
    }

    pub(crate) fn end_launch(&self, task_id: &str) {
        self.launching.send_if_modified(|launching| launching.remove(task_id));
    }

    #[cfg(test)]
    pub(crate) async fn until_launch_ends(&self, task_id: &str) {
        let mut launching = self.launching.subscribe();
        launching.wait_for(|launching| !launching.contains(task_id)).await.expect("Live owns the sender");
    }

    pub(crate) fn begin_watch(&self, session_id: &str) -> bool {
        self.watching.lock_or_recover("the closed sessions").insert(session_id.to_owned())
    }

    pub(crate) fn end_watch(&self, session_id: &str) {
        self.watching.lock_or_recover("the closed sessions").remove(session_id);
    }

    /// When Alondra last cut the session's turn to close it, if it did.
    pub(crate) fn close_interrupted_at(&self, session_id: &str) -> Option<i64> {
        self.close_interrupts.lock_or_recover("the close interrupts").get(session_id).copied()
    }

    pub(crate) fn set_close_interrupted(&self, session_id: &str, at: Option<i64>) {
        let mut interrupts = self.close_interrupts.lock_or_recover("the close interrupts");
        match at {
            Some(at) => interrupts.insert(session_id.to_owned(), at),
            None => interrupts.remove(session_id),
        };
    }

    /// A hook a session sent. What a node of the Agent SDK does is its task's lifecycle, so its
    /// hooks never feed the deduction kept for `claude --bg` nodes.
    pub(crate) fn observe_hook(&self, state: &alondra_core::domain::DomainState, payload: &Value) {
        let session = payload.get("session_id").and_then(Value::as_str).unwrap_or_default();
        if !through_sdk(state, &alondra_core::domain::AgentId(session.into())) {
            self.observe(payload);
        }
    }

    pub(crate) fn observe(&self, payload: &Value) {
        let Some(event) = hook_event(payload) else { return };
        let session_id = event.session_id.clone();
        let mut view = self.view.lock_or_exit("the live sessions");
        let now = now_millis();
        if !view.nodes.contains_key(&session_id) {
            view.apply(self.next(), now, &Event::Joined { session_id });
        }
        let limit = (event.event == "StopFailure" && event.error_type.as_deref() == Some("rate_limit")).then(|| crate::hook_text::str_field(payload, "error_details"));
        if event.event == "StopFailure" && event.error_type.as_deref() == Some("authentication_failed") {
            self.account_check.notify_one();
        }
        view.apply(self.next(), now, &Event::Hook(event));
        drop(view);
        if let Some(details) = limit {
            let until = details.as_deref().and_then(|text| crate::usage_pause::reset_in(text, now)).or_else(|| self.measured.reset_after_limit(&alondra_core::accounts::AccountId::default_account(), now));
            self.usage_pause.exhaust(until, now);
        }
        self.changed.publish();
    }

    fn next(&self) -> u64 {
        self.sequence.fetch_add(1, Ordering::Relaxed) + 1
    }

    pub(crate) fn touch(&self, client: &str) {
        self.presence.lock_or_recover("the client presence").insert(client.to_owned(), now_millis());
    }

    #[cfg(test)]
    pub(crate) fn last_touched(&self, client: &str) -> Option<i64> {
        self.presence.lock_or_recover("the client presence").get(client).copied()
    }

    pub(crate) fn apply(&self, event: &Event) {
        self.view.lock_or_exit("the live sessions").apply(self.next(), now_millis(), event);
        self.changed.publish();
    }

    pub(crate) fn node(&self, session_id: &str) -> Option<Node> {
        self.view.lock_or_exit("the live sessions").nodes.get(session_id).cloned()
    }

    pub(crate) fn nodes(&self) -> Vec<Node> {
        self.view.lock_or_exit("the live sessions").nodes.values().cloned().collect()
    }

    pub(crate) fn threads(&self, session_id: &str) -> Option<Vec<alondra_core::activity::Activity>> {
        if let Some(activity) = self.mods.session(session_id).as_ref().and_then(|session| crate::codex_live::activity(session).or_else(|| crate::sdk_live::activity(session))) {
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
pub(crate) fn runtime_nodes(deduced: Vec<Node>, state: &alondra_core::domain::DomainState, since: impl Fn(&alondra_core::domain::TaskId) -> Option<i64>) -> Vec<Value> {
    use alondra_core::domain::AgentId;
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

fn from_lifecycle(node: &mut Node, state: &alondra_core::domain::DomainState) {
    use alondra_core::{NodeState, NodeStatus, StateSource};
    use alondra_domain::LiveState;
    let agent = alondra_core::domain::AgentId(node.session_id.clone());
    let result = state.node_task(&agent).and_then(|task| task.final_presentation.as_ref());
    node.state = match state.sdk_live(&agent) {
        Some(LiveState::Working(_)) => NodeState::Working,
        Some(LiveState::WaitingUser(cards)) if cards.iter().all(|card| Some(card) == result) => NodeState::WaitingMerge,
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

fn through_sdk(state: &alondra_core::domain::DomainState, agent: &alondra_core::domain::AgentId) -> bool {
    state.through_sdk(agent) || state.node_phase(agent) == Some(&alondra_core::domain::NodePhase::ThroughSdk)
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
mod sdk_node_tests {
    use super::*;
    use alondra_core::domain::{Actor, Command, CommandEnvelope, DomainState, LaunchPlan, PROTOCOL_VERSION};
    use std::collections::BTreeSet;

    fn apply(state: &mut DomainState, actor: Actor, command: Command) {
        let envelope = CommandEnvelope { protocol_version: PROTOCOL_VERSION, data_epoch: state.data_epoch.clone(),
            command_id: format!("c-{}", state.revision).into(), expected_revision: state.revision, command };
        state.execute(&actor, &envelope).unwrap();
    }

    /// Agents `sdk-seen` and `sdk-unseen` run through the Agent SDK, each on its own task being
    /// started; `bg` is a node of `claude --bg`.
    fn world() -> DomainState {
        let mut state = DomainState::new("epoch");
        for agent in ["sdk-seen", "sdk-unseen", "bg"] {
            apply(&mut state, Actor::System, Command::RegisterAgent { agent_id: agent.into(), name: agent.into(), at: 0 });
        }
        for agent in ["sdk-seen", "sdk-unseen"] {
            let (request, task) = (format!("r-{agent}"), format!("t-{agent}"));
            apply(&mut state, Actor::User { user_id: "u".into() }, Command::CreateRequest { request_id: request.clone().into(), body: "Notes".into(), auto_merge: false, maintenance: false, message: false });
            apply(&mut state, Actor::Orchestrator { generation: 0 }, Command::CreateTask { plan_items: None, task_id: task.clone().into(), request_id: request.into(), title: "Notes".into(),
                agent_id: None, review_prs: false, dependencies: BTreeSet::new(), stack: None, launch: Some(LaunchPlan::new("node", "/tmp")) });
            apply(&mut state, Actor::System, Command::AdmitExecution { provider: alondra_core::accounts::Provider::Claude, task_id: task.clone().into(), agent_id: agent.into(),
                conversation: format!("conversation-{task}"), instance: format!("instance-{task}"), host: crate::sdk_owner::HOST.into(), model: None, account: None, leaves_limit: None });
        }
        state
    }

    #[test]
    fn a_hook_of_a_node_of_the_agent_sdk_never_reaches_the_deduction_of_claude_bg_nodes() {
        let state = world();
        let live = Live::default();
        for agent in ["sdk-seen", "bg"] {
            live.observe_hook(&state, &json!({"session_id": agent, "hook_event_name": "Stop", "last_assistant_message": "Ready"}));
        }
        assert!(live.node("sdk-seen").is_none(), "{:?}", live.node("sdk-seen"));
        assert!(live.node("bg").is_some(), "a claude --bg node is still deduced from its hooks");
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
        assert_exited_for(&in_child("client_routes::live::poison_tests::view_child", &home), "the live sessions");
    }

    #[test]
    fn presence_goes_on_after_a_panic_holding_it() {
        let live = Live::default();
        crate::locks::poison(&live.presence);
        live.touch("phone");
        assert!(live.last_touched("phone").is_some());
    }
}
