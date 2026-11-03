//! A permission prompt, plan waiting for approval, AskUserQuestion or MCP elicitation of a
//! background Agent becomes a held card the user answers from any device. Plans and questions are
//! held by the node's mod (`plugin/hooks/register.ts`), which posts a card under the tool call's id
//! and posts it again whenever its wait is cut, so a card survives a daemon restart and a repeated
//! post finds the card already there. A permission prompt is held by the plugin's PermissionRequest
//! hook (`alondra hook`), which only runs once Claude Code would show its own dialog, after auto
//! mode's classifier: it posts the same kind of card, dials again across a daemon restart, and its
//! card is withdrawn when the hook goes away. An MCP elicitation has no mod answer, so its
//! settings hook keeps waiting and a hook that goes away withdraws its card.
use crate::domain_runtime::DomainRuntime;
use crate::hook_text::{elicitation_card, elicitation_content, permission_detail, permission_title, str_field, tool_summary};
use alondra_core::domain::{
    clip, without_recommended_suffix, Actor, AgentId, Answer, Author, CardKind, CardSpec, Command, CommandEnvelope,
    ContentKind, DecisionPrompt, DomainState, GroupQuestion, Hold, InputValue, InteractionId, InteractionPayload,
    McpServerOrigin, QuestionGroup, QuestionOption, PROTOCOL_VERSION, plan_checklist_problem, include_linked_files,
};
use alondra_provider_contract::DecisionKind;
use alondra_core::allow_always::AllowAlways;
use anyhow::Result;
use serde::Deserialize;
use serde_json::{Value, json};
use alondra_core::cited_file::read_cited_file;
use std::path::{Path, PathBuf};

pub(crate) struct Held {
    pub id: InteractionId,
    pub hold: Hold,
    pub input: Value,
}

#[derive(Clone, Copy, PartialEq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum Kind {
    Question,
    Plan,
    Permission,
}

/// One card a node puts to the user. `id` is the tool call's id, or the hook run's own id for a
/// permission prompt, so the same call posts the same card however many times it asks.
/// `withdraw_when_gone` marks a poster whose wait is the question itself: when it goes away the
/// question was answered elsewhere or abandoned, so the card stops waiting.
#[derive(Deserialize)]
pub(crate) struct Card {
    pub session_id: String,
    pub id: String,
    pub kind: Kind,
    #[serde(default)]
    pub tool: Option<String>,
    #[serde(default)]
    pub input: Value,
    /// Claude Code's `permission_suggestions` for a permission prompt.
    #[serde(default)]
    pub suggestions: Value,
    /// Where the prompted command runs and the session's permission mode, which decide whether the
    /// card can offer «allow always».
    #[serde(default)]
    pub cwd: Option<String>,
    #[serde(default)]
    pub permission_mode: Option<String>,
    #[serde(default)]
    pub withdraw_when_gone: bool,
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
    open(domain, &session_id, id, Hold::Elicitation, None, input, None, Some(payload))
}

pub(crate) enum Posted {
    Held(InteractionId),
    /// R3: the node's Task was cancelled, so its permission or plan is denied without asking.
    Denied(String),
    NotHeld,
}

