//! The answer Antigravity gets for a `session/request_permission` the host persisted. An ACP
//! request names its own options; Alondra answers with one of them, chosen here from the request
//! as the domain kept it, and never with an `allow_always` or `reject_always` option, which would
//! write permission rules into the user's `agy` settings.
use alondra_antigravity_protocol::{Action, Decision as WireDecision, DecisionKind as WireKind};
use alondra_foundation::RequestId;
use alondra_provider_contract::AdapterError;
use alondra_store::{Decision, DecisionKind, DecisionRequest};
use serde_json::{Map, Value};

/// The option `decision` selects among `options`, the request's own: the first `allow_once` for
/// an allow, the option named by the answer for a multiple-choice question. `None` when the
/// request offers no such option.
pub fn chosen_option<'a>(options: &'a [Value], decision: &Decision) -> Option<&'a str> {
    let field = |option: &'a Value, key: &str| option.get(key).and_then(Value::as_str);
    match decision {
        Decision::Allow => options.iter().find(|option| field(option, "kind") == Some("allow_once")),
        Decision::Answers { answers } if answers.len() == 1 => options
            .iter()
            .filter(|option| !matches!(field(option, "kind"), Some("allow_always" | "reject_always")))
            .find(|option| field(option, "name") == Some(answers[0].as_str())),
        _ => None,
    }
    .and_then(|option| field(option, "optionId"))
}

/// Whether a request is one of Antigravity's own multiple-choice questions: it offers neither an
/// `allow_once` nor a `reject_once` option, so only one of its options answers it.
pub fn is_question(options: &[Value]) -> bool {
    !options.iter().any(|option| matches!(option.get("kind").and_then(Value::as_str), Some("allow_once" | "reject_once")))
}

/// The host action that answers the request persisted at `epoch` with `decision`. An allow or an
/// answer carries the option chosen here; a deny is answered by the host with the request's first
/// `reject_once` option, or cancelled when it has none. The domain supersedes only plans, which
/// Antigravity never asks for.
pub(super) fn respond_action(epoch: u64, request: &DecisionRequest, decision: &Decision) -> Result<Action, AdapterError> {
    let payload: Value = serde_json::from_slice(request.payload()).map_err(|_| AdapterError::InvalidObservation)?;
    let options = payload.pointer("/native/options").and_then(Value::as_array).ok_or(AdapterError::InvalidObservation)?;
    let kind = request.kind();
    let decision = match (decision, kind) {
        (Decision::Allow, DecisionKind::Permission) | (Decision::Answers { .. }, DecisionKind::Question) => {
            let option = chosen_option(options, decision).ok_or(AdapterError::InvalidObservation)?;
            WireDecision::Allow { updated_input: Map::from_iter([("optionId".to_owned(), Value::String(option.to_owned()))]) }
        }
        (Decision::Deny { message }, DecisionKind::Permission | DecisionKind::Question) => WireDecision::Deny { message: message.clone() },
        _ => return Err(AdapterError::InvalidObservation),
    };
    Ok(Action::Respond {
        request_id: RequestId(request.request_id().to_owned()),
        epoch,
        kind: match kind {
            DecisionKind::Permission => WireKind::Permission,
            DecisionKind::Question => WireKind::Question,
            DecisionKind::Plan | DecisionKind::Elicitation => return Err(AdapterError::InvalidObservation),
        },
        tool_use_id: request.tool_use_id().map(str::to_owned),
        decision,
        asked: payload.as_object().cloned().ok_or(AdapterError::InvalidObservation)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn asked(kind: DecisionKind, options: Value) -> DecisionRequest {
        let payload = json!({"provider": "antigravity", "method": "session/request_permission", "tool": "echo hi",
            "native": {"sessionId": "session-1", "toolCall": {"toolCallId": "call_1", "kind": "execute", "title": "echo hi"}, "options": options},
            "input": {"options": options}});
        DecisionRequest::new("string:1".into(), Some("call_1".into()), kind, serde_json::to_vec(&payload).unwrap()).unwrap()
    }
    fn decision(action: Action) -> WireDecision {
        match action {
            Action::Respond { decision, .. } => decision,
            other => panic!("{other:?}"),
        }
    }
    fn option(id: &str) -> WireDecision {
        WireDecision::Allow { updated_input: json!({"optionId": id}).as_object().unwrap().clone() }
    }

    #[test]
    fn allow_selects_the_first_allow_once_option_and_never_an_always_one() {
        let request = asked(DecisionKind::Permission, json!([
            {"optionId": "always", "name": "Always allow", "kind": "allow_always"},
            {"optionId": "allow", "name": "Allow", "kind": "allow_once"},
            {"optionId": "deny", "name": "Deny", "kind": "reject_once"},
        ]));
        let action = respond_action(4, &request, &Decision::Allow).unwrap();
        let Action::Respond { ref request_id, epoch, ref tool_use_id, kind, ref asked, .. } = action else { panic!() };
        assert_eq!((request_id.0.as_str(), epoch, tool_use_id.as_deref(), kind), ("string:1", 4, Some("call_1"), WireKind::Permission));
        assert_eq!(asked["native"]["toolCall"]["toolCallId"], "call_1");
        assert_eq!(decision(action), option("allow"));
    }

    #[test]
    fn deny_reaches_the_host_as_a_deny_which_picks_reject_once() {
        let request = asked(DecisionKind::Permission, json!([{"optionId": "allow", "name": "Allow", "kind": "allow_once"}, {"optionId": "deny", "name": "Deny", "kind": "reject_once"}]));
        assert_eq!(decision(respond_action(4, &request, &Decision::Deny { message: "Not now".into() }).unwrap()), WireDecision::Deny { message: "Not now".into() });
    }

    #[test]
    fn an_answer_selects_the_option_it_names_and_nothing_else() {
        let options = json!([
            {"optionId": "a", "name": "Keep the current schema", "kind": "other"},
            {"optionId": "b", "name": "Migrate the schema", "kind": "other"},
            {"optionId": "c", "name": "Remember: always migrate", "kind": "allow_always"},
        ]);
        assert!(is_question(options.as_array().unwrap()));
        let request = asked(DecisionKind::Question, options);
        let answer = |text: &str| respond_action(5, &request, &Decision::Answers { answers: vec![text.into()] });
        assert_eq!(decision(answer("Migrate the schema").unwrap()), option("b"));
        assert!(answer("Remember: always migrate").is_err(), "an always option writes rules into the user's settings");
        assert!(answer("Something the request does not offer").is_err(), "a question is answered only with one of its options");
        assert!(respond_action(5, &request, &Decision::Answers { answers: vec!["Keep the current schema".into(), "Migrate the schema".into()] }).is_err());
    }

    #[test]
    fn a_request_without_allow_once_cannot_be_allowed() {
        let request = asked(DecisionKind::Permission, json!([{"optionId": "always", "name": "Always allow", "kind": "allow_always"}, {"optionId": "deny", "name": "Deny", "kind": "reject_once"}]));
        assert!(!is_question(&[json!({"kind": "reject_once"})]));
        assert!(respond_action(4, &request, &Decision::Allow).is_err());
    }

    #[test]
    fn superseded_and_other_answers_are_not_antigravitys_to_take() {
        let request = asked(DecisionKind::Permission, json!([{"optionId": "allow", "name": "Allow", "kind": "allow_once"}]));
        for refused in [Decision::Superseded, Decision::Decline, Decision::Accept { text: "yes".into(), content: None }, Decision::Answers { answers: vec!["Allow".into()] }] {
            assert!(respond_action(4, &request, &refused).is_err(), "{refused:?}");
        }
    }
}
