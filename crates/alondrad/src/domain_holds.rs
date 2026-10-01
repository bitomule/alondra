//! A Claude Code permission prompt, plan waiting for approval, AskUserQuestion or MCP
//! elicitation of a background Agent becomes a held card the user answers from any device. The hook keeps waiting and replies
//! with that answer, as 8ccd131 did; a hook that goes away withdraws its card.
use crate::domain_runtime::DomainRuntime;
use crate::http::{elicitation_card, elicitation_content, permission_detail, permission_title, str_field, tool_summary};
use alondra_core::domain_v2::{
    Actor, AgentId, Answer, Command, CommandEnvelope, Hold, InputValue, InteractionId, InteractionPayload, QuestionOption,
    PROTOCOL_VERSION,
};
use anyhow::Result;
use serde_json::{Value, json};

pub(crate) struct Held {
    pub id: InteractionId,
    pub hold: Hold,
    pub input: Value,
}

/// Which hooks are held, as 8ccd131 did: any permission prompt or MCP elicitation, and the
/// parent session's plan or AskUserQuestion. Only a background session working on an active
/// Task is held; a session in the user's own terminal answers there.
pub(crate) fn hold(domain: &DomainRuntime, payload: &Value, background: bool) -> Result<Option<Held>> {
    let event = str_field(payload, "hook_event_name").unwrap_or_default();
    let tool = str_field(payload, "tool_name");
    let parent = payload.get("agent_id").is_none();
    let hold = match (event.as_str(), tool.as_deref()) {
        ("PermissionRequest", _) => Hold::Permission,
        ("Elicitation", _) => Hold::Elicitation,
        ("PreToolUse", Some("ExitPlanMode")) if parent => Hold::Plan,
        ("PreToolUse", Some("AskUserQuestion")) if parent => Hold::Question,
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
    let input = payload.get(if hold == Hold::Elicitation { "requested_schema" } else { "tool_input" })
        .cloned().unwrap_or(Value::Null);
    let titled = |title: String, detail: Option<String>| match detail {
        Some(detail) => format!("{title}\n\n{detail}"),
        None => title,
    };
    let mut options = hold.options();
    let body = match hold {
        Hold::Permission => {
            let summary = tool_summary(tool.as_deref(), Some(&input));
            titled(permission_title(tool.as_deref(), &input, summary.as_deref()), permission_detail(&input, summary.as_deref()))
        }
        Hold::Plan => str_field(&input, "plan").filter(|plan| !plan.trim().is_empty()).unwrap_or_else(|| "Plan".into()),
        Hold::Question => {
            let questions = input.get("questions").and_then(Value::as_array).cloned().unwrap_or_default();
            let first = questions.first().cloned().unwrap_or(Value::Null);
            options = first.get("options").and_then(Value::as_array).into_iter().flatten().enumerate()
                .filter_map(|(index, option)| Some(QuestionOption {
                    id: format!("o{index}"),
                    label: str_field(option, "label").filter(|label| !label.trim().is_empty())?,
                    description: str_field(option, "description").filter(|text| !text.trim().is_empty()),
                    inputs: vec![],
                }))
                .collect();
            let extra: Vec<String> = questions.iter().skip(1).filter_map(|question| str_field(question, "question")).collect();
            titled(str_field(&first, "question").unwrap_or_else(|| "?".into()),
                (!extra.is_empty()).then(|| format!("- {}", extra.join("\n- "))))
        }
        Hold::Elicitation => {
            let (title, detail, card_options) = elicitation_card(payload);
            options[0].description = card_options.into_iter().next().and_then(|option| option.description);
            titled(title, detail)
        }
    };
    let id = InteractionId(format!("hold-{}", crate::auth::random_hex(16)));
    execute(domain, &state, Command::HoldHook {
        interaction_id: id.clone(), task_id: task.id.clone(), agent_id, hold, body, options,
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
    matches!(hold, Hold::Permission | Hold::Plan).then_some(())?;
    (!hold.approves(&answer.option_id)).then(|| note(answer)).flatten()
}

/// The answer as AskUserQuestion takes it: the chosen option's label, or the user's own words.
fn question_answer(input: &Value, answer: &Answer) -> String {
    let chosen = answer.option_id.strip_prefix('o').and_then(|index| index.parse::<usize>().ok())
        .and_then(|index| input.pointer(&format!("/questions/0/options/{index}/label")))
        .and_then(Value::as_str);
    chosen.map(str::to_owned).or_else(|| note(answer)).unwrap_or_default()
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
        (Hold::Question, _) => {
            let text = question_answer(&held.input, answer);
            let questions = held.input.get("questions").cloned().unwrap_or(json!([]));
            let answers: serde_json::Map<String, Value> = questions.as_array().into_iter().flatten()
                .filter_map(|question| str_field(question, "question"))
                .map(|question| (question, Value::String(text.clone())))
                .collect();
            json!({"hookSpecificOutput": {"hookEventName": "PreToolUse", "permissionDecision": "allow",
                "updatedInput": {"questions": questions, "answers": answers}}})
        }
        (Hold::Elicitation, true) => {
            let mut out = json!({"hookEventName": "Elicitation", "action": "accept"});
            if held.input.is_object() {
                out["content"] = elicitation_content(&held.input, note(answer).as_deref().unwrap_or_default());
            }
            json!({"hookSpecificOutput": out})
        }
        (Hold::Elicitation, false) => json!({"hookSpecificOutput": {"hookEventName": "Elicitation", "action": "decline"}}),
    }
}
