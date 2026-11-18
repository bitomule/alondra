//! What a Codex turn error says about the account and about OpenAI. `unauthorized` comes only from
//! a refresh that failed for good; the server and stream errors are OpenAI failing for everyone
//! (Codex app-server schema 0.163.0, codex-rs/protocol/src/error.rs).
use alondra_provider_contract::{bounded_detail, ProviderFailure, RequestProblem, Verdict};
use serde_json::Value;

/// The failure Codex reported in a turn's or a notification's `error`: the variant of its
/// `codexErrorInfo`, the HTTP status it carries and its message.
pub fn provider_failure(error: &Value) -> ProviderFailure {
    let info = error.get("codexErrorInfo").unwrap_or(&Value::Null);
    let (kind, fields) = match info {
        Value::String(kind) => (kind.clone(), &Value::Null),
        Value::Object(map) => map.iter().next().map_or((String::new(), &Value::Null), |(kind, fields)| (kind.clone(), fields)),
        _ => (String::new(), &Value::Null),
    };
    let status = fields.get("httpStatusCode").and_then(Value::as_u64).and_then(|status| u16::try_from(status).ok());
    let detail = error.get("message").and_then(Value::as_str).and_then(bounded_detail);
    ProviderFailure { error: kind, status, detail, ..ProviderFailure::default() }
}

/// The verdict of a Codex failure; none for a code that is not the provider's, which the task's
/// own failed turn handles.
pub fn classify_failure(failure: &ProviderFailure) -> Option<Verdict> {
    let server = failure.status.is_none_or(|status| status >= 500);
    Some(match failure.error.as_str() {
        "usageLimitExceeded" => Verdict::Usage { resets_at: failure.resets_at },
        // A rate limit of requests passes in moments and says nothing of the account's quota.
        "rateLimitExceeded" => Verdict::Outage,
        "unauthorized" => Verdict::SessionLost,
        "serverOverloaded" | "internalServerError" | "responseTooManyFailedAttempts" => Verdict::Outage,
        kind if kind.starts_with("responseStream") => Verdict::Outage,
        "httpConnectionFailed" if server => Verdict::Outage,
        "httpConnectionFailed" if matches!(failure.status, Some(401 | 403)) => Verdict::Doubtful,
        "badRequest" => Verdict::RequestProblem { problem: RequestProblem::BadRequest },
        "contextWindowExceeded" => Verdict::RequestProblem { problem: RequestProblem::ContextWindowExceeded },
        _ => return None,
    })
}
