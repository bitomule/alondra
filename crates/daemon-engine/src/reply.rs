//! What a route answers, and the HTTP status of each domain refusal.
use alondra_core::domain::DomainError;
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

    pub fn into_value(self) -> Value {
        json!({"status": self.status.as_u16(), "body": self.body})
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
        "invalid_input" | "invalid_action_for_kind" | "launch_plan_required" => StatusCode::UNPROCESSABLE_ENTITY,
        "payload_too_large" => StatusCode::PAYLOAD_TOO_LARGE,
        "storage_unavailable" => StatusCode::INTERNAL_SERVER_ERROR,
        _ => StatusCode::CONFLICT,
    }
}
