//! Why an Antigravity session ended or refused an input, as its host reports it: a failure code
//! and, when `agy acp` never opened the session, the step it did not answer. Each code has the
//! message the node's result shows.
use crate::adapter::host_event;
use alondra_antigravity_protocol::Observation;
use alondra_provider_contract::OrderedObservation;
use serde_json::Value;

pub fn native_message(observation: &OrderedObservation) -> Option<Value> {
    match host_event(observation)?.observation {
        Observation::Sdk { payload, .. } => Some(payload),
        _ => None,
    }
}

/// Why a session ended without being asked to, from the host's `alondra/transportFailed`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionFailure {
    pub code: String,
    pub step: Option<String>,
}

impl std::fmt::Display for SessionFailure {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match &self.step {
            Some(step) => write!(formatter, "{} at {step}", self.code),
            None => formatter.write_str(&self.code),
        }
    }
}

pub fn session_failure(observation: &OrderedObservation) -> Option<SessionFailure> {
    failure_of(&native_message(observation)?)
}

fn failure_of(message: &Value) -> Option<SessionFailure> {
    if message.get("method")?.as_str()? != "alondra/transportFailed" {
        return None;
    }
    let code = message.pointer("/params/code")?.as_str()?.to_owned();
    let step = message.pointer("/params/step").and_then(Value::as_str).map(str::to_owned);
    Some(SessionFailure { code, step })
}

/// The message for a failure code of the host, or of a turn the provider ended as failed.
pub fn failure_message(code: &str) -> Option<&'static str> {
    Some(match code {
        "authentication_required" => "Antigravity is not signed in on this account. Sign in to Antigravity outside Alondra with the account's sign-in command, then send the input again",
        "native_version_unsupported" => "This Antigravity CLI is older than 1.5.12. Update it with `agy update`; Alondra never updates it",
        "native_version_unreadable" => "Antigravity CLI did not say which version it is, so Alondra did not start the session",
        "native_incompatible" => "This Antigravity CLI speaks another protocol or cannot resume sessions. Update Antigravity CLI",
        "native_spawn_failed" => "Antigravity CLI could not be started. Check that `agy` is installed and that the disk has room",
        "native_initialization_timeout" => "Antigravity CLI did not open the session in time",
        "native_initialization_failed" => "Antigravity CLI refused to open the session",
        "alondra_mcp_missing" => "Alondra's MCP server is missing from the node's settings, so the node could not report to Alondra",
        "native_process_exited" => "Antigravity CLI ended on its own. The task resumes the session with its next input",
        "native_input_closed" | "native_output_closed" => "The connection to Antigravity CLI closed",
        "native_protocol_failed" => "Antigravity CLI sent something that is not the Agent Client Protocol",
        "native_response_uncertain" => "An answer to Antigravity could not be delivered, so Alondra ended the session",
        "native_prompt_uncertain" => "Alondra could not tell whether Antigravity received the input",
        "native_session_not_durable" => "Alondra could not keep the Antigravity session to resume it later, so Alondra ended the session",
        "native_decision_not_durable" | "native_decision_capacity" | "native_request_identity_conflict" => "Antigravity asked for a permission Alondra could not keep track of, so Alondra ended the session",
        "max_tokens" => "Antigravity stopped the turn at the model's output limit",
        "max_turn_requests" => "Antigravity stopped the turn after its most model requests in one turn",
        "refusal" => "The model refused to continue the turn",
        "interrupted" => "The turn was interrupted",
        code if code.starts_with("agy_") => "Antigravity answered the input with an error",
        _ => return None,
    })
}

/// A refusal before any `session/prompt` was sent for an input: the reason the task holds.
pub fn account_refusal(observation: &OrderedObservation) -> Option<&'static str> {
    refusal_of(&native_message(observation)?)
}

