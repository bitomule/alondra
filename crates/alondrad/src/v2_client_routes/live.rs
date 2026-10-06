//! What the daemon observes of each Claude session through its hooks and its node mod. The
//! domain never records it: it is the live state a client shows next to the projection, kept in
//! memory with the legacy reducer so a node keeps the shape every client already reads.
use alondra_core::{Event, HookEvent, Node, View, now_millis};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::sync::{Mutex, atomic::{AtomicU64, Ordering}};

const CLOSED_FILE: &str = "closed-sessions.json";
const CLOSED_TASKS_FILE: &str = "closed-task-nodes.json";

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
    presence: Mutex<HashMap<String, i64>>,
    pub(crate) mods: crate::node_reports::Reports,
    pub(crate) pushes: crate::node_push::Pushes,
    /// A sandbox daemon registers the Agent of any session whose mod connects, so a hand-launched
    /// probe node can be sent to.
    pub(crate) sandbox: std::sync::atomic::AtomicBool,
    pub(crate) losses: crate::node_revival::Losses,
    pub(crate) usage_pause: crate::usage_pause::UsagePause,
    watching: Mutex<std::collections::HashSet<String>>,
    reconnecting: Mutex<std::collections::HashSet<String>>,
    closed: Mutex<std::collections::BTreeSet<String>>,
    closed_file: Option<std::path::PathBuf>,
    closed_tasks: Mutex<std::collections::BTreeMap<String, ClosedNode>>,
    closed_tasks_file: Option<std::path::PathBuf>,
}

impl Live {
    pub(crate) fn persisted_in(home: &std::path::Path) -> Self {
        let file = home.join(CLOSED_FILE);
        let closed = std::fs::read(&file).ok().and_then(|bytes| serde_json::from_slice(&bytes).ok()).unwrap_or_default();
        let closed_tasks_file = home.join(CLOSED_TASKS_FILE);
        let closed_tasks = std::fs::read(&closed_tasks_file).ok().and_then(|bytes| serde_json::from_slice(&bytes).ok()).unwrap_or_default();
        Self {
            closed_tasks: Mutex::new(closed_tasks),
            closed_tasks_file: Some(closed_tasks_file),
            mods: crate::node_reports::Reports::persisted_in(home),
            usage_pause: crate::usage_pause::UsagePause::persisted_in(home),
            closed: Mutex::new(closed),
            closed_file: Some(file),
            ..Self::default()
        }
    }

    /// The user closed the session: it is not brought back and leaves Fleet, also after a restart.
    pub(crate) fn mark_closed(&self, session_id: &str) {
        let mut closed = self.closed.lock().expect("revivals poisoned");
        closed.insert(session_id.to_owned());
        if let Some(file) = &self.closed_file {
            save(file, &*closed);
        }
    }

    /// The task the user's closed node worked on waits for a relaunch, which needs this.
    pub(crate) fn remember_closed_task(&self, task_id: &str, node: ClosedNode) {
        let mut tasks = self.closed_tasks.lock().expect("revivals poisoned");
        tasks.insert(task_id.to_owned(), node);
        if let Some(file) = &self.closed_tasks_file {
            save(file, &*tasks);
        }
    }

    pub(crate) fn closed_task(&self, task_id: &str) -> Option<ClosedNode> {
        self.closed_tasks.lock().expect("revivals poisoned").get(task_id).cloned()
    }

    pub(crate) fn forget_closed_task(&self, task_id: &str) {
        let mut tasks = self.closed_tasks.lock().expect("revivals poisoned");
        if tasks.remove(task_id).is_some()
            && let Some(file) = &self.closed_tasks_file
        {
            save(file, &*tasks);
        }
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
    pub(crate) fn begin_watch(&self, session_id: &str) -> bool {
        self.watching.lock().expect("revivals poisoned").insert(session_id.to_owned())
    }

    pub(crate) fn end_watch(&self, session_id: &str) {
        self.watching.lock().expect("revivals poisoned").remove(session_id);
    }

    /// A restart of the session is running.
    pub(crate) fn set_reconnecting(&self, session_id: &str, on: bool) {
        let mut sessions = self.reconnecting.lock().expect("revivals poisoned");
        if on { sessions.insert(session_id.to_owned()); } else { sessions.remove(session_id); }
    }

    pub(crate) fn is_reconnecting(&self, session_id: &str) -> bool {
        self.reconnecting.lock().expect("revivals poisoned").contains(session_id)
    }

    pub(crate) fn observe(&self, payload: &Value) {
        let Some(event) = hook_event(payload) else { return };
        let session_id = event.session_id.clone();
        let mut view = self.view.lock().expect("live sessions poisoned");
        let now = now_millis();
        if !view.nodes.contains_key(&session_id) {
            view.apply(self.next(), now, &Event::Joined { session_id });
        }
        let limit = (event.event == "StopFailure" && event.error_type.as_deref() == Some("rate_limit")).then(|| crate::http::str_field(payload, "error_details"));
        view.apply(self.next(), now, &Event::Hook(event));
        drop(view);
        if let Some(details) = limit {
            let until = details.as_deref().and_then(|text| crate::usage_pause::reset_in(text, now)).or_else(|| self.mods.reset_after_limit(now));
            self.usage_pause.exhaust(until, now);
        }
    }

    /// A node's measurement that shows a plan window used up is the limit too.
    pub(crate) fn note_measures(&self) {
        let now = now_millis();
        if let Some(until) = self.mods.exhausted_until(now) {
            self.usage_pause.exhaust(Some(until), now);
        }
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
    }

    pub(crate) fn node(&self, session_id: &str) -> Option<Node> {
        self.view.lock().expect("live sessions poisoned").nodes.get(session_id).cloned()
    }

    pub(crate) fn nodes(&self) -> Vec<Node> {
        self.view.lock().expect("live sessions poisoned").nodes.values().cloned().collect()
    }

    pub(crate) fn threads(&self, session_id: &str) -> Option<Vec<alondra_core::activity::Activity>> {
        let view = self.view.lock().expect("live sessions poisoned");
        let node = view.nodes.get(session_id)?;
        Some(view.activity(node, now_millis()))
    }
}

fn hook_event(payload: &Value) -> Option<HookEvent> {
    use crate::http::{hook_detail, named_task, str_field, tool_summary};
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