/// The card the mod posted, held for its session's active Task; a permission or plan of a node
/// whose Task was cancelled is denied, and so is a node's plan no Task can review: a node's plan
/// never falls through to the other plan hooks of the user's machine. Anything else of a session
/// that is not a background one working on a Task is not held. A card posted again is the card
/// already open.
pub(crate) fn post(domain: &DomainRuntime, card: &Card, background: bool) -> Result<Posted> {
    let (hold, kind) = match card.kind {
        Kind::Question => (Hold::Question, KINDS[0]),
        Kind::Plan => (Hold::Plan, KINDS[1]),
        Kind::Permission => (Hold::Permission, KINDS[2]),
    };
    let id = interaction_id(kind, &card.id);
    let state = domain.state()?;
    if state.interactions.contains_key(&id) {
        return Ok(Posted::Held(id));
    }
    let first = QuestionGroup::card_id(&id, 1);
    if state.interactions.contains_key(&first) {
        return Ok(Posted::Held(first));
    }
    if !background {
        return Ok(Posted::NotHeld);
    }
    if hold != Hold::Question
        && let Some(reason) = cancelled_task(&state, &card.session_id)
    {
        return Ok(Posted::Denied(reason));
    }
    let mut input = card.input.clone();
    if hold == Hold::Plan {
        let plan = str_field(&card.input, "plan").unwrap_or_default();
        if let Some((_, fix)) = plan_checklist_problem(&plan) {
            return Ok(Posted::Denied(capitalized(&fix)));
        }
        let task = state.tasks.values().find(|task| task.agent_id.as_ref().is_some_and(|a| a.0 == card.session_id) && !task.over());
        if let Some(notice) = task.and_then(|task| state.plan_while_waiting(&task.id)) {
            return Ok(Posted::Denied(notice));
        }
        match include_linked_files(&plan, |path| read_cited_file(card.cwd.as_deref().map(Path::new), path)) {
            Ok(whole) => input["plan"] = json!(whole),
            Err(fix) => return Ok(Posted::Denied(capitalized(&fix))),
        }
    }
    let allow = (hold == Hold::Permission).then(|| crate::allow_always::assess(card.tool.as_deref(), &card.input,
        card.cwd.as_deref(), card.permission_mode.as_deref(), &card.suggestions, domain.home())).flatten();
    Ok(match open(domain, &card.session_id, id, hold, card.tool.clone(), input, allow, None)? {
        Some(held) => Posted::Held(held.id),
        None if hold == Hold::Plan => Posted::Denied(NO_TASK_FOR_PLAN.into()),
        None => Posted::NotHeld,
    })
}

fn capitalized(text: &str) -> String {
    let mut chars = text.chars();
    chars.next().map(|first| first.to_uppercase().chain(chars).collect()).unwrap_or_default()
}

const NO_TASK_FOR_PLAN: &str = "No open Alondra task can review this plan: do not go on with it. Stop.";

/// Why a node's plan whose card stopped waiting is turned down rather than left to Claude Code.
pub(crate) fn plan_gone(domain: &DomainRuntime, session_id: &str) -> Result<String> {
    Ok(cancelled_task(&*domain.state()?, session_id)
        .unwrap_or_else(|| "This plan is no longer waiting for the user's review: do not go on with it.".into()))
}

/// The mod's reply to a card Alondra turns down without asking, in the shape its kind reads.
pub(crate) fn denied(kind: Kind, reason: &str) -> Value {
    match kind {
        Kind::Plan => json!({"held": true, "approved": false, "feedback": reason}),
        _ => json!({"held": true, "allowed": false, "reason": reason}),
    }
}

/// Why a node with no active Task but a cancelled one must stop instead of asking.
fn cancelled_task(state: &DomainState, session_id: &str) -> Option<String> {
    let mine = || state.tasks.values().filter(|task| task.agent_id.as_ref().is_some_and(|a| a.0 == session_id));
    if mine().any(|task| !task.over()) {
        return None;
    }
    mine().find(|task| task.cancelled())
        .map(|task| format!("Task {} was cancelled: Alondra denies this call without asking. Stop working on it.", task.id))
}

/// A label marked "(Recommended)" is the recommendation: the label loses the mark and its option
/// is the one recommended.
fn recommending(options: Vec<QuestionOption>) -> (Vec<QuestionOption>, Option<String>) {
    let mut recommended = None;
    let options = options.into_iter().map(|mut option| {
        if let Some(bare) = without_recommended_suffix(&option.label) {
            option.label = bare;
            recommended.get_or_insert_with(|| option.id.clone());
        }
        option
    }).collect();
    (options, recommended)
}

/// Claude's questions as the cards ask them. Option ids are `o` and the option's place in the
/// question Claude asked, which is how an answer finds its label again.
fn questions_of(input: &Value) -> Vec<GroupQuestion> {
    input.get("questions").and_then(Value::as_array).into_iter().flatten().map(|question| {
        let asked = str_field(question, "question").map(|text| text.trim().to_owned()).filter(|text| !text.is_empty())
            .unwrap_or_else(|| "?".into());
        let need = clip(asked.lines().map(str::trim).find(|line| !line.is_empty()).unwrap_or("?"), CardSpec::NEED_MAX);
        let options = question.get("options").and_then(Value::as_array).into_iter().flatten().enumerate()
            .filter_map(|(index, option)| Some(QuestionOption {
                id: format!("o{index}"),
                label: str_field(option, "label").filter(|label| !label.trim().is_empty())?,
                description: str_field(option, "description").filter(|text| !text.trim().is_empty()),
            }))
            .collect();
        let (options, recommended) = recommending(options);
        GroupQuestion {
            why: if asked == need { String::new() } else { clip(&asked, CardSpec::WHY_MAX) },
            need, options, recommended,
        }
    }).collect()
}

