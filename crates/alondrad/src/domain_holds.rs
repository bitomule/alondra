//! A permission prompt, plan waiting for approval, AskUserQuestion or MCP elicitation of a
//! background Agent becomes a held card the user answers from any device. The first three are
//! held by the node's mod (`plugin/hooks/register.ts`), which posts a card under an id derived from
//! the tool call and posts it again whenever its wait is cut, so a card survives a daemon restart
//! and a repeated post finds the card already there. An MCP elicitation has no mod answer, so its
//! settings hook keeps waiting and a hook that goes away withdraws its card.
use crate::domain_runtime::DomainRuntime;
use crate::http::{elicitation_card, elicitation_content, permission_detail, permission_title, str_field, tool_summary};
use alondra_core::domain_v2::{
    Actor, AgentId, Answer, Author, Command, CommandEnvelope, DomainState, Hold, InputValue, InteractionId,
    InteractionPayload, QuestionOption, PROTOCOL_VERSION,
};
use anyhow::Result;
use serde::Deserialize;
use serde_json::{Value, json};

pub(crate) struct Held {
    pub id: InteractionId,
    pub hold: Hold,
    pub input: Value,
}

#[derive(Clone, Copy, Deserialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum Kind {
    Question,
    Plan,
    Permission,
}

/// One card a node mod puts to the user. `id` is the tool call's id, so the same call posts the
/// same card however many times the mod asks.
#[derive(Deserialize)]
pub(crate) struct Card {
    pub session_id: String,
    pub id: String,
    pub kind: Kind,
    #[serde(default)]
    pub tool: Option<String>,
    #[serde(default)]
    pub input: Value,
}

const KINDS: [&str; 3] = ["question", "plan", "permission"];

fn interaction_id(kind: &str, call: &str) -> InteractionId {
    InteractionId(format!("hold-{kind}-{call}"))
}

/// An MCP elicitation of a background session working on an active Task is held; a session in the
/// user's own terminal answers there.
pub(crate) fn hold_elicitation(domain: &DomainRuntime, payload: &Value, background: bool) -> Result<Option<Held>> {
    if str_field(payload, "hook_event_name").as_deref() != Some("Elicitation") || !background {
        return Ok(None);
    }
    let Some(session_id) = str_field(payload, "session_id") else { return Ok(None) };
    let input = payload.get("requested_schema").cloned().unwrap_or(Value::Null);
    let id = InteractionId(format!("hold-{}", crate::auth::random_hex(16)));
    open(domain, &session_id, id, Hold::Elicitation, None, input, Some(payload))
}

/// The card the mod posted, held for its session's active Task, or `None` when the session is
/// not a background one working on a Task. A card posted again is the card already open.
pub(crate) fn post(domain: &DomainRuntime, card: &Card, background: bool) -> Result<Option<InteractionId>> {
    let (hold, kind) = match card.kind {
        Kind::Question => (Hold::Question, KINDS[0]),
        Kind::Plan => (Hold::Plan, KINDS[1]),
        Kind::Permission => (Hold::Permission, KINDS[2]),
    };
    let id = interaction_id(kind, &card.id);
    if domain.state()?.interactions.contains_key(&id) {
        return Ok(Some(id));
    }
    if !background {
        return Ok(None);
    }
    Ok(open(domain, &card.session_id, id, hold, card.tool.clone(), card.input.clone(), None)?.map(|held| held.id))
}

fn open(domain: &DomainRuntime, session_id: &str, id: InteractionId, hold: Hold, tool: Option<String>, input: Value,
    elicitation: Option<&Value>) -> Result<Option<Held>> {
    let agent_id = AgentId(session_id.to_owned());
    let state = domain.state()?;
    let Some(task) = state.tasks.values()
        .find(|task| task.agent_id.as_ref() == Some(&agent_id) && !task.state.terminal())
    else {
        return Ok(None);
    };
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
            let (title, detail, card_options) = elicitation_card(elicitation.unwrap_or(&Value::Null));
            options[0].description = card_options.into_iter().next().and_then(|option| option.description);
            titled(title, detail)
        }
    };
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

