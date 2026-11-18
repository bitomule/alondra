use crate::classify_failure;
use alondra_provider_contract::{ProviderFailure, RefusalReason, RequestProblem, Verdict};

fn failure(error: &str, status: Option<u16>, detail: Option<&str>, retried: &[Option<u16>]) -> ProviderFailure {
    ProviderFailure { error: error.into(), status, detail: detail.map(str::to_owned), retried: retried.to_vec(), resets_at: None }
}

fn verdict(error: &str, status: Option<u16>, detail: Option<&str>) -> Verdict {
    classify_failure(&failure(error, status, detail, &[]))
}

const API_403: &str = r#"Failed to authenticate. API Error: 403 {"type":"error","error":{"type":"permission_error","message":"Forbidden"},"request_id":"req_1"}"#;

#[test]
fn an_answer_cut_at_the_output_limit_is_no_failure() {
    assert_eq!(verdict("max_output_tokens", None, None), Verdict::None);
}

#[test]
fn the_usage_limit_keeps_its_reset() {
    let limited = ProviderFailure { resets_at: Some(1_790_202_000), ..failure("rate_limit", Some(429), None, &[]) };
    assert_eq!(classify_failure(&limited), Verdict::Usage { resets_at: Some(1_790_202_000) });
}

#[test]
fn an_account_the_api_refuses_names_its_reason() {
    for (error, reason) in [
        ("account_on_hold", RefusalReason::AccountOnHold),
        ("oauth_org_not_allowed", RefusalReason::OrgNotAllowed),
        ("billing_error", RefusalReason::Billing),
        ("verification_required", RefusalReason::VerificationRequired),
    ] {
        assert_eq!(verdict(error, Some(403), None), Verdict::Refused { reason }, "{error}");
    }
}

#[test]
fn only_the_texts_of_a_gone_sign_in_lose_the_session() {
    for text in [
        "OAuth token revoked. Please run /login",
        "Failed to authenticate. API Error: 401 OAuth token has expired",
        "OAuth session expired and could not be refreshed",
        "Your session reached its time limit",
        "Not logged in · Please run /login",
        "Login expired",
    ] {
        assert_eq!(verdict("authentication_failed", Some(401), Some(text)), Verdict::SessionLost, "{text}");
    }
}

// The 13 Nov outage: Claude Code retried 503s, then ended the turn with Cloudflare's 403 page
// labelled authentication_failed. It is Claude failing for everyone, never the account.
#[test]
fn cloudflares_403_page_after_retried_503s_is_an_outage_never_a_refusal() {
    let cloudflare = "Failed to authenticate. API Error: 403 Attention Required! | Cloudflare";
    assert_eq!(classify_failure(&failure("authentication_failed", Some(403), Some(cloudflare), &[Some(503), Some(503), None])), Verdict::Outage);
    assert_eq!(verdict("authentication_failed", Some(403), Some(cloudflare)), Verdict::Outage);
    assert_eq!(verdict("authentication_failed", Some(403), Some("<!DOCTYPE html><html>blocked</html>")), Verdict::Outage);
}

#[test]
fn a_403_without_the_apis_json_error_is_an_outage() {
    assert_eq!(verdict("authentication_failed", Some(403), Some("API Error: 403 Forbidden")), Verdict::Outage);
}

#[test]
fn a_request_blocked_at_the_edge_is_an_outage() {
    assert_eq!(verdict("invalid_request", Some(403), Some("Request blocked before it reached Anthropic's API")), Verdict::Outage);
}

#[test]
fn an_authentication_error_after_retried_server_errors_is_an_outage() {
    assert_eq!(classify_failure(&failure("authentication_failed", Some(401), Some("API Error: 401"), &[Some(503), Some(529)])), Verdict::Outage);
}

#[test]
fn server_errors_and_unknown_categories_are_an_outage() {
    for (error, status) in [("overloaded", Some(529)), ("server_error", Some(500)), ("unknown", None), ("api_error", Some(503)), ("a_category_claude_adds_later", None)] {
        assert_eq!(verdict(error, status, None), Verdict::Outage, "{error}");
    }
}

#[test]
fn any_other_authentication_error_is_doubtful() {
    assert_eq!(verdict("authentication_failed", Some(401), Some("API Error: 401")), Verdict::Doubtful);
    assert_eq!(verdict("authentication_failed", Some(403), Some(API_403)), Verdict::Doubtful);
    assert_eq!(verdict("authentication_failed", Some(403), None), Verdict::Doubtful, "no text to read is no evidence");
    assert_eq!(classify_failure(&failure("authentication_failed", Some(401), None, &[Some(401), Some(429)])), Verdict::Doubtful);
}

#[test]
fn a_wrong_request_is_the_tasks_problem_not_the_accounts() {
    for (error, problem) in [
        ("cloud_credential_error", RequestProblem::CloudCredentials),
        ("invalid_request", RequestProblem::InvalidRequest),
        ("model_not_found", RequestProblem::ModelNotFound),
    ] {
        assert_eq!(verdict(error, Some(400), Some(r#"{"type":"error"}"#)), Verdict::RequestProblem { problem }, "{error}");
    }
}