/// The card a held prompt stands for: a permission is authorized, a plan reviewed, Claude's
/// question decided.
fn hold_card(hold: Hold, body: &str, options: &[QuestionOption], recommended: Option<String>, title: Option<&str>) -> CardSpec {
    let first_line = |text: &str| text.lines().map(str::trim).find(|line| !line.is_empty()).unwrap_or_default().to_owned();
    match hold {
        Hold::Permission => {
            let need = clip(title.unwrap_or_else(|| body.lines().next().unwrap_or_default()), CardSpec::NEED_MAX);
            let card = CardSpec::new(CardKind::Authorize, need);
            match body.split_once("\n\n") {
                Some((_, command)) => card.content(ContentKind::Command, command),
                None => card,
            }
        }
        Hold::Plan => {
            let heading = body.lines().map(str::trim).find(|line| line.starts_with('#'))
                .map(|line| line.trim_start_matches('#').trim().to_owned())
                .filter(|line| !line.is_empty())
                .unwrap_or_else(|| first_line(body));
            CardSpec::new(CardKind::Review, clip(&heading, CardSpec::NEED_MAX)).content(ContentKind::Plan, body)
        }
        Hold::Question => {
            let kind = if options.len() >= 2 { CardKind::Decide } else { CardKind::Provide };
            let mut card = CardSpec::new(kind, clip(&first_line(body), CardSpec::NEED_MAX));
            card.recommended = recommended;
            card
        }
        Hold::Elicitation => CardSpec::new(CardKind::Provide, clip(&first_line(body), CardSpec::NEED_MAX)),
    }
}

fn open(domain: &DomainRuntime, session_id: &str, id: InteractionId, hold: Hold, tool: Option<String>, input: Value,
    allow: Option<AllowAlways>, elicitation: Option<&Value>) -> Result<Option<Held>> {
    let agent_id = AgentId(session_id.to_owned());
    let state = domain.state()?;
    let Some(task) = state.tasks.values()
        .find(|task| task.agent_id.as_ref() == Some(&agent_id) && !task.over())
    else {
        return Ok(None);
    };
    let titled = |title: String, detail: Option<String>| match detail {
        Some(detail) => format!("{title}\n\n{detail}"),
        None => title,
    };
    let mut options = hold.options_offering(allow.as_ref().is_some_and(|allow| allow.rules().is_some()));
    let mut title = None;
    let mut recommended = None;
    let body = match hold {
        Hold::Permission => {
            let summary = tool_summary(tool.as_deref(), Some(&input));
            let need = permission_title(tool.as_deref(), &input, summary.as_deref());
            title = Some(need.clone());
            titled(need, permission_detail(&input, summary.as_deref()))
        }
        Hold::Plan => str_field(&input, "plan").filter(|plan| !plan.trim().is_empty()).unwrap_or_else(|| "Plan".into()),
        Hold::Question => {
            let mut questions = questions_of(&input);
            if questions.len() > 1 {
                let first = QuestionGroup::card_id(&id, 1);
                execute(domain, &state, Command::HoldQuestions {
                    interaction_id: id, task_id: task.id.clone(), agent_id, questions,
                })?;
                return Ok(Some(Held { id: first, hold, input }));
            }
            match questions.pop() {
                Some(only) => {
                    options = only.options;
                    recommended = only.recommended;
                    input.pointer("/questions/0/question").and_then(Value::as_str).unwrap_or("?").to_owned()
                }
                None => "?".into(),
            }
        }
        Hold::Elicitation => {
            let (title, detail, card_options) = elicitation_card(elicitation.unwrap_or(&Value::Null));
            options[0].description = card_options.into_iter().next().and_then(|option| option.description);
            titled(title, detail)
        }
    };
    let mut card = hold_card(hold, &body, &options, recommended, title.as_deref());
    card.allow_always = allow;
    if hold == Hold::Elicitation {
        card.form = alondra_core::requested_form::RequestedForm::from_schema(&input);
    }
    execute(domain, &state, Command::HoldHook {
        interaction_id: id.clone(), task_id: task.id.clone(), agent_id, hold, body, options, card,
    })?;
    Ok(Some(Held { id, hold, input }))
}