/// The card stops waiting for an answer.
pub(crate) fn withdraw(domain: &DomainRuntime, id: &InteractionId) -> Result<()> {
    let state = domain.state()?;
    let open = state.interactions.get(id).is_some_and(|interaction| !interaction.withdrawn
        && matches!(interaction.payload, InteractionPayload::Question { answer: None, .. }));
    if open {
        execute(domain, &state, Command::WithdrawQuestion { interaction_id: id.clone() })?;
    }
    Ok(())
}

/// No hook survives a daemon restart, so every elicitation still held is withdrawn at start. The
/// cards the mod holds are not: their helpers dial again and find them.
pub(crate) fn release_orphans(domain: &DomainRuntime) -> Result<()> {
    for (id, hold) in domain.state()?.holds {
        if hold == Hold::Elicitation {
            withdraw(domain, &id)?;
        }
    }
    Ok(())
}

fn asked_by(state: &DomainState, id: &InteractionId, session_id: &str) -> bool {
    state.interactions.get(id)
        .is_some_and(|interaction| matches!(&interaction.author, Author::Agent { agent_id } if agent_id.0 == session_id))
}

/// The tool call that asked is over, so its cards stop waiting.
pub(crate) fn release_call(domain: &DomainRuntime, session_id: &str, call: &str) -> Result<()> {
    let state = domain.state()?;
    for kind in KINDS {
        let id = interaction_id(kind, call);
        if asked_by(&state, &id, session_id) {
            withdraw(domain, &id)?;
        }
    }
    Ok(())
}

/// The session's turn was aborted or the session ended: nothing it asked through its mod is
/// still waiting.
pub(crate) fn release_session(domain: &DomainRuntime, session_id: &str) -> Result<()> {
    let state = domain.state()?;
    for (id, hold) in &state.holds {
        if *hold != Hold::Elicitation && asked_by(&state, id, session_id) {
            withdraw(domain, id)?;
        }
    }
    Ok(())
}

fn execute(domain: &DomainRuntime, state: &DomainState, command: Command) -> Result<()> {
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

/// The answer as AskUserQuestion takes it: the chosen option's label, or the user's own words.
fn question_answer(input: &Value, answer: &Answer) -> String {
    let chosen = answer.option_id.strip_prefix('o').and_then(|index| index.parse::<usize>().ok())
        .and_then(|index| input.pointer(&format!("/questions/0/options/{index}/label")))
        .and_then(Value::as_str);
    chosen.map(str::to_owned).or_else(|| note(answer)).unwrap_or_default()
}

/// What the mod needs from the user's answer to its card. The words the user wrote when turning a
/// permission or plan down travel in the refusal, so the node reads them with the denial.
pub(crate) fn mod_reply(hold: Hold, input: &Value, answer: &Answer, user: &str) -> Value {
    let approved = hold.approves(&answer.option_id);
    let refusal = |plain: &str, with_note: &str| match note(answer) {
        Some(said) => format!("{user} {with_note}: {said}"),
        None => format!("{user} {plain}"),
    };
    match hold {
        Hold::Question => {
            let text = question_answer(input, answer);
            let answers: serde_json::Map<String, Value> = input.get("questions").and_then(Value::as_array).into_iter().flatten()
                .filter_map(|question| str_field(question, "question"))
                .map(|question| (question, Value::String(text.clone())))
                .collect();
            json!({"held": true, "answers": answers})
        }
        Hold::Plan if approved => json!({"held": true, "approved": true}),
        Hold::Plan => json!({"held": true, "approved": false,
            "feedback": refusal("requests changes to the plan", "requested changes to the plan")}),
        Hold::Permission if approved => json!({"held": true, "allowed": true, "reason": format!("{user} allowed this call")}),
        Hold::Permission => json!({"held": true, "allowed": false, "reason": refusal("denies it", "denied it")}),
        Hold::Elicitation => unreachable!("an elicitation is answered through its hook"),
    }
}

/// The hook reply Claude Code expects for the user's answer to an elicitation.
pub(crate) fn elicitation_decision(held: &Held, answer: &Answer) -> Value {
    if !held.hold.approves(&answer.option_id) {
        return json!({"hookSpecificOutput": {"hookEventName": "Elicitation", "action": "decline"}});
    }
    let mut out = json!({"hookEventName": "Elicitation", "action": "accept"});
    if held.input.is_object() {
        out["content"] = elicitation_content(&held.input, note(answer).as_deref().unwrap_or_default());
    }
    json!({"hookSpecificOutput": out})
}
