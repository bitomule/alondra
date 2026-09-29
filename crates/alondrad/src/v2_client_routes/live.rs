//! What the daemon observes of each Claude session through its hooks. The domain never
//! records it: it is the live state a client shows next to the projection, kept in memory
//! with the legacy reducer so a node keeps the shape every client already reads.
use alondra_core::{Event, HookEvent, Node, View, now_millis};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};

#[derive(Default)]
pub(crate) struct Live {
    view: Mutex<View>,
    sequence: AtomicU64,
    presence: Mutex<HashMap<String, i64>>,
}

impl Live {
    pub(crate) fn observe(&self, payload: &Value) {
        let Some(event) = hook_event(payload) else { return };
        let session_id = event.session_id.clone();
        let mut view = self.view.lock().expect("live sessions poisoned");
        let now = now_millis();
        if !view.nodes.contains_key(&session_id) {
            view.apply(self.next(), now, &Event::Joined { session_id });
        }
        view.apply(self.next(), now, &Event::Hook(event));
    }

    fn next(&self) -> u64 {
        self.sequence.fetch_add(1, Ordering::Relaxed) + 1
    }

    pub(crate) fn touch(&self, client: &str) {
        self.presence.lock().expect("presence poisoned").insert(client.to_owned(), now_millis());
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
