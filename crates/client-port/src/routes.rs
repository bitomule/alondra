//! Where a domain operation goes: the daemon's resource routes, shared by the MCP tools and the
//! domain commands.
use anyhow::Result;
use serde_json::Value;

/// `s` as one URL path segment or query value: every byte but the unreserved ones escaped.
pub fn urlencode(s: &str) -> String {
    s.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => (b as char).to_string(),
            _ => format!("%{b:02X}"),
        })
        .collect()
}

/// The route and method of the resource a domain operation named `name` writes.
pub fn resource_target(name: &str, command: &Value) -> Result<(&'static str, String)> {
    let value = |key: &str| -> Result<&str> {
        command.get(key).and_then(Value::as_str).filter(|s| !s.is_empty())
            .ok_or_else(|| anyhow::anyhow!("{name} requires {key}"))
    };
    let id = |key: &str| -> Result<String> { Ok(urlencode(value(key)?)) };
    let target = match name {
        "ask" if asks_together(command) => ("POST", "/question-groups".to_owned()),
        "ask" | "ask_question" | "request_input" | "request_test" | "suggest" => ("POST", "/questions".to_owned()),
        "request_action" => ("POST", "/action-requests".to_owned()),
        "publish_information" => ("POST", "/messages".to_owned()),
        "raise_problem" => ("POST", "/problems".to_owned()),
        "withdraw" | "withdraw_question" => ("POST", format!("/interactions/{}/withdrawal", id("interaction_id")?)),
        "request_clarification" => ("POST", format!("/interactions/{}/clarification", id("interaction_id")?)),
        "answer_question" => ("POST", format!("/interactions/{}/answers", id("interaction_id")?)),
        "settle_suggestion" => ("POST", format!("/interactions/{}/settlement", id("interaction_id")?)),
        "settle_plan_review" => ("POST", format!("/tasks/{}/plan-review", id("task_id")?)),
        "settle_stall" => ("POST", format!("/tasks/{}/stall-settlement", id("task_id")?)),
        "mark_read" => ("PUT", format!("/interactions/{}/read-state", id("interaction_id")?)),
        "submit_feedback" => ("POST", format!("/interactions/{}/feedback", id("presentation_id")?)),
        "accept_delivery" => ("POST", format!("/interactions/{}/acceptance", id("presentation_id")?)),
        "authorize_merge" => ("POST", format!("/interactions/{}/merge-authorizations", id("presentation_id")?)),
        "create_request" => ("POST", "/requests".to_owned()),
        "close_request" => ("POST", format!("/requests/{}/close", id("request_id")?)),
        "title_request" => ("POST", format!("/requests/{}/title", id("request_id")?)),
        "create_task" => ("POST", format!("/requests/{}/tasks", id("request_id")?)),
        "assign_task" => ("PUT", format!("/tasks/{}/assignee", id("task_id")?)),
        "retitle_task" => ("PATCH", format!("/tasks/{}", id("task_id")?)),
        "set_dependencies" => ("PUT", format!("/tasks/{}/dependencies", id("task_id")?)),
        "set_relations" => ("PUT", format!("/tasks/{}/relations", id("task_id")?)),
        "set_priority" => ("PUT", format!("/tasks/{}/priority", id("task_id")?)),
        "record_verdict" => ("POST", format!("/tasks/{}/verdict", id("task_id")?)),
        "propose_specs" => ("POST", format!("/tasks/{}/specs", id("task_id")?)),
        "request_spec_change" => ("POST", format!("/tasks/{}/spec-changes", id("task_id")?)),
        "propose_amendment" => ("POST", format!("/tasks/{}/spec-amendments", id("task_id")?)),
        "propose_decision" => ("POST", format!("/tasks/{}/decisions", id("task_id")?)),
        "start_task" => ("POST", format!("/tasks/{}/start", id("task_id")?)),
        "cleanup_done" => ("POST", format!("/tasks/{}/cleanup", id("task_id")?)),
        "post_merge_work" => ("PUT", format!("/tasks/{}/post-merge-work", id("task_id")?)),
        "wait_for_tasks" => ("PUT", format!("/tasks/{}/task-wait", id("task_id")?)),
        "wait_for_mcp" => ("PUT", format!("/tasks/{}/mcp-wait", id("task_id")?)),
        "create_document" => ("POST", format!("/tasks/{}/documents", id("task_id")?)),
        "revise_document" => ("PUT", format!("/tasks/{}/documents/{}", id("task_id")?, id("document_id")?)),
        "decide_document_review" => ("PUT", format!("/tasks/{}/documents/{}/review", id("task_id")?, id("document_id")?)),
        "link_task_document" => ("POST", format!("/tasks/{}/document-links", id("task_id")?)),
        "register_pr" => ("POST", format!("/tasks/{}/pull-requests", id("task_id")?)),
        "link_interaction_subject" => ("PUT", format!("/interactions/{}/subject", id("interaction_id")?)),
        "present_result" => ("POST", format!("/tasks/{}/results", id("task_id")?)),
        "publish_stack_readiness" => ("POST", format!("/tasks/{}/stack-readiness", id("task_id")?)),
        "cancel_task" => ("POST", format!("/tasks/{}/cancellation", id("task_id")?)),
        "reopen_task" => ("POST", format!("/tasks/{}/reopening", id("task_id")?)),
        _ => anyhow::bail!("unknown domain resource operation: {name}"),
    };
    Ok(target)
}

/// Several questions asked as one card.
pub fn asks_together(command: &Value) -> bool {
    command.get("questions").and_then(Value::as_array).is_some_and(|questions| questions.len() > 1)
}
