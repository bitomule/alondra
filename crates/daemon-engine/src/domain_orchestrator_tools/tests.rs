//! The launch tools' tests that start no node; the ones that do run with `engine-sdk`.
use super::*;
use serde_json::json;

#[tokio::test]
async fn send_takes_no_model() {
    let parsed: Result<SendBody, _> = serde_json::from_value(json!({"node": "Instance B", "text": "Go", "task": "t", "model": "sonnet"}));
    assert!(parsed.is_err(), "deny_unknown_fields refuses it as invalid_input");
}

#[test]
fn a_launch_or_send_with_an_unknown_approach_is_refused_naming_the_three() {
    let launch = serde_json::from_value::<LaunchBody>(json!({"name": "n", "cwd": "/", "task": "t", "approach": "swarm"})).err().unwrap().to_string();
    let send = serde_json::from_value::<SendBody>(json!({"node": "n", "text": "Go", "approach": "swarm"})).err().unwrap().to_string();
    for error in [launch, send] {
        assert!(["solo", "subagents", "workflow"].iter().all(|name| error.contains(name)), "{error}");
    }
}

#[test]
fn a_stopped_session_is_one_claude_agents_no_longer_shows_running() {
    let listing = json!([{"id": "agent-b", "state": "idle"}, {"id": "old00000", "state": "stopped"}, {"id": "done0000", "state": "done"}]).to_string();
    assert!(crate::sdk_handover::still_running(&listing, "agent-b"));
    for gone in ["old00000", "done0000", "missing0"] {
        assert!(!crate::sdk_handover::still_running(&listing, gone), "{gone}");
    }
}

#[test]
fn partial_launch_plan_refuses_null_unknown_and_duplicate_fields() {
    for changes in [json!({"mission": "lost"}), json!({"name": "lost"}), json!({"cwd": "/"}),
        json!({"role": "review"}), json!({"project": "other"}), json!({"model": null}), json!({"effort": null}),
        json!({"approach": null}), json!({"tags": null}), json!({"reason": null}), json!({"effort": "high", "reasoning_effort": "low"})] {
        assert!(serde_json::from_value::<UpdateLaunchPlanBody>(json!({"task_id": "t", "changes": changes})).is_err());
    }
}