/// What the domain is told of a decision Claude asks for through a task's SDK execution: the call
/// it is about and the card the user would answer, built exactly as a held hook's card.
pub(crate) struct SdkDecision {
    pub hold: Hold,
    pub tool: Option<String>,
    pub input: Value,
    pub mcp_server: Option<McpServerOrigin>,
    pub prompt: DecisionPrompt,
}

/// The decision the host persisted with `payload`, for a task that works in `worktree` and whose
/// open worktrees are `worktrees`: a tool call is `{tool, input, request, cwd}` with the SDK's
/// options in `request` and the folder Claude Code runs it from in `cwd`; an MCP server's request
/// for input is its form. «Allow always» is offered as for a held permission prompt, and «allow in
/// this task» for a command that stays in the task's folders.
pub(crate) fn sdk_decision(kind: DecisionKind, payload: &Value, worktree: Option<&str>, worktrees: &[String], home: &Path) -> SdkDecision {
    let hold = match kind {
        DecisionKind::Permission => Hold::Permission,
        DecisionKind::Question => Hold::Question,
        DecisionKind::Plan => Hold::Plan,
        DecisionKind::Elicitation => Hold::Elicitation,
    };
    let tool = (hold != Hold::Elicitation).then(|| str_field(payload, "tool")).flatten();
    let input = if hold == Hold::Elicitation { Value::Null } else { payload.get("input").cloned().unwrap_or(Value::Null) };
    let request = payload.get("request").unwrap_or(&Value::Null);
    let mcp_server = request.get("mcpServer").and_then(|server| Some(McpServerOrigin {
        name: str_field(server, "name")?,
        source: str_field(server, "source")?,
    }));
    let titled = |title: String, detail: Option<String>| match detail {
        Some(detail) => format!("{title}\n\n{detail}"),
        None => title,
    };
    let card = |body: String, options: Vec<QuestionOption>, recommended: Option<String>, title: Option<&str>, allow: Option<AllowAlways>| {
        let mut card = hold_card(hold, &body, &options, recommended, title);
        card.allow_always = allow;
        DecisionPrompt::Card { body, options, card }
    };
    let prompt = match hold {
        Hold::Permission => {
            let allow = crate::allow_always::assess(tool.as_deref(), &input, worktree, None, request.get("suggestions").unwrap_or(&Value::Null), home);
            let for_task = alondra_core::allow_for_task::assess(&alondra_core::allow_for_task::Prompt {
                tool: tool.as_deref(),
                input: &input,
                cwd: payload.get("cwd").and_then(Value::as_str),
                persistent_refused: request.get("suppressAlwaysAllowRule").and_then(Value::as_bool).unwrap_or(false),
                ask_rule: request.get("matchedAskRule").is_some_and(|rule| !rule.is_null()),
            }, &task_folders(worktree.into_iter().chain(worktrees.iter().map(String::as_str)), home));
            let summary = tool_summary(tool.as_deref(), Some(&input));
            let need = permission_title(tool.as_deref(), &input, summary.as_deref());
            let body = titled(need.clone(), permission_detail(&input, summary.as_deref()));
            let options = hold.permission_offering(allow.as_ref().is_some_and(|allow| allow.rules().is_some()), for_task.offered());
            let mut prompt = card(body, options, None, Some(&need), allow);
            if let DecisionPrompt::Card { card, .. } = &mut prompt {
                card.allow_for_task = Some(for_task);
            }
            prompt
        }
        Hold::Plan => {
            let plan = str_field(&input, "plan").unwrap_or_default();
            let whole = include_linked_files(&plan, |path| read_cited_file(worktree.map(Path::new), path)).unwrap_or(plan);
            card(whole, hold.options_offering(false), None, None, None)
        }
        Hold::Question => {
            let mut questions = questions_of(&input);
            match questions.len() {
                0 => card("?".into(), vec![], None, None, None),
                1 => {
                    let only = questions.pop().expect("one question");
                    let body = input.pointer("/questions/0/question").and_then(Value::as_str).unwrap_or("?").to_owned();
                    card(body, only.options, only.recommended, None, None)
                }
                _ => DecisionPrompt::Questions { questions },
            }
        }
        Hold::Elicitation => {
            let form = json!({
                "mcp_server_name": payload.get("serverName"), "message": payload.get("message"),
                "requested_schema": payload.get("requestedSchema"), "url": payload.get("url"),
            });
            let (title, detail, card_options) = elicitation_card(&form);
            let mut options = hold.options_offering(false);
            options[0].description = card_options.into_iter().next().and_then(|option| option.description);
            let mut prompt = card(titled(title, detail), options, None, None, None);
            if let DecisionPrompt::Card { card, .. } = &mut prompt {
                card.form = alondra_core::requested_form::RequestedForm::from_schema(payload.get("requestedSchema").unwrap_or(&Value::Null));
            }
            prompt
        }
    };
    SdkDecision { hold, tool, input, mcp_server, prompt }
}

