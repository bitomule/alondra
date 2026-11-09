use super::{LocalRequest, QuietHours, Transport};
use anyhow::Result;
use serde_json::{Value, json};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Operation {
    PushBinding,
    RemoveBinding,
    ActivityKey,
}

impl Operation {
    pub(super) fn for_request(request: &LocalRequest) -> Option<Self> {
        match request.path.as_str() {
            super::PUSH_BINDING_ROUTE | "/v1/service/push-binding" => Some(Self::PushBinding),
            super::PUSH_BINDING_REMOVE_ROUTE => Some(Self::RemoveBinding),
            super::ACTIVITY_KEY_ROUTE => Some(Self::ActivityKey),
            _ => None,
        }
    }

    fn name(self) -> &'static str {
        match self {
            Self::PushBinding => "push_binding",
            Self::RemoveBinding => "remove_push_binding",
            Self::ActivityKey => "activity_key",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Rejection {
    InvalidBody,
    UnknownField,
    InvalidBinding,
    InvalidQuietHours,
    InvalidRevision,
    MissingQuietHours,
    InvalidActivityKey,
    NotEnrolled,
    StalePreferences(i64),
    UnsupportedRoute,
    InvalidMethod,
    ApprovalRevoked,
    Capacity,
    Expired,
}

impl Rejection {
    pub(super) fn kind(self) -> &'static str {
        match self {
            Self::InvalidBody => "invalid_registration_body",
            Self::UnknownField => "unknown_registration_field",
            Self::InvalidBinding => "invalid_push_binding",
            Self::InvalidQuietHours => "invalid_quiet_hours",
            Self::InvalidRevision => "invalid_preferences_revision",
            Self::MissingQuietHours => "missing_quiet_hours",
            Self::InvalidActivityKey => "invalid_activity_key",
            Self::NotEnrolled => "device_not_enrolled",
            Self::StalePreferences(_) => "stale_push_preferences",
            Self::UnsupportedRoute => "unsupported_registration_route",
            Self::InvalidMethod => "invalid_registration_method",
            Self::ApprovalRevoked => "device_approval_revoked",
            Self::Capacity => "operation_capacity",
            Self::Expired => "registration_expired",
        }
    }

    pub(super) fn status(self) -> u16 {
        match self {
            Self::StalePreferences(_) | Self::NotEnrolled => 409,
            Self::UnsupportedRoute => 404,
            Self::InvalidMethod => 400,
            Self::ApprovalRevoked => 403,
            Self::Capacity => 429,
            Self::Expired => 410,
            _ => 422,
        }
    }

    pub(super) fn log(self, operation: Operation, transport: Transport) {
        self.log_with_status(operation, transport, self.status());
    }

    pub(super) fn log_with_status(self, operation: Operation, transport: Transport, status: u16) {
        let operation = operation.name();
        let reason = self.kind();
        log!("device registration rejected: operation={operation} transport={transport:?} status={status} reason={reason}");
    }

    pub(super) fn response(self) -> Value {
        let mut result = json!({"status": self.status(), "body": {"kind": self.kind(), "error": self.to_string()}});
        if let Self::StalePreferences(current) = self {
            // Older phones read these fields beside status, newer ones read the body.
            result["error"] = json!(self.kind());
            result["kind"] = json!(self.kind());
            result["current_revision"] = json!(current);
            result["body"]["current_revision"] = json!(current);
        }
        result
    }
}

impl std::fmt::Display for Rejection {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::InvalidBody => "registration body must be an object",
            Self::UnknownField => "unknown registration field",
            Self::InvalidBinding => "invalid push binding",
            Self::InvalidQuietHours => "quiet_start and quiet_end must both be HH:mm or both null",
            Self::InvalidRevision => "preferences_revision must be an integer from 1 to 9007199254740991",
            Self::MissingQuietHours => "versioned preferences require quiet_start and quiet_end",
            Self::InvalidActivityKey => "invalid activity key",
            Self::NotEnrolled => "device is not actively enrolled with the relay",
            Self::StalePreferences(_) => "stale_push_preferences",
            Self::UnsupportedRoute => "this daemon does not support this registration route",
            Self::InvalidMethod => "registration requires POST with a body",
            Self::ApprovalRevoked => "device is not locally approved",
            Self::Capacity => "the Mac has no room for another pending registration",
            Self::Expired => "the registration receipt has expired",
        })
    }
}

impl std::error::Error for Rejection {}

pub(super) fn response(error: &anyhow::Error, operation: Operation, transport: Transport) -> Result<Value> {
    let Some(rejection) = error.downcast_ref::<Rejection>() else {
        return Err(anyhow::anyhow!("registration failed without a typed rejection"));
    };
    rejection.log(operation, transport);
    Ok(rejection.response())
}

#[derive(Clone, Copy)]
pub(super) enum PreferencesOutcome {
    Accepted,
    Rejected(Rejection),
}

impl PreferencesOutcome {
    pub(super) fn response(self) -> Value {
        match self {
            Self::Accepted => json!({"status":204}),
            Self::Rejected(rejection) => rejection.response(),
        }
    }
}

pub(super) fn object(request: &LocalRequest) -> Result<&serde_json::Map<String, Value>> {
    request.body.as_ref().and_then(Value::as_object).ok_or_else(|| Rejection::InvalidBody.into())
}

pub(super) fn quiet_hours(map: &serde_json::Map<String, Value>) -> Result<Option<QuietHours>> {
    QuietHours::parse_update(map.get("quiet_start"), map.get("quiet_end"))
        .map_err(|_| Rejection::InvalidQuietHours.into())
}