/// Whether the refusal is the account's sign-in or key: the user signs in before the task goes on.
pub fn signed_out(observation: &OrderedObservation) -> bool {
    native_message(observation).is_some_and(|message| signed_out_of(&message))
}

fn signed_out_of(message: &Value) -> bool {
    refusal_of(message).is_some() && message.pointer("/params/code").and_then(Value::as_str) == Some("authentication_required")
}

fn refusal_of(message: &Value) -> Option<&'static str> {
    match message.get("method")?.as_str()? {
        "alondra/accountBlocked" => {}
        "alondra/inputRefused" if message.pointer("/params/nativeSubmitted").and_then(Value::as_bool) == Some(false) => {}
        _ => return None,
    }
    match message.pointer("/params/code")?.as_str()? {
        code @ ("authentication_required" | "native_version_unsupported" | "native_version_unreadable" | "native_incompatible") => failure_message(code),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn a_session_that_never_opened_names_the_step_agy_did_not_answer() {
        let failure = failure_of(&json!({"method": "alondra/transportFailed", "params": {"code": "native_initialization_timeout", "step": "session_new"}})).unwrap();
        assert_eq!(failure.to_string(), "native_initialization_timeout at session_new");
        assert_eq!(failure_of(&json!({"method": "alondra/transportFailed", "params": {"code": "native_process_exited"}})).unwrap().step, None);
        assert_eq!(failure_of(&json!({"method": "alondra/accountBlocked", "params": {"code": "authentication_required"}})), None);
    }

    #[test]
    fn only_a_refusal_that_lasts_holds_the_task_and_only_before_agy_took_the_input() {
        let refused = |method: &str, code: &str, submitted: bool| refusal_of(&json!({"method": method, "params": {"code": code, "nativeSubmitted": submitted}}));
        for code in ["authentication_required", "native_version_unsupported", "native_version_unreadable", "native_incompatible"] {
            assert_eq!(refused("alondra/accountBlocked", code, false), failure_message(code), "{code}");
            assert_eq!(refused("alondra/inputRefused", code, false), failure_message(code), "{code}");
            assert_eq!(refused("alondra/inputRefused", code, true), None, "{code} after agy took the input");
        }
        for code in ["native_spawn_failed", "native_initialization_timeout", "native_process_exited", "agy_-32603"] {
            assert_eq!(refused("alondra/inputRefused", code, false), None, "{code} is retried with the next input");
        }
        assert_eq!(refused("alondra/transportFailed", "authentication_required", false), None);
    }

    #[test]
    fn only_a_refused_sign_in_or_key_asks_to_sign_in() {
        assert!(signed_out_of(&json!({"method": "alondra/accountBlocked", "params": {"code": "authentication_required"}})));
        assert!(signed_out_of(&json!({"method": "alondra/inputRefused", "params": {"code": "authentication_required", "nativeSubmitted": false}})));
        assert!(!signed_out_of(&json!({"method": "alondra/accountBlocked", "params": {"code": "native_version_unsupported"}})));
        assert!(!signed_out_of(&json!({"method": "alondra/inputRefused", "params": {"code": "authentication_required", "nativeSubmitted": true}})));
    }

    #[test]
    fn every_code_the_host_and_the_normalizer_report_has_its_message() {
        for code in [
            "authentication_required", "native_version_unsupported", "native_version_unreadable", "native_incompatible",
            "native_spawn_failed", "native_initialization_timeout", "native_initialization_failed", "alondra_mcp_missing",
            "native_process_exited", "native_input_closed", "native_output_closed", "native_protocol_failed",
            "native_response_uncertain", "native_prompt_uncertain", "native_session_not_durable", "native_decision_not_durable", "native_decision_capacity",
            "native_request_identity_conflict", "max_tokens", "max_turn_requests", "refusal", "interrupted", "agy_-32603",
        ] {
            assert!(failure_message(code).is_some_and(|message| !message.is_empty()), "{code}");
        }
        assert_eq!(failure_message("something_new"), None);
    }
}