/// The folders a task's commands may stay in for «allow in this task»: its worktrees and Alondra's
/// files folder, each as written and as the disk resolves it.
fn task_folders<'a>(worktrees: impl Iterator<Item = &'a str>, home: &Path) -> Vec<PathBuf> {
    let mut folders = vec![];
    for folder in worktrees.map(PathBuf::from).chain([home.join("files")]) {
        if let Ok(real) = std::fs::canonicalize(&folder) {
            folders.push(real);
        }
        folders.push(folder);
    }
    folders.sort();
    folders.dedup();
    folders
}

/// A card Alondra itself puts to the user about the session's open task, with the options it
/// gives; `None` when the session has no open task.
pub(crate) fn ask(domain: &DomainRuntime, session_id: &str, body: String, options: Vec<QuestionOption>) -> Result<Option<InteractionId>> {
    let agent_id = AgentId(session_id.to_owned());
    let state = domain.state()?;
    let Some(task) = state.tasks.values().find(|task| task.agent_id.as_ref() == Some(&agent_id) && !task.over()) else {
        return Ok(None);
    };
    let id = InteractionId(format!("hold-{}", crate::auth::random_hex(16)));
    let (options, recommended) = recommending(options);
    let card = hold_card(Hold::Question, &body, &options, recommended, None);
    execute(domain, &state, Command::HoldHook {
        interaction_id: id.clone(), task_id: task.id.clone(), agent_id, hold: Hold::Question, body, options, card,
    })?;
    Ok(Some(id))
}

/// The user's answers, one per question and in the order asked, or `None` once the card can no
/// longer be answered. Questions asked together are given only when every one is answered, so
/// the node never gets part of them; an answer taken back makes this wait again.
pub(crate) async fn wait(domain: &DomainRuntime, id: &InteractionId) -> Result<Option<Vec<Answer>>> {
    let mut changed = domain.subscribe();
    loop {
        let state = domain.state()?;
        let Some(interaction) = state.interactions.get(id) else { return Ok(None) };
        let members = state.group_members(interaction);
        if members.iter().any(|member| member.withdrawn) {
            return Ok(None);
        }
        let answers: Vec<Answer> = members.iter().filter_map(|member| match &member.payload {
            InteractionPayload::Question { answer: Some(answer), .. } => Some(answer.clone()),
            _ => None,
        }).collect();
        if answers.len() == members.len() {
            return Ok(Some(answers));
        }
        if changed.changed().await.is_err() {
            return Ok(None);
        }
    }
}

/// The card stops waiting for an answer; so do the questions asked together with it, answered
/// or not, since the node will not be given any of the answers.
pub(crate) fn withdraw(domain: &DomainRuntime, id: &InteractionId) -> Result<()> {
    let state = domain.state()?;
    let Some(interaction) = state.interactions.get(id) else { return Ok(()) };
    let open = state.group_members(interaction).into_iter().find(|member| !member.withdrawn
        && matches!(member.payload, InteractionPayload::Question { answer: None, .. }));
    if let Some(open) = open {
        execute(domain, &state, Command::Withdraw { interaction_id: open.id.clone() })?;
    }
    Ok(())
}

