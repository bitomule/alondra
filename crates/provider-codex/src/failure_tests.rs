use crate::{classify_failure, provider_failure};
use alondra_provider_contract::{RequestProblem, Verdict};
use serde_json::{json, Value};

fn verdict(info: Value) -> Option<Verdict> {
    classify_failure(&provider_failure(&json!({"codexErrorInfo": info, "message": "The native turn failed"})))
}

#[test]
fn only_the_accounts_quota_is_usage() {
    assert_eq!(verdict(json!("usageLimitExceeded")), Some(Verdict::Usage { resets_at: None }));
    assert_eq!(verdict(json!("rateLimitExceeded")), Some(Verdict::Outage), "a request rate limit waits like an outage and never exhausts the account");
}

#[test]
fn a_refresh_that_failed_for_good_loses_the_session() {
    assert_eq!(verdict(json!("unauthorized")), Some(Verdict::SessionLost));
}

#[test]
fn server_and_stream_failures_are_an_outage() {
    for info in [
        json!("serverOverloaded"),
        json!("internalServerError"),
        json!({"responseStreamConnectionFailed": {"httpStatusCode": 502}}),
        json!({"responseStreamDisconnected": {"httpStatusCode": null}}),
        json!({"responseTooManyFailedAttempts": {"httpStatusCode": 503}}),
        json!({"httpConnectionFailed": {"httpStatusCode": 503}}),
        json!({"httpConnectionFailed": {"httpStatusCode": null}}),
    ] {
        assert_eq!(verdict(info.clone()), Some(Verdict::Outage), "{info}");
    }
}

#[test]
fn a_connection_refused_with_401_or_403_is_doubtful() {
    assert_eq!(verdict(json!({"httpConnectionFailed": {"httpStatusCode": 401}})), Some(Verdict::Doubtful));
    assert_eq!(verdict(json!({"httpConnectionFailed": {"httpStatusCode": 403}})), Some(Verdict::Doubtful));
}

#[test]
fn a_wrong_request_is_the_tasks_problem() {
    assert_eq!(verdict(json!("badRequest")), Some(Verdict::RequestProblem { problem: RequestProblem::BadRequest }));
    assert_eq!(verdict(json!("contextWindowExceeded")), Some(Verdict::RequestProblem { problem: RequestProblem::ContextWindowExceeded }));
}

#[test]
fn any_other_code_is_not_the_providers() {
    for info in [json!("other"), json!("sandboxError"), json!({"httpConnectionFailed": {"httpStatusCode": 400}}), Value::Null] {
        assert_eq!(verdict(info.clone()), None, "{info}");
    }
}

#[test]
fn the_message_is_kept_bounded_and_without_user_data() {
    let failure = provider_failure(&json!({"codexErrorInfo": "unauthorized", "message": "Refresh failed for ana@example.com"}));
    assert_eq!(failure.detail.as_deref(), Some("Refresh failed for [email]"));
    assert_eq!(failure.error, "unauthorized");
}
