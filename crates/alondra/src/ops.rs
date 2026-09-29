//! Requests the user makes of a session or of the daemon that are not domain commands: a
//! message to an agent, closing a session, the orchestrator's recovery and the like. Each is
//! one route of the v2 daemon; the TUI and the CLI build them here so both send the same body.

use alondra_core::domain_v2::{AgentId, AttachmentRef, InteractionId, PrId, TaskId};
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
    /// Marks `interaction` read in the same step, for a reply to information.
    pub mark_read: bool,
}

impl Message {
    pub fn to(agent: Option<AgentId>, kind: MessageKind, text: String) -> Self {
        Message { agent, kind, text, attachments: vec![], interaction: None, pr: None, mark_read: false }
    }

    pub fn path(&self) -> String {
        match &self.agent {
            Some(a) => format!("/v2/agents/{}/messages", urlencode(&a.0)),
            None => "/v2/orchestrator/messages".into(),
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
        }
        body
    }
}

pub fn close(agent: &AgentId, force: bool) -> (String, Value) {
    (format!("/v2/agents/{}/close", urlencode(&agent.0)), json!({"force": force}))
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
    (format!("/v2/orchestrator/{step}"), json!({"run_id": run_id}))
}

pub fn resolve_conflicts(pr: &PrId) -> (String, Value) {
    (format!("/v2/pull-requests/{}/resolve-conflicts", urlencode(&pr.0)), json!({}))
}

pub fn set_aside(pr: &PrId) -> (String, Value) {
    (format!("/v2/pull-requests/{}/set-aside", urlencode(&pr.0)), json!({}))
}

pub fn cancel_reason(task: &TaskId, reason: &str) -> (String, Value) {
    (format!("/v2/tasks/{}/cancel-reason", urlencode(&task.0)), json!({"reason": reason}))
}

pub fn history(task: &TaskId) -> String {
    format!("/v2/tasks/{}/history", urlencode(&task.0))
}

pub fn activity(agent: &AgentId) -> String {
    format!("/v2/agents/{}/activity", urlencode(&agent.0))
}

pub fn search(query: &str) -> String {
    format!("/v2/agents/search?q={}", urlencode(query))
}

pub const PRESENCE: &str = "/v2/activity";
pub const REQUESTS: &str = "/v2/requests";
pub const PROJECTS: &str = "/v2/projects";

pub fn options(project: Option<&str>) -> String {
    match project {
        Some(p) => format!("/v2/options?project={}", urlencode(p)),
        None => "/v2/options".into(),
    }
}

pub fn issues(refresh: bool) -> &'static str {
    if refresh { "/v2/issues?refresh=1" } else { "/v2/issues" }
}

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
        assert_eq!(m.path(), "/v2/agents/session%2F1/messages");
        assert_eq!(m.body(), json!({"text": "Thanks", "attachments": [], "kind": "reply", "mark_read": true, "interaction_id": "info-1"}));
        let o = Message::to(None, MessageKind::Message, "Plan the week".into());
        assert_eq!((o.path(), o.body()), ("/v2/orchestrator/messages".to_string(), json!({"text": "Plan the week", "attachments": []})));
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
            ("close", close(&"agent-a".into(), true)),
            ("orchestrator_retry", orchestrator(Recovery::Retry, "run-7")),
            ("resolve_conflicts", resolve_conflicts(&"pr-1".into())),
            ("set_aside", set_aside(&"pr-1".into())),
            ("cancel_reason", cancel_reason(&"task-1".into(), "Superseded")),
            ("new_request", (REQUESTS.to_string(), new_request("request-9", "Ship it", Some("App"), &json!({"effort": "hard"}), Some("me/App#3"), &[a]))),
        ];
        for (name, (path, body)) in built {
            assert_eq!(contract[name]["path"], json!(path), "{name}");
            assert_eq!(contract[name]["body"], body, "{name}");
        }
    }
}
