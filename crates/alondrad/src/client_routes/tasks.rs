//! What a task's page needs beyond the projection: how it got where it is, and why the user
//! cancelled it.
use super::{Client, Reply, storage};
use alondra_core::domain::{DomainError, TaskId};
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::Deserialize;
use serde_json::{Value, json};

const MAX_REASON_BYTES: usize = 4096;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Reason {
    reason: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Presence {
    client: String,
}

impl Client {
    /// The states the task entered, oldest first, each with the time of the commit that moved it.
    pub(super) fn task_history(&self, id: &str) -> Response {
        let task = TaskId(id.to_owned());
        match self.domain.state() {
            Ok(state) if state.tasks.contains_key(&task) => {}
            Ok(_) => return Reply::error(DomainError::new("not_found", "task does not exist")).into_response(),
            Err(_) => return storage(),
        }
        match self.domain.store.lock().expect("domain store poisoned").task_history(&task) {
            Ok(history) => Json(json!({"brief": null, "history": history})).into_response(),
            Err(_) => storage(),
        }
    }

    /// The user's words for cancelling a task, recorded beside the cancelled task.
    pub(super) fn cancel_reason(&self, id: &str, body: &Value) -> anyhow::Result<Reply> {
        let Ok(Reason { reason }) = serde_json::from_value(body.clone()) else {
            return Ok(Reply::error(DomainError::new("invalid_input", "invalid cancel reason")));
        };
        let reason = reason.trim();
        if reason.is_empty() || reason.len() > MAX_REASON_BYTES {
            return Ok(Reply::error(DomainError::new("invalid_input", "a reason has words and fits in 4 KiB")));
        }
        let task = TaskId(id.to_owned());
        let Some(cancelled) = self.domain.state()?.tasks.get(&task).map(alondra_core::domain::Task::cancelled) else {
            return Ok(Reply::error(DomainError::new("not_found", "task does not exist")));
        };
        if !cancelled {
            return Ok(Reply::error(DomainError::new("task_not_cancelled", "only a cancelled task has a reason")));
        }
        self.domain.store.lock().expect("domain store poisoned").set_cancel_reason(&task, reason)?;
        Ok(Reply::ok(json!({})))
    }

    /// The user is at a client: recorded, so the daemon can tell.
    pub(super) fn presence(&self, body: &Value) -> Reply {
        match serde_json::from_value::<Presence>(body.clone()) {
            Ok(Presence { client }) if !client.is_empty() && client.len() <= 64 => {
                self.domain.live.touch(&client);
                Reply::ok(json!({}))
            }
            _ => Reply::error(DomainError::new("invalid_input", "invalid presence")),
        }
    }
}
