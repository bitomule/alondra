//! What a Claude API error says about the account and about Claude. Claude Code labels every 401
//! and 403 of the API `authentication_failed`, also Cloudflare's challenge page in front of it, so
//! the label alone never says the session is gone: its text, its status and the retries before it
//! decide (code.claude.com/docs/en/errors.md; platform.claude.com/docs/en/api/errors: a real API
//! error is JSON with a `type`).
use alondra_provider_contract::{ProviderFailure, RefusalReason, RequestProblem, Verdict};

/// The texts with which Claude Code says the sign-in itself is gone.
const SESSION_LOST: [&str; 6] = [
    "oauth token revoked",
    "oauth token has expired",
    "oauth session expired and could not be refreshed",
    "reached its time limit",
    "not logged in",
    "login expired",
];
/// What a page in front of the API, not the API, answers with.
const EDGE_PAGE: [&str; 4] = ["<html", "<!doctype", "attention required", "cloudflare"];
const EDGE_BLOCK: &str = "request blocked before it reached anthropic's api";

/// The verdict of a failure Claude Code reported: the first rule that fits wins.
pub fn classify_failure(failure: &ProviderFailure) -> Verdict {
    let text = failure.detail.as_deref().unwrap_or_default().to_lowercase();
    let says = |words: &[&str]| words.iter().any(|word| text.contains(word));
    let authentication = failure.error == "authentication_failed";
    let server = |status: u16| status >= 500;
    match failure.error.as_str() {
        "max_output_tokens" => Verdict::None,
        "rate_limit" => Verdict::Usage { resets_at: failure.resets_at },
        "account_on_hold" => Verdict::Refused { reason: RefusalReason::AccountOnHold },
        "oauth_org_not_allowed" => Verdict::Refused { reason: RefusalReason::OrgNotAllowed },
        "billing_error" => Verdict::Refused { reason: RefusalReason::Billing },
        "verification_required" => Verdict::Refused { reason: RefusalReason::VerificationRequired },
        _ if authentication && says(&SESSION_LOST) => Verdict::SessionLost,
        _ if authentication && failure.status == Some(403) && failure.detail.is_some() && (says(&EDGE_PAGE) || !text.contains("\"type\"")) => Verdict::Outage,
        "invalid_request" if text.contains(EDGE_BLOCK) => Verdict::Outage,
        _ if authentication && failure.retried.iter().flatten().any(|status| server(*status)) => Verdict::Outage,
        "overloaded" | "server_error" | "unknown" => Verdict::Outage,
        _ if failure.status.is_some_and(server) => Verdict::Outage,
        "authentication_failed" => Verdict::Doubtful,
        "cloud_credential_error" => Verdict::RequestProblem { problem: RequestProblem::CloudCredentials },
        "invalid_request" => Verdict::RequestProblem { problem: RequestProblem::InvalidRequest },
        "model_not_found" => Verdict::RequestProblem { problem: RequestProblem::ModelNotFound },
        _ => Verdict::Outage,
    }
}
