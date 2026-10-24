//! What the daemon observes of each Claude session through its hooks and its node mod. The
//! domain never records it: it is the live state a client shows next to the projection, kept in
//! memory with the legacy reducer so a node keeps the shape every client already reads.
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
    view: Mutex<View>,
    sequence: AtomicU64,
    pub(crate) changed: crate::node_reports::LiveChanges,
    presence: Mutex<HashMap<String, i64>>,
    pub(crate) mods: crate::node_reports::Reports,
    pub(crate) pushes: crate::node_push::Pushes,
    /// A sandbox daemon registers the Agent of any session whose mod connects, so a hand-launched
    /// probe node can be sent to.
    pub(crate) sandbox: std::sync::atomic::AtomicBool,
    pub(crate) losses: crate::node_revival::Losses,
    pub(crate) usage_pause: crate::usage_pause::UsagePause,
    pub(crate) account_wait: crate::accounts::AccountWait,
    /// Asks the accounts check to run now.
    pub(crate) account_check: tokio::sync::Notify,
    pub(crate) measured: crate::usage_probe::Measured,
    pub(crate) api_watch: crate::api_failures::ApiWatch,
    pub(crate) machine_watch: crate::machine_watch::MachineWatch,
    pub(crate) gate: crate::admission::Admission,
    watching: Mutex<std::collections::HashSet<String>>,
    launching: tokio::sync::watch::Sender<std::collections::HashSet<String>>,
    /// When Alondra last cut the turn of a session it has to close, by session.
    close_interrupts: Mutex<HashMap<String, i64>>,
    closed: Mutex<std::collections::BTreeSet<String>>,
    closed_file: Option<std::path::PathBuf>,
    closed_tasks: Mutex<std::collections::BTreeMap<String, ClosedNode>>,
    closed_tasks_file: Option<std::path::PathBuf>,
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
        let mut closed = self.closed.lock().expect("revivals poisoned");
        closed.insert(session_id.to_owned());
        if let Some(file) = &self.closed_file {
            save(file, &*closed);
        }
        drop(closed);
        self.changed.publish();
    }

    /// The task the user's closed node worked on waits for a relaunch, which needs this.
    pub(crate) fn remember_closed_task(&self, task_id: &str, node: ClosedNode) {
        let mut tasks = self.closed_tasks.lock().expect("revivals poisoned");
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
        let mut aborted = self.aborted_replaces.lock().expect("revivals poisoned");
        aborted.insert(session_id.to_owned(), why.to_owned());
        if let Some(file) = &self.aborted_replaces_file {
            save(file, &*aborted);
        }
    }

    pub(crate) fn take_aborted_replace(&self, session_id: &str) -> Option<String> {
        let mut aborted = self.aborted_replaces.lock().expect("revivals poisoned");
        let why = aborted.remove(session_id)?;
        if let Some(file) = &self.aborted_replaces_file {
            save(file, &*aborted);
        }
        Some(why)
    }

    pub(crate) fn closed_task(&self, task_id: &str) -> Option<ClosedNode> {
        self.closed_tasks.lock().expect("revivals poisoned").get(task_id).cloned()
    }

    pub(crate) fn forget_closed_task(&self, task_id: &str) {
        let mut tasks = self.closed_tasks.lock().expect("revivals poisoned");
        if tasks.remove(task_id).is_none() { return; }
        if let Some(file) = &self.closed_tasks_file {
            save(file, &*tasks);
        }
        drop(tasks);
        self.changed.publish();
    }

    pub(crate) fn closed_task_ids(&self) -> Vec<String> {
        self.closed_tasks.lock().expect("revivals poisoned").keys().cloned().collect()
    }

    pub(crate) fn closed_sessions(&self) -> Vec<String> {
        self.closed.lock().expect("revivals poisoned").iter().cloned().collect()
    }

    pub(crate) fn is_closed(&self, session_id: &str) -> bool {
        self.closed.lock().expect("revivals poisoned").contains(session_id)
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
        self.watching.lock().expect("revivals poisoned").insert(session_id.to_owned())
    }

    pub(crate) fn end_watch(&self, session_id: &str) {
        self.watching.lock().expect("revivals poisoned").remove(session_id);
    }

    /// When Alondra last cut the session's turn to close it, if it did.
    pub(crate) fn close_interrupted_at(&self, session_id: &str) -> Option<i64> {
        self.close_interrupts.lock().expect("close interrupts poisoned").get(session_id).copied()
    }

    pub(crate) fn set_close_interrupted(&self, session_id: &str, at: Option<i64>) {
        let mut interrupts = self.close_interrupts.lock().expect("close interrupts poisoned");
        match at {
            Some(at) => interrupts.insert(session_id.to_owned(), at),
            None => interrupts.remove(session_id),
        };
    }

    pub(crate) fn observe(&self, payload: &Value) {
        let Some(event) = hook_event(payload) else { return };
        let session_id = event.session_id.clone();
        let mut view = self.view.lock().expect("live sessions poisoned");
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
        self.presence.lock().expect("presence poisoned").insert(client.to_owned(), now_millis());
    }

    #[cfg(test)]
    pub(crate) fn last_touched(&self, client: &str) -> Option<i64> {
        self.presence.lock().expect("presence poisoned").get(client).copied()
    }

    pub(crate) fn apply(&self, event: &Event) {
        self.view.lock().expect("live sessions poisoned").apply(self.next(), now_millis(), event);
        self.changed.publish();
    }

    pub(crate) fn node(&self, session_id: &str) -> Option<Node> {
        self.view.lock().expect("live sessions poisoned").nodes.get(session_id).cloned()
    }

    pub(crate) fn nodes(&self) -> Vec<Node> {
        self.view.lock().expect("live sessions poisoned").nodes.values().cloned().collect()
    }

    pub(crate) fn threads(&self, session_id: &str) -> Option<Vec<alondra_core::activity::Activity>> {
        if let Some(activity) = self.mods.session(session_id).as_ref().and_then(crate::sdk_live::activity) {
            return Some(activity);
        }
        let view = self.view.lock().expect("live sessions poisoned");
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
