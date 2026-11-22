//! Decisions: what the host asks the daemon when Claude calls a tool its rules do not allow, or an
//! MCP server asks for input, as the fake native Claude does for `decide:<scenario> <argument>`.
use super::*;

/// The one decision the host asked for, with the plan file's path, which each side keeps in a
/// folder of its own, as `<plan file>`.
fn only_decision(prompt: &str) -> (Value, PathBuf) {
    let launched = launch(prompt, None);
    assert_eq!(launched.decisions.len(), 1, "{:#?}", launched.observations);
    let mut decision = launched.decisions[0].clone();
    if let Some(path) = decision["payload"].get_mut("input").and_then(|input| input.get_mut("planFilePath")) {
        assert!(path.as_str().is_some_and(|path| path.ends_with(".plan.md")), "{path}");
        *path = "<plan file>".into();
    }
    (decision, launched.cwd)
}

#[test]
fn a_command_claude_may_not_run_is_asked_with_the_command_and_its_folder() {
    let (decision, cwd) = only_decision("decide:bash make test");
    assert_eq!(decision, serde_json::json!({"kind": "permission", "tool_use_id": "toolu_fixture_1", "payload": {
        "tool": "Bash", "input": {"command": "make test"}, "request": {"toolUseID": "toolu_fixture_1", "requestId": "decision-1"}, "cwd": cwd}}));
}

#[test]
fn a_question_is_asked_with_its_options() {
    let (decision, _) = only_decision("decide:question");
    assert_eq!((&decision["kind"], &decision["payload"]["tool"]), (&serde_json::json!("question"), &serde_json::json!("AskUserQuestion")));
    assert_eq!(decision["payload"]["input"]["questions"][0]["options"][1]["label"], "Blue", "{decision}");
}

#[test]
fn a_plan_is_asked_with_its_text_and_file() {
    let (decision, _) = only_decision("decide:plan");
    assert_eq!((&decision["kind"], &decision["payload"]["input"]), (&serde_json::json!("plan"),
        &serde_json::json!({"plan": "# Fixture plan\n\n- [ ] Change the fixture\n- [ ] Test the fixture", "planFilePath": "<plan file>"})));
}

#[test]
fn a_tool_of_alondras_plugin_is_asked_naming_its_server() {
    let (decision, _) = only_decision("decide:alondra");
    assert_eq!(decision["payload"]["request"]["mcpServer"], serde_json::json!({"name": "plugin:alondra:alondra", "source": "plugin"}), "{decision}");
}

#[test]
fn a_servers_request_for_input_is_asked_with_its_form() {
    let (decision, _) = only_decision("decide:elicitation");
    assert_eq!(decision, serde_json::json!({"kind": "elicitation", "tool_use_id": null, "payload": {
        "serverName": "fixture-server", "message": "Which fixture name?", "mode": "form",
        "requestedSchema": {"type": "object", "properties": {"name": {"type": "string", "title": "Name"}}, "required": ["name"]}}}));
}
