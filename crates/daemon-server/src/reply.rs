//! What a route answers over HTTP, and the HTTP status of each domain refusal.
use alondra_domain_model::DomainError;
use axum::Json;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde_json::{Value, json};

/// What a route answers: the status and the JSON body both the local listener and the relay
/// carry.
pub struct Reply {
    pub status: StatusCode,
    pub body: Value,
}

impl Reply {
    pub fn ok(body: Value) -> Self {
        Self { status: StatusCode::OK, body }
    }

    pub fn error(error: DomainError) -> Self {
        Self { status: status_for(&error), body: json!(error) }
    }

    pub fn answer(result: Result<Value, DomainError>) -> Self {
        result.map_or_else(Self::error, Self::ok)
    }

    pub fn into_value(self) -> Value {
        json!({"status": self.status.as_u16(), "body": self.body})
    }
}

impl From<alondra_daemon_engine::reply::Reply> for Reply {
    fn from(reply: alondra_daemon_engine::reply::Reply) -> Self {
        Self { status: reply.status, body: reply.body }
    }
}

impl IntoResponse for Reply {
    fn into_response(self) -> Response {
        (self.status, Json(self.body)).into_response()
    }
}

pub fn status_for(error: &DomainError) -> StatusCode {
    match error.code.as_str() {
        "forbidden" | "not_assigned" => StatusCode::FORBIDDEN,
        "not_found" => StatusCode::NOT_FOUND,
        "invalid_input" | "invalid_action_for_kind" | "launch_plan_required" | "card_language_mismatch" => StatusCode::UNPROCESSABLE_ENTITY,
        "payload_too_large" => StatusCode::PAYLOAD_TOO_LARGE,
        "storage_unavailable" => StatusCode::INTERNAL_SERVER_ERROR,
        _ => StatusCode::CONFLICT,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_relayed_answer_carries_the_status_the_local_listener_gives() {
        let invalid = Reply::answer(Err(DomainError::new("invalid_input", "invalid message"))).into_value();
        assert_eq!((invalid["status"].as_u64(), invalid["body"]["code"].as_str()), (Some(422), Some("invalid_input")));
        let unknown = Reply::answer(Err(DomainError::new("not_found", "node does not exist"))).into_value();
        assert_eq!((unknown["status"].as_u64(), unknown["body"]["code"].as_str()), (Some(404), Some("not_found")));
        let sent = Reply::answer(Ok(json!({"reaches": "task"}))).into_value();
        assert_eq!(sent, json!({"status": 200, "body": {"reaches": "task"}}));
    }
}
