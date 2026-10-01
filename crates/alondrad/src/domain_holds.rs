//! A Claude Code permission prompt, or a plan waiting for approval, of a background Agent
//! becomes a held card the user answers from any device. The hook keeps waiting and replies
//! with that answer, as 8ccd131 did; a hook that goes away withdraws its card.
use crate::domain_runtime::DomainRuntime;
use crate::http::{permission_detail, permission_title, str_field, tool_summary};
use alondra_core::domain_v2::{
    Actor, AgentId, Answer, Command, CommandEnvelope, Hold, InputValue, InteractionId, InteractionPayload,
    PROTOCOL_VERSION,
};
use anyhow::Result;
use serde_json::{Value, json};

pub(crate) struct Held {
    pub id: InteractionId,
    pub hold: Hold,
    pub input: Value,
}

/// Which hooks are held: any permission prompt, and the parent session's plan. Only a
/// background session working on an active Task is held; a session in the user's own
/// terminal answers there.
pub(crate) fn hold(domain: &DomainRuntime, payload: &Value, background: bool) -> Result<Option<Held>> {
    let event = str_field(payload, "hook_event_name").unwrap_or_default();
    let tool = str_field(payload, "tool_name");
    let hold = match (event.as_str(), tool.as_deref()) {
        ("PermissionRequest", _) => Hold::Permission,
        ("PreToolUse", Some("ExitPlanMode")) if payload.get("agent_id").is_none() => Hold::Plan,
        _ => return Ok(None),
    };
    let Some(session_id) = str_field(payload, "session_id") else { return Ok(None) };
    if !background {
        return Ok(None);
    }
    let agent_id = AgentId(session_id);
    let state = domain.state()?;
    let Some(task) = state.tasks.values()
        .find(|task| task.agent_id.as_ref() == Some(&agent_id) && !task.state.terminal())
    else {
        return Ok(None);
    };
    let input = payload.get("tool_input").cloned().unwrap_or(Value::Null);
    let body = match hold {
        Hold::Permission => {
            let summary = tool_summary(tool.as_deref(), Some(&input));
            let title = permission_title(tool.as_deref(), &input, summary.as_deref());
            match permission_detail(&input, summary.as_deref()) {
                Some(detail) => format!("{title}\n\n{detail}"),
                None => title,
            }
        }
        Hold::Plan => str_field(&input, "plan").filter(|plan| !plan.trim().is_empty()).unwrap_or_else(|| "Plan".into()),
    };
    let id = InteractionId(format!("hold-{}", crate::auth::random_hex(16)));
    execute(domain, &state, Command::HoldHook {
        interaction_id: id.clone(), task_id: task.id.clone(), agent_id, hold, body, options: hold.options(),
    })?;
    Ok(Some(Held { id, hold, input }))
}

/// The user's answer, or `None` once the card can no longer be answered.
pub(crate) async fn wait(domain: &DomainRuntime, id: &InteractionId) -> Result<Option<Answer>> {
    let mut changed = domain.subscribe();
    loop {
        let state = domain.state()?;
        match state.interactions.get(id) {
            Some(interaction) if !interaction.withdrawn => {
                if let InteractionPayload::Question { answer: Some(answer), .. } = &interaction.payload {
                    return Ok(Some(answer.clone()));
                }
            }
            _ => return Ok(None),
        }
        if changed.changed().await.is_err() {
            return Ok(None);
        }
    }
}

/// The hook that held this card is gone: the card stops waiting for an answer.
pub(crate) fn withdraw(domain: &DomainRuntime, id: &InteractionId) -> Result<()> {
    let state = domain.state()?;
    let open = state.interactions.get(id).is_some_and(|interaction| !interaction.withdrawn
        && matches!(interaction.payload, InteractionPayload::Question { answer: None, .. }));
    if open {
        execute(domain, &state, Command::WithdrawQuestion { interaction_id: id.clone() })?;
    }
    Ok(())
}

/// No hook survives a daemon restart, so every card still held is withdrawn at start.
pub(crate) fn release_orphans(domain: &DomainRuntime) -> Result<()> {
    for id in domain.state()?.holds.into_keys() {
        withdraw(domain, &id)?;
    }
    Ok(())
}

fn execute(domain: &DomainRuntime, state: &alondra_core::domain_v2::DomainState, command: Command) -> Result<()> {
    let envelope = CommandEnvelope {
        protocol_version: PROTOCOL_VERSION, data_epoch: state.data_epoch.clone(),
        command_id: format!("hold-{}", crate::auth::random_hex(16)).into(),
        expected_revision: state.revision, command,
    };
    domain.execute(&Actor::System, &envelope, None, || Ok(())).map(|_| ()).map_err(|error| anyhow::anyhow!("{error:?}"))
}

fn note(answer: &Answer) -> Option<String> {
    answer.inputs.values().find_map(|value| match value {
        InputValue::Text(text) if !text.trim().is_empty() => Some(text.trim().to_owned()),
        _ => None,
    })
}

/// What the user wrote when turning a permission or plan down reaches the node as a message
/// of its own, after the hook's reply.
pub(crate) fn follow_up(hold: Hold, answer: &Answer) -> Option<String> {
    (!hold.approves(&answer.option_id)).then(|| note(answer)).flatten()
}

/// The hook reply Claude Code expects for the user's answer.
pub(crate) fn decision(held: &Held, answer: &Answer, user: &str) -> Value {
    let approved = held.hold.approves(&answer.option_id);
    let follows = follow_up(held.hold, answer).is_some();
    match (held.hold, approved) {
        (Hold::Permission, true) => json!({"hookSpecificOutput": {"hookEventName": "PermissionRequest",
            "decision": {"behavior": "allow"}}}),
        (Hold::Permission, false) => {
            let message = if follows { format!("{user} denied it; the message follows") } else { format!("{user} denies it") };
            json!({"hookSpecificOutput": {"hookEventName": "PermissionRequest", "decision": {"behavior": "deny", "message": message}}})
        }
        (Hold::Plan, true) => json!({"hookSpecificOutput": {"hookEventName": "PreToolUse",
            "permissionDecision": "allow", "updatedInput": held.input}}),
        (Hold::Plan, false) => {
            let reason = if follows { format!("{user} requested changes; the message follows") } else { format!("{user} requests changes to the plan") };
            json!({"hookSpecificOutput": {"hookEventName": "PreToolUse", "permissionDecision": "deny", "permissionDecisionReason": reason}})
        }
    }
}