/// No hook survives a daemon restart, so every elicitation still held is withdrawn at start. The
/// cards the mod holds are not: their helpers dial again and find them.
pub(crate) fn release_orphans(domain: &DomainRuntime) -> Result<()> {
    for (id, hold) in &domain.state()?.holds {
        if *hold == Hold::Elicitation {
            withdraw(domain, id)?;
        }
    }
    Ok(())
}

fn asked_by(state: &DomainState, id: &InteractionId, session_id: &str) -> bool {
    state.interactions.get(id)
        .is_some_and(|interaction| matches!(&interaction.author, Author::Agent { agent_id } if agent_id.0 == session_id))
}

/// The session has a held card still open: it waits for the user, however silent it is. Read
/// from the domain, so it holds across a daemon restart that forgot which call the session is in.
pub(crate) fn waits_on_user(state: &DomainState, session_id: &str) -> bool {
    state.holds.keys().any(|id| asked_by(state, id, session_id) && state.interactions.get(id).is_some_and(|interaction| {
        !interaction.withdrawn && matches!(interaction.payload, InteractionPayload::Question { answer: None, .. })
    }))
}

/// The tool call that asked is over, so its cards stop waiting.
pub(crate) fn release_call(domain: &DomainRuntime, session_id: &str, call: &str) -> Result<()> {
    let state = domain.state()?;
    for kind in KINDS {
        let id = interaction_id(kind, call);
        for id in [QuestionGroup::card_id(&id, 1), id] {
            if asked_by(&state, &id, session_id) {
                withdraw(domain, &id)?;
            }
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
    domain.execute_as_system(envelope).map(|_| ()).map_err(|error| anyhow::anyhow!("{error:?}"))
}

fn note(answer: &Answer) -> Option<String> {
    answer.inputs.values().find_map(|value| match value {
        InputValue::Text(text) if !text.trim().is_empty() => Some(text.trim().to_owned()),
        _ => None,
    })
}

/// What AskUserQuestion takes as the answer to question `question`: the label of the option chosen
/// as Claude wrote it, or the user's own words.
fn question_answer(input: &Value, question: usize, answer: &Answer) -> String {
    let chosen = answer.option_id.strip_prefix('o').and_then(|index| index.parse::<usize>().ok())
        .and_then(|index| input.pointer(&format!("/questions/{question}/options/{index}/label")))
        .and_then(Value::as_str);
    chosen.map(str::to_owned).or_else(|| note(answer)).unwrap_or_default()
}

/// What the mod needs from the user's answer to its card. The words the user wrote when turning a
/// permission or plan down travel in the refusal, so the node reads them with the denial.
pub(crate) fn mod_reply(hold: Hold, input: &Value, allow: Option<&AllowAlways>, answers: &[Answer], user: &str) -> Value {
    let answer = &answers[0];
    let approved = hold.approves(&answer.option_id);
    let refusal = |plain: &str, with_note: &str| match note(answer) {
        Some(said) => format!("{user} {with_note}: {said}"),
        None => format!("{user} {plain}"),
    };
    match hold {
        Hold::Question => {
            let asked = input.get("questions").and_then(Value::as_array).map(Vec::as_slice).unwrap_or_default();
            let said: serde_json::Map<String, Value> = asked.iter().zip(answers).enumerate()
                .filter_map(|(index, (question, answer))| Some((str_field(question, "question")?, Value::String(question_answer(input, index, answer)))))
                .collect();
            json!({"held": true, "answers": said})
        }
        Hold::Plan if approved => json!({"held": true, "approved": true}),
        Hold::Plan => json!({"held": true, "approved": false,
            "feedback": refusal("requests changes to the plan", "requested changes to the plan")}),
        Hold::Permission if answer.option_id == Hold::ALWAYS => match allow.and_then(AllowAlways::rules) {
            Some(rules) => json!({"held": true, "allowed": true, "reason": format!("{user} allowed this call and similar ones"),
                "permissions": [alondra_core::allow_always::permission_update(rules)]}),
            None => json!({"held": true, "allowed": true, "reason": format!("{user} allowed this call")}),
        },
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
        out["content"] = elicitation_content(&held.input, note(answer).as_deref().unwrap_or_default(), answer.form_values());
    }
    json!({"hookSpecificOutput": out})
}
