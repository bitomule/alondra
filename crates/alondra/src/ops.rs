//! Requests the user makes of a session or of the daemon that are not domain commands: a
//! message to an agent, closing a session, the orchestrator's recovery and the like. Each is
//! one resource route; the TUI and the CLI build them here so both send the same body.

use alondra_core::domain::{AgentId, AttachmentRef, InteractionId, PrId, TaskId};
use serde_json::{Value, json};

use crate::mcp::urlencode;

/// Why a message goes to an agent: the daemon words it for the agent from this.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum MessageKind {
    Message,
    Reply,
    MoreContext,
    PrChanges,
    PrQuestion,
}

impl MessageKind {
    fn wire(self) -> &'static str {
        match self {
            MessageKind::Message => "message",
            MessageKind::Reply => "reply",
            MessageKind::MoreContext => "more_context",
            MessageKind::PrChanges => "pr_changes",
            MessageKind::PrQuestion => "pr_question",
        }
    }
}

/// A message from the user; `agent: None` is the orchestrator.
#[derive(Clone, Debug, PartialEq)]
pub struct Message {
    pub agent: Option<AgentId>,
    pub kind: MessageKind,
    pub text: String,
    pub attachments: Vec<AttachmentRef>,
    pub interaction: Option<InteractionId>,
    pub pr: Option<PrId>,
    /// The Task the message is about, which must be the agent's.
    pub task: Option<TaskId>,
    /// Marks `interaction` read in the same step, for a reply to information.
    pub mark_read: bool,
}

impl Message {
    pub fn to(agent: Option<AgentId>, kind: MessageKind, text: String) -> Self {
        Message { agent, kind, text, attachments: vec![], interaction: None, pr: None, task: None, mark_read: false }
    }

    pub fn path(&self) -> String {
        match &self.agent {
            Some(a) => format!("/agents/{}/messages", urlencode(&a.0)),
            None => "/orchestrator/messages".into(),
        }
    }

    pub fn body(&self) -> Value {
        let mut body = json!({"text": self.text, "attachments": self.attachments});
        if self.agent.is_some() {
            body["kind"] = json!(self.kind.wire());
            body["mark_read"] = json!(self.mark_read);
            if let Some(i) = &self.interaction {
                body["interaction_id"] = json!(i);
            }
            if let Some(p) = &self.pr {
                body["pr_id"] = json!(p);
            }
            if let Some(t) = &self.task {
                body["task_id"] = json!(t);
            }
        }
        body
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Recovery {
    Cancel,
    Acknowledge,
    Retry,
}

pub fn orchestrator(action: Recovery, run_id: &str) -> (String, Value) {
    let step = match action {
        Recovery::Cancel => "cancel",
        Recovery::Acknowledge => "acknowledge",
        Recovery::Retry => "retry",
    };
    (format!("/orchestrator/{step}"), json!({"run_id": run_id}))
}

pub fn cancel_reason(task: &TaskId, reason: &str) -> (String, Value) {
    (format!("/tasks/{}/cancel-reason", urlencode(&task.0)), json!({"reason": reason}))
}

pub const PRESENCE: &str = "/activity";
pub const REQUESTS: &str = "/requests";

/// A new request carrying what the plain `create_request` command cannot: its project, launch
/// options, issue and attachments. The daemon creates the Request and keeps its launch policy
/// in one step.
pub fn new_request(request_id: &str, text: &str, project: Option<&str>, options: &Value, issue: Option<&str>, attachments: &[AttachmentRef]) -> Value {
    let mut body = json!({"request_id": request_id, "text": text, "project": project, "options": options, "attachments": attachments});
    if let Some(issue) = issue {
        body["issue"] = json!(issue);
    }
    body
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_message_names_its_agent_and_why_and_the_orchestrator_takes_only_text() {
        let mut m = Message::to(Some("session/1".into()), MessageKind::Reply, "Thanks".into());
        m.interaction = Some("info-1".into());
        m.mark_read = true;
        assert_eq!(m.path(), "/agents/session%2F1/messages");
        assert_eq!(m.body(), json!({"text": "Thanks", "attachments": [], "kind": "reply", "mark_read": true, "interaction_id": "info-1"}));
        let o = Message::to(None, MessageKind::Message, "Plan the week".into());
        assert_eq!((o.path(), o.body()), ("/orchestrator/messages".to_string(), json!({"text": "Plan the week", "attachments": []})));
    }

    #[test]
    fn operational_routes_use_unversioned_resource_paths() {
        assert_eq!(Message::to(Some("agent-1".into()), MessageKind::Message, "x".into()).path(), "/agents/agent-1/messages");
        assert_eq!(orchestrator(Recovery::Cancel, "run-1").0, "/orchestrator/cancel");
        assert_eq!(orchestrator(Recovery::Acknowledge, "run-1").0, "/orchestrator/acknowledge");
        assert_eq!(orchestrator(Recovery::Retry, "run-1").0, "/orchestrator/retry");
        assert_eq!(cancel_reason(&"task-1".into(), "done").0, "/tasks/task-1/cancel-reason");
        assert_eq!(PRESENCE, "/activity");
    }

    #[test]
    fn every_request_matches_the_fixture_the_daemon_implements() {
        let contract = crate::runtime::fixture("requests.json");
        let a = AttachmentRef { id: "att-1".into(), name: "shot.png".into(), content_digest: "d".repeat(64), content_reference: "files/domain-v2/x".into(), bytes: 3 };
        let mut pr = Message::to(Some("agent-a".into()), MessageKind::PrChanges, "Rename it".into());
        pr.pr = Some("pr-1".into());
        pr.attachments = vec![a.clone()];
        let built = [
            ("message", (pr.path(), pr.body())),
            ("orchestrator_retry", orchestrator(Recovery::Retry, "run-7")),
            ("cancel_reason", cancel_reason(&"task-1".into(), "Superseded")),
            ("new_request", (REQUESTS.to_string(), new_request("request-9", "Ship it", Some("App"), &json!({"effort": "hard"}), Some("me/App#3"), &[a]))),
        ];
        for (name, (path, body)) in built {
            assert_eq!(contract[name]["path"], json!(path), "{name}");
            assert_eq!(contract[name]["body"], body, "{name}");
        }
    }
}
