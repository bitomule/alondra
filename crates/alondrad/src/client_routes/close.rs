//! The close route: closing a `claude --bg` session of an earlier release by hand, from the apps
//! or the CLI. A node of the Agent SDK ends with its task.
use super::{Client, Reply};
pub(crate) use crate::node_close::Answer;
use crate::node_close::{CloseOutcome, Reason, Sessions, close_node};
use alondra_core::domain::{AgentId, DomainError};
use axum::http::StatusCode;
use serde::Deserialize;
use serde_json::{Value, json};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Body {}

fn refused(message: impl Into<String>, reasons: Vec<Reason>) -> Reply {
    let message = message.into();
    Reply { status: StatusCode::CONFLICT, body: json!({"code": "close_refused", "error": message, "message": message, "reasons": reasons}) }
}

fn failed(status: StatusCode, message: impl Into<String>) -> Reply {
    let message = message.into();
    Reply { status, body: json!({"code": "close_failed", "error": message, "message": message}) }
}

impl Client {
    pub(super) async fn close_session(&self, agent_id: &str, body: &Value) -> anyhow::Result<Reply> {
        if serde_json::from_value::<Body>(body.clone()).is_err() {
            return Ok(Reply::error(DomainError::new("invalid_input", "invalid close request")));
        }
        let agent = AgentId(agent_id.to_owned());
        let Some(known) = self.domain.state()?.agents.get(&agent).cloned() else {
            return Ok(Reply::error(DomainError::new("not_found", "agent does not exist")));
        };
        if self.domain.read().index().node(&agent).is_some_and(|node| node.through_sdk) {
            return Ok(refused("This node runs through the Agent SDK and ends with its task: cancel its task to stop it", vec![]));
        }
        let sessions = self.sessions();
        let name = self.domain.live.node(agent_id).and_then(|node| node.name.clone()).unwrap_or(known.name);
        Ok(match close_node(&self.domain, sessions.as_ref(), &agent, true).await? {
            CloseOutcome::Closed { output } => Reply::ok(json!({"ok": true, "interactive": false, "output": output})),
            CloseOutcome::NotListed => Reply::ok(json!({"ok": true, "interactive": true, "output": format!(
                "{name} is an interactive session, not a background job: Alondra stopped tracking it and left the terminal session itself open. Close it there with /exit when you are done."
            )})),
            CloseOutcome::Refused { reasons, message } => refused(message, reasons),
            CloseOutcome::Failed { status, message } => failed(status, message),
        })
    }

    /// What closing a session asks of the machine; the closer of finished tasks shares it.
    pub(crate) fn sessions(&self) -> std::sync::Arc<dyn Sessions> {
        self.sessions.lock().expect("session control poisoned").clone()
    }
}
