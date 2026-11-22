//! What an Antigravity host's observations mean for the task. The host forwards every ACP
//! `session/update` as it came and marks the life of each `session/prompt` itself
//! (`alondra/promptSent`, `alondra/promptEnded`, `alondra/promptFailed`), since ACP names no turn:
//! a prompt is a turn, open from its request to its answer.
use crate::records::{InputRecord, SessionRecord};
use alondra_agent_application::{Normalization, ObservationMetadata, ObservationNormalizer};
use alondra_antigravity_protocol::{Body, DecisionKind, DecisionOutcome, Fence, FramedReader, Observation as WireObservation};
use alondra_foundation::{InputId, MessageUuid, TurnId};
use alondra_provider_contract::{runs_gh_pr_close, AdapterError, ApiFailure, Observation, OrderedObservation, TurnOutcome};
use alondra_store::{ContextSource, TaskContextItem};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
use std::io::Cursor;

pub(super) struct Normalizer {
    pub session: Option<SessionRecord>,
    pub fence: Option<Fence>,
    pub first_cursor: Option<u64>,
    pub host_ended: bool,
}

/// The most of a turn's answer kept for the task's context.
const MAX_ANSWER_BYTES: usize = 64 * 1024;
/// Session updates that describe the session, not the work of a turn: they arrive outside any
/// prompt too, after a session opens or a setting changes.
const SESSION_UPDATES: &[&str] = &["available_commands_update", "config_option_update", "current_mode_update", "session_info_update", "usage_update"];

#[derive(Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Checkpoint {
    cursor: u64,
    initialized: bool,
    #[serde(default)]
    process: String,
    /// The ACP session the host runs. A host that could not resume opens another one.
    #[serde(default)]
    session: Option<String>,
    #[serde(default)]
    model: Option<String>,
    #[serde(default)]
    cwd: Option<String>,
    #[serde(default)]
    prompt: Option<Prompt>,
    /// A turn Antigravity opened with no prompt of Alondra's, such as a scheduled instruction.
    #[serde(default)]
    unprompted: Option<String>,
    #[serde(default)]
    unprompted_turns: u64,
    #[serde(default)]
    asked: BTreeMap<u64, String>,
    /// Tool call ids of the open turn's commands that close a pull request.
    #[serde(default)]
    closing: BTreeSet<String>,
    #[serde(default)]
    answered: bool,
    #[serde(default)]
    answer: String,
    #[serde(default)]
    blocked_policy: Option<String>,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Prompt {
    turn: String,
    input: String,
    started: bool,
}

pub fn blocked_policy(bytes: &[u8]) -> Option<String> {
    serde_json::from_slice::<Checkpoint>(bytes).ok()?.blocked_policy.filter(|reason| !reason.is_empty() && reason.len() <= 1024)
}
fn invalid<T>() -> Result<T, AdapterError> {
    Err(AdapterError::InvalidObservation)
}
fn storage(error: impl std::fmt::Debug) -> AdapterError {
    AdapterError::Storage(format!("{error:?}"))
}
fn string<'a>(value: &'a Value, key: &str) -> Option<&'a str> {
    value.get(key).and_then(Value::as_str)
}
fn scoped(batch: &OrderedObservation, process: &str, kind: &str, id: &str) -> String {
    format!("antigravity:{}:{process}:{kind}:{id}", batch.execution.generation())
}
fn native_turn(batch: &OrderedObservation, process: &str, id: &str) -> Result<TurnId, AdapterError> {
    TurnId::new(scoped(batch, process, "turn", id)).map_err(|_| AdapterError::InvalidObservation)
}
fn lookup(metadata: &ObservationMetadata<'_, '_>, id: &str) -> Result<InputId, AdapterError> {
    let message_id = MessageUuid::new(id).map_err(|_| AdapterError::InvalidObservation)?;
    let bytes = serde_json::to_vec(&InputRecord { message_id }).map_err(|_| AdapterError::InvalidObservation)?;
    let records = metadata.find_inputs(&[bytes]).map_err(storage)?.ok_or(AdapterError::UnknownInput)?;
    if records.len() != 1 {
        return invalid();
    }
    Ok(records[0].input().clone())
}
fn prior_checkpoint(batch: &OrderedObservation, metadata: &ObservationMetadata<'_, '_>) -> Result<Checkpoint, AdapterError> {
    match metadata.checkpoint().map_err(storage)? {
        Some(prior) => {
            let state: Checkpoint = serde_json::from_slice(prior.bytes()).map_err(|_| AdapterError::InvalidObservation)?;
            if state.cursor != prior.cursor() || state.cursor.checked_add(1) != Some(batch.cursor) {
                return invalid();
            }
            Ok(state)
        }
        None if batch.cursor == 1 => Ok(Checkpoint::default()),
        None => invalid(),
    }
}
impl Checkpoint {
    /// A new process or session carries nothing of the turn the last one had open.
    fn forget_turn(&mut self) {
        self.prompt = None;
        self.unprompted = None;
        self.closing.clear();
        self.answered = false;
        self.answer.clear();
    }
}

impl ObservationNormalizer for Normalizer {
    fn skip(&self, batch: &OrderedObservation, metadata: &ObservationMetadata<'_, '_>) -> Result<Normalization, AdapterError> {
        if &batch.execution != metadata.execution() || batch.cursor != metadata.cursor() {
            return invalid();
        }
        let mut state = prior_checkpoint(batch, metadata)?;
        state.cursor = batch.cursor;
        Normalization::new(Vec::new(), serde_json::to_vec(&state).map_err(|_| AdapterError::InvalidObservation)?)
    }
    fn normalize(&self, batch: &OrderedObservation, metadata: &ObservationMetadata<'_, '_>) -> Result<Normalization, AdapterError> {
        if &batch.execution != metadata.execution() || batch.cursor != metadata.cursor() || self.session.is_none() || batch.evidence.is_empty() || batch.evidence.len() > alondra_antigravity_protocol::MAX_FRAME_BYTES {
            return invalid();
        }
        let mut framed = (batch.evidence.len() as u32).to_be_bytes().to_vec();
        framed.extend_from_slice(&batch.evidence);
        let received = FramedReader::new(Cursor::new(framed)).read_received().map_err(|_| AdapterError::InvalidObservation)?.ok_or(AdapterError::InvalidObservation)?;
        let Body::Event(event) = &received.frame().body else { return invalid() };
        let alondra_antigravity_protocol::Execution::Task(execution) = &event.execution else { return invalid() };
        if event.cursor != batch.cursor || self.fence.as_ref() != Some(&event.fence)
            || execution.task_id != *batch.execution.task() || execution.agent_id != *batch.execution.node()
            || execution.conversation_id != batch.execution.conversation().as_str()
            || execution.process_instance_id != batch.execution.instance().as_str()
            || execution.generation != batch.execution.generation() || event.fence.host_id != batch.execution.host().as_str()
        {
            return invalid();
        }
        event.validate().map_err(|_| AdapterError::InvalidObservation)?;
        let mut state = prior_checkpoint(batch, metadata)?;
        if self.first_cursor == Some(batch.cursor) {
            state.forget_turn();
        }
        let mut facts = Vec::new();
        let mut context = Vec::new();
        match &event.observation {
            WireObservation::DecisionRequested { request_id, tool_use_id, kind, payload } => {
                if state.asked.len() >= 64 || state.asked.values().any(|id| id == &request_id.0) {
                    return invalid();
                }
                state.asked.insert(batch.cursor, request_id.0.clone());
                facts.push(Observation::Asked {
                    request: request_id.0.clone(),
                    epoch: batch.cursor,
                    tool_use: tool_use_id.clone(),
                    kind: match kind {
                        DecisionKind::Permission => alondra_provider_contract::DecisionKind::Permission,
                        DecisionKind::Question => alondra_provider_contract::DecisionKind::Question,
                        DecisionKind::Plan | DecisionKind::Elicitation => return invalid(),
                    },
                    payload: serde_json::to_vec(payload).map_err(|_| AdapterError::InvalidObservation)?,
                });
            }
            WireObservation::DecisionSettled { request_id, epoch, outcome } => {
                if state.asked.get(epoch) != Some(&request_id.0) {
                    return invalid();
                }
                state.asked.remove(epoch);
                facts.push(Observation::DecisionSettled { epoch: *epoch, answered: *outcome == DecisionOutcome::Answered, continued: state.prompt.is_some() || state.unprompted.is_some() });
            }
            WireObservation::Sdk { payload, .. } => {
                let method = string(payload, "method").unwrap_or("");
                let params = payload.get("params").unwrap_or(&Value::Null);
                match method {
                    "alondra/sessionReady" => {
                        let session = string(params, "sessionId").filter(|id| !id.is_empty() && id.len() <= 256).ok_or(AdapterError::InvalidObservation)?;
                        let process = string(params, "processGeneration").ok_or(AdapterError::InvalidObservation)?;
                        if state.initialized && (state.process != process || state.session.as_deref() != Some(session)) {
                            state.forget_turn();
                            state.asked.clear();
                        }
                        state.session = Some(session.to_owned());
                        state.process = process.to_owned();
                        state.model = string(params, "model").filter(|model| !model.is_empty()).map(str::to_owned);
                        state.cwd = string(params, "cwd").map(str::to_owned);
                        state.blocked_policy = None;
                        if !state.initialized {
                            state.initialized = true;
                            facts.push(Observation::Available);
                        }
                    }
                    "alondra/transportFailed" if !self.host_ended => facts.push(Observation::TransportLost),
                    "alondra/inputRefused" if params.get("nativeSubmitted").and_then(Value::as_bool) == Some(false) => {
                        let input = string(params, "inputId").ok_or(AdapterError::InvalidObservation)?;
                        facts.push(Observation::InputDropped { input: lookup(metadata, input)? });
                    }
                    _ if state.session.is_some() && string(params, "sessionId") == state.session.as_deref() => {
                        fold(batch, &mut state, method, params, &mut facts, &mut context, |id| lookup(metadata, id))?;
                    }
                    _ => {}
                }
            }
            WireObservation::ProcessExited { .. } if !self.host_ended => facts.push(Observation::ResourceReleased),
            _ => {}
        }
        if let Some(reason) = crate::context::account_refusal(batch) {
            state.blocked_policy = Some(reason.to_owned());
        }
        state.cursor = batch.cursor;
        let bytes = serde_json::to_vec(&state).map_err(|_| AdapterError::InvalidObservation)?;
        Normalization::new(facts, bytes)?.with_context(context)
    }
}

fn fold(
    batch: &OrderedObservation,
    state: &mut Checkpoint,
    method: &str,
    params: &Value,
    facts: &mut Vec<Observation>,
    context: &mut Vec<TaskContextItem>,
    lookup: impl Fn(&str) -> Result<InputId, AdapterError>,
) -> Result<(), AdapterError> {
    match method {
        // The host wrote the input's `session/prompt` to agy: agy holds it.
        "alondra/promptSent" => {
            let input = string(params, "inputId").filter(|id| !id.is_empty() && id.len() <= 256).ok_or(AdapterError::InvalidObservation)?;
            let turn = string(params, "turnId").filter(|id| !id.is_empty() && id.len() <= 256).ok_or(AdapterError::InvalidObservation)?;
            if state.prompt.is_some() {
                return invalid();
            }
            end_unprompted(batch, state, facts, context)?;
            match lookup(input) {
                Ok(input) => facts.push(Observation::InputHeld { input }),
                Err(AdapterError::UnknownInput) => {}
                Err(error) => return Err(error),
            }
            state.prompt = Some(Prompt { turn: turn.into(), input: input.into(), started: false });
        }
        // agy answered the prompt with a stop reason: it took the input, whether or not it said
        // anything first.
        "alondra/promptEnded" => {
            let Some(prompt) = answered_prompt(state, params)? else { return Ok(()) };
            start_turn(batch, state, &prompt, facts, &lookup)?;
            let outcome = match string(params, "stopReason") {
                Some("end_turn") => TurnOutcome::Completed,
                Some("cancelled") => TurnOutcome::Failed { code: "interrupted".into() },
                Some(reason) if !reason.is_empty() && reason.len() <= 64 => TurnOutcome::Failed { code: reason.into() },
                _ => return invalid(),
            };
            end_turn(batch, state, &prompt.turn, outcome, facts, context)?;
        }
        // agy answered the prompt with a JSON-RPC error: before any update for it, it never took
        // the input; after, the turn it ran failed.
        "alondra/promptFailed" => {
            let Some(prompt) = answered_prompt(state, params)? else { return Ok(()) };
            let error = params.get("error").ok_or(AdapterError::InvalidObservation)?;
            if prompt.started {
                end_turn(batch, state, &prompt.turn, failed_outcome(error)?, facts, context)?;
            } else {
                state.forget_turn();
                match lookup(&prompt.input) {
                    Ok(input) => facts.push(Observation::InputDropped { input }),
                    Err(AdapterError::UnknownInput) => {}
                    Err(error) => return Err(error),
                }
                facts.push(Observation::Reconciled { idle: true, live: Default::default() });
            }
        }
        "session/update" => {
            let update = params.get("update").unwrap_or(&Value::Null);
            let kind = string(update, "sessionUpdate").unwrap_or("");
            if SESSION_UPDATES.contains(&kind) {
                // agy reports the context it used once a model call is done: the last word of a
                // turn it opened by itself, which no prompt answers.
                if kind == "usage_update" && update.get("used").and_then(Value::as_u64).is_some_and(|used| used > 0) {
                    end_unprompted(batch, state, facts, context)?;
                }
                return Ok(());
            }
            match state.prompt.clone() {
                Some(prompt) => start_turn(batch, state, &prompt, facts, &lookup)?,
                None if state.unprompted.is_none() => {
                    state.unprompted_turns += 1;
                    let turn = format!("unprompted-{}", state.unprompted_turns);
                    facts.push(Observation::TurnUnprompted { turn: native_turn(batch, &state.process, &turn)? });
                    state.unprompted = Some(turn);
                }
                None => {}
            }
            work(state, kind, update, facts, context, batch)?;
        }
        _ => {}
    }
    Ok(())
}

/// The open prompt `params` answers, taken out of the checkpoint; `None` for the answer of a prompt
/// a host before a restart sent.
fn answered_prompt(state: &mut Checkpoint, params: &Value) -> Result<Option<Prompt>, AdapterError> {
    let turn = string(params, "turnId").ok_or(AdapterError::InvalidObservation)?;
    match &state.prompt {
        Some(prompt) if prompt.turn == turn => Ok(state.prompt.clone()),
        Some(_) => invalid(),
        None => Ok(None),
    }
}

/// The prompt's first word from agy opens its turn, which takes its input.
fn start_turn(batch: &OrderedObservation, state: &mut Checkpoint, prompt: &Prompt, facts: &mut Vec<Observation>, lookup: &impl Fn(&str) -> Result<InputId, AdapterError>) -> Result<(), AdapterError> {
    if prompt.started {
        return Ok(());
    }
    let turn = native_turn(batch, &state.process, &prompt.turn)?;
    match lookup(&prompt.input) {
        Ok(input) => facts.push(Observation::TurnStarted { input, turn }),
        Err(AdapterError::UnknownInput) => facts.push(Observation::TurnUnprompted { turn }),
        Err(error) => return Err(error),
    }
    if let Some(open) = state.prompt.as_mut() {
        open.started = true;
    }
    Ok(())
}

/// What an update of a turn says: the model's first words, and the pull requests its commands open
/// or close.
fn work(state: &mut Checkpoint, kind: &str, update: &Value, facts: &mut Vec<Observation>, context: &mut Vec<TaskContextItem>, batch: &OrderedObservation) -> Result<(), AdapterError> {
    match kind {
        "agent_message_chunk" => {
            if let Some(text) = update.pointer("/content/text").and_then(Value::as_str) {
                if !state.answered {
                    state.answered = true;
                    facts.push(Observation::ModelAnswered);
                }
                let room = MAX_ANSWER_BYTES.saturating_sub(state.answer.len());
                let mut end = room.min(text.len());
                while !text.is_char_boundary(end) {
                    end -= 1;
                }
                state.answer.push_str(&text[..end]);
            }
        }
        "tool_call" | "tool_call_update" => {
            let Some(id) = string(update, "toolCallId").filter(|id| !id.is_empty() && id.len() <= 256) else { return Ok(()) };
            if string(update, "kind") == Some("execute") && !state.closing.contains(id) && state.closing.len() < 64 {
                let command = update.pointer("/rawInput/CommandLine").or_else(|| update.pointer("/rawInput/command")).and_then(Value::as_str).unwrap_or("");
                if runs_gh_pr_close(command) {
                    state.closing.insert(id.into());
                }
            }
            if string(update, "status") == Some("completed") {
                let output = command_output(update);
                if state.closing.remove(id) && update.pointer("/rawOutput/exit_code").and_then(Value::as_i64) == Some(0) {
                    facts.push(Observation::PullRequestsChanged);
                }
                if !output.is_empty() {
                    let identity = alondra_store::native_context_id(&format!("{}:{}:tool:{id}", scoped(batch, &state.process, "session", state.session.as_deref().unwrap_or("")), current_turn(state)));
                    let exit = update.pointer("/rawOutput/exit_code").and_then(Value::as_i64);
                    context.push(TaskContextItem::tool_result(identity, output, exit, None).map_err(storage)?);
                }
            }
        }
        _ => {}
    }
    Ok(())
}

fn current_turn(state: &Checkpoint) -> &str {
    state.prompt.as_ref().map(|prompt| prompt.turn.as_str()).or(state.unprompted.as_deref()).unwrap_or("none")
}

/// What a finished tool call printed: its raw output, or the text of its content.
fn command_output(update: &Value) -> String {
    if let Some(output) = update.pointer("/rawOutput/output").and_then(Value::as_str) {
        return output.to_owned();
    }
    update.get("content").and_then(Value::as_array).into_iter().flatten()
        .filter_map(|content| content.pointer("/content/text").and_then(Value::as_str))
        .collect::<Vec<_>>()
        .join("\n")
}

/// Ends the turn with `outcome`: nothing of it runs on, since ACP keeps no work past a prompt.
fn end_turn(batch: &OrderedObservation, state: &mut Checkpoint, turn: &str, outcome: TurnOutcome, facts: &mut Vec<Observation>, context: &mut Vec<TaskContextItem>) -> Result<(), AdapterError> {
    let answer = std::mem::take(&mut state.answer);
    if !answer.is_empty() {
        let identity = alondra_store::native_context_id(&format!("{}:{turn}:answer", scoped(batch, &state.process, "session", state.session.as_deref().unwrap_or(""))));
        context.push(TaskContextItem::text(identity, ContextSource::Assistant, answer).map_err(storage)?);
    }
    state.forget_turn();
    facts.push(Observation::TurnEnded { turn: native_turn(batch, &state.process, turn)?, outcome });
    facts.push(Observation::Reconciled { idle: true, live: Default::default() });
    Ok(())
}

fn end_unprompted(batch: &OrderedObservation, state: &mut Checkpoint, facts: &mut Vec<Observation>, context: &mut Vec<TaskContextItem>) -> Result<(), AdapterError> {
    match state.unprompted.clone() {
        Some(turn) => end_turn(batch, state, &turn, TurnOutcome::Completed, facts, context),
        None => Ok(()),
    }
}

/// A JSON-RPC error answering a prompt. Its `data`, when it carries the upstream call's status in
/// Google's error shape (`code` an HTTP status, `status` a gRPC status), says the API failed and
/// how; otherwise the failure is named by the JSON-RPC code. Never by the error's text.
fn failed_outcome(error: &Value) -> Result<TurnOutcome, AdapterError> {
    let code = error.get("code").and_then(Value::as_i64).ok_or(AdapterError::InvalidObservation)?;
    let data = error.get("data").filter(|data| data.is_object());
    let http = data.and_then(|data| data.get("code")).and_then(Value::as_u64).and_then(|status| u16::try_from(status).ok());
    let grpc = data.and_then(|data| string(data, "status"));
    let api = |error: &str, status: Option<u16>| TurnOutcome::ApiFailed(ApiFailure { error: error.into(), status, resets_at: None, ..Default::default() });
    Ok(match (http, grpc) {
        (Some(429), _) | (_, Some("RESOURCE_EXHAUSTED")) => api("rate_limit", Some(429)),
        (Some(401), _) | (_, Some("UNAUTHENTICATED")) => api("authentication_failed", Some(401)),
        (Some(403), _) | (_, Some("PERMISSION_DENIED")) => api("permission_error", Some(403)),
        (Some(status @ 500..=599), _) => api("api_error", Some(status)),
        _ => TurnOutcome::Failed { code: format!("agy_{code}") },
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use alondra_foundation::{AgentId, ConversationId, ExecutionStamp, HostId, InstanceId, TaskId};
    use serde_json::json;

    fn batch() -> OrderedObservation {
        OrderedObservation { owner_generation: 1, execution: ExecutionStamp::new(TaskId("task".into()), AgentId("agent".into()), ConversationId::new("conversation").unwrap(), InstanceId::new("instance").unwrap(), HostId::new("alondra-sdk").unwrap(), 1).unwrap(), cursor: 1, evidence: vec![1] }
    }
    fn input(id: &str) -> Result<InputId, AdapterError> {
        match id {
            "60000000-0000-4000-8000-000000000001" => Ok(InputId::new("input-1").unwrap()),
            _ => Err(AdapterError::UnknownInput),
        }
    }
    const INPUT: &str = "60000000-0000-4000-8000-000000000001";
    fn session() -> Checkpoint {
        Checkpoint { initialized: true, session: Some("session-1".into()), process: "process-1".into(), ..Checkpoint::default() }
    }
    fn step(state: &mut Checkpoint, method: &str, params: Value) -> Vec<String> {
        let mut facts = vec![];
        let mut context = vec![];
        fold(&batch(), state, method, &params, &mut facts, &mut context, input).unwrap();
        facts.iter().map(|fact| format!("{fact:?}")).map(|fact| fact.split([' ', '{', '(']).next().unwrap().to_owned()).collect()
    }
    fn update(update: Value) -> Value {
        json!({"sessionId": "session-1", "update": update})
    }
    fn sent(state: &mut Checkpoint) {
        assert_eq!(step(state, "alondra/promptSent", json!({"sessionId": "session-1", "inputId": INPUT, "turnId": "alondra:7"})), ["InputHeld"]);
    }
    fn ended(state: &mut Checkpoint, reason: &str) -> Vec<Observation> {
        let mut facts = vec![];
        fold(&batch(), state, "alondra/promptEnded", &json!({"sessionId": "session-1", "inputId": INPUT, "turnId": "alondra:7", "stopReason": reason}), &mut facts, &mut vec![], input).unwrap();
        facts
    }

    #[test]
    fn each_session_update_kind_of_a_turn() {
        let mut state = session();
        sent(&mut state);
        assert!(step(&mut state, "session/update", update(json!({"sessionUpdate": "usage_update", "used": 0, "size": 1048576}))).is_empty(), "a session update opens no turn");
        assert_eq!(step(&mut state, "session/update", update(json!({"sessionUpdate": "agent_thought_chunk", "content": {"type": "text", "text": "Thinking"}}))), ["TurnStarted"]);
        assert!(step(&mut state, "session/update", update(json!({"sessionUpdate": "plan", "entries": []}))).is_empty());
        assert!(step(&mut state, "session/update", update(json!({"sessionUpdate": "tool_call", "toolCallId": "call_1", "kind": "read", "status": "pending"}))).is_empty());
        assert_eq!(step(&mut state, "session/update", update(json!({"sessionUpdate": "agent_message_chunk", "content": {"type": "text", "text": "Done"}}))), ["ModelAnswered"]);
        assert!(step(&mut state, "session/update", update(json!({"sessionUpdate": "agent_message_chunk", "content": {"type": "text", "text": " now."}}))).is_empty(), "ModelAnswered once per turn");
        assert!(step(&mut state, "session/update", update(json!({"sessionUpdate": "session_info_update", "title": "Task"}))).is_empty());
        assert_eq!(state.answer, "Done now.");
        let facts = ended(&mut state, "end_turn");
        assert!(matches!(&facts[..], [Observation::TurnEnded { outcome: TurnOutcome::Completed, .. }, Observation::Reconciled { idle: true, live }] if live.is_empty()));
        sent(&mut state);
        assert_eq!(step(&mut state, "session/update", update(json!({"sessionUpdate": "agent_message_chunk", "content": {"type": "text", "text": "Again"}}))), ["TurnStarted", "ModelAnswered"], "the next turn answers again");
    }

    #[test]
    fn each_stop_reason_ends_its_turn() {
        for (reason, outcome) in [
            ("end_turn", TurnOutcome::Completed),
            ("cancelled", TurnOutcome::Failed { code: "interrupted".into() }),
            ("max_tokens", TurnOutcome::Failed { code: "max_tokens".into() }),
            ("max_turn_requests", TurnOutcome::Failed { code: "max_turn_requests".into() }),
            ("refusal", TurnOutcome::Failed { code: "refusal".into() }),
        ] {
            let mut state = session();
            sent(&mut state);
            let facts = ended(&mut state, reason);
            assert!(matches!(&facts[..], [Observation::TurnStarted { .. }, Observation::TurnEnded { outcome: ended, .. }, Observation::Reconciled { .. }] if *ended == outcome), "{reason}: {facts:?}");
            assert!(state.prompt.is_none());
        }
    }

    #[test]
    fn a_prompt_answered_with_an_error_before_any_update_drops_its_input() {
        let mut state = session();
        sent(&mut state);
        assert_eq!(step(&mut state, "alondra/promptFailed", json!({"sessionId": "session-1", "inputId": INPUT, "turnId": "alondra:7", "error": {"code": -32603}})), ["InputDropped", "Reconciled"]);
        assert!(state.prompt.is_none());
    }

    #[test]
    fn errors_with_and_without_structured_data() {
        let outcome = |error: Value| failed_outcome(&error).unwrap();
        let api = |error: &str, status: u16| TurnOutcome::ApiFailed(ApiFailure { error: error.into(), status: Some(status), resets_at: None, ..Default::default() });
        assert_eq!(outcome(json!({"code": -32603, "data": {"code": 429, "status": "RESOURCE_EXHAUSTED"}})), api("rate_limit", 429));
        assert_eq!(outcome(json!({"code": -32603, "data": {"status": "RESOURCE_EXHAUSTED"}})), api("rate_limit", 429));
        assert_eq!(outcome(json!({"code": -32603, "data": {"code": 401, "status": "UNAUTHENTICATED"}})), api("authentication_failed", 401));
        assert_eq!(outcome(json!({"code": -32603, "data": {"code": 403, "status": "PERMISSION_DENIED"}})), api("permission_error", 403));
        assert_eq!(outcome(json!({"code": -32603, "data": {"code": 503, "status": "UNAVAILABLE"}})), api("api_error", 503));
        assert_eq!(outcome(json!({"code": -32603})), TurnOutcome::Failed { code: "agy_-32603".into() });
        assert_eq!(outcome(json!({"code": -32000, "data": "Quota exceeded, retry later"})), TurnOutcome::Failed { code: "agy_-32000".into() }, "text is never read");
        assert!(failed_outcome(&json!({"message": "no code"})).is_err());
        let mut state = session();
        sent(&mut state);
        step(&mut state, "session/update", update(json!({"sessionUpdate": "agent_message_chunk", "content": {"type": "text", "text": "Working"}})));
        assert_eq!(step(&mut state, "alondra/promptFailed", json!({"sessionId": "session-1", "inputId": INPUT, "turnId": "alondra:7", "error": {"code": -32603, "data": {"code": 429, "status": "RESOURCE_EXHAUSTED"}}})), ["TurnEnded", "Reconciled"]);
    }

    #[test]
    fn a_prompt_held_is_consumed_dropped_or_left_uncertain() {
        // Consumed: answered with a stop reason, even with nothing said first.
        let mut consumed = session();
        sent(&mut consumed);
        assert!(matches!(&ended(&mut consumed, "end_turn")[..], [Observation::TurnStarted { .. }, Observation::TurnEnded { .. }, Observation::Reconciled { .. }]));
        // Dropped: answered with an error before any update.
        let mut dropped = session();
        sent(&mut dropped);
        assert_eq!(step(&mut dropped, "alondra/promptFailed", json!({"sessionId": "session-1", "inputId": INPUT, "turnId": "alondra:7", "error": {"code": -32602}})), ["InputDropped", "Reconciled"]);
        // Uncertain: the process died with the prompt held; nothing is decided about the input here,
        // and the next process's session forgets the turn.
        let mut uncertain = session();
        sent(&mut uncertain);
        assert!(uncertain.prompt.is_some());
        uncertain.forget_turn();
        assert!(uncertain.prompt.is_none());
    }

    #[test]
    fn a_prompt_answered_after_the_process_died_is_the_last_host_s() {
        let mut state = session();
        sent(&mut state);
        state.forget_turn();
        assert!(ended(&mut state, "end_turn").is_empty(), "the answer of a prompt the last process held decides nothing");
        let mut other = session();
        sent(&mut other);
        let mut facts = vec![];
        assert!(fold(&batch(), &mut other, "alondra/promptEnded", &json!({"sessionId": "session-1", "turnId": "alondra:9", "stopReason": "end_turn"}), &mut facts, &mut vec![], input).is_err(), "the answer of another prompt while one is open");
    }

    #[test]
    fn a_turn_agy_opens_by_itself_ends_when_its_model_call_reports_usage() {
        let mut state = session();
        assert_eq!(step(&mut state, "session/update", update(json!({"sessionUpdate": "user_message_chunk", "content": {"type": "text", "text": "Scheduled check"}}))), ["TurnUnprompted"]);
        assert_eq!(step(&mut state, "session/update", update(json!({"sessionUpdate": "agent_message_chunk", "content": {"type": "text", "text": "Checked"}}))), ["ModelAnswered"]);
        assert_eq!(step(&mut state, "session/update", update(json!({"sessionUpdate": "usage_update", "used": 900, "size": 1048576}))), ["TurnEnded", "Reconciled"]);
        assert_eq!(step(&mut state, "session/update", update(json!({"sessionUpdate": "agent_message_chunk", "content": {"type": "text", "text": "Again"}}))), ["TurnUnprompted", "ModelAnswered"]);
        assert_eq!(step(&mut state, "alondra/promptSent", json!({"sessionId": "session-1", "inputId": INPUT, "turnId": "alondra:7"})), ["TurnEnded", "Reconciled", "InputHeld"], "a prompt of Alondra's closes it");
    }

    /// No pull request is taken from a command's text or output; a closed one only tells the
    /// task to read its pull requests again.
    #[test]
    fn no_pull_request_is_taken_from_a_commands_text_or_output() {
        let finished = |command: &str, exit: i64, output: &str| {
            let mut state = session();
            sent(&mut state);
            step(&mut state, "session/update", update(json!({"sessionUpdate": "tool_call", "toolCallId": "call_1", "kind": "execute", "status": "pending", "rawInput": {"CommandLine": command}})));
            let mut facts = vec![];
            let mut context = vec![];
            fold(&batch(), &mut state, "session/update", &update(json!({"sessionUpdate": "tool_call_update", "toolCallId": "call_1", "kind": "execute", "status": "completed",
                "rawInput": {"CommandLine": command}, "rawOutput": {"output": output, "exit_code": exit}})), &mut facts, &mut context, input).unwrap();
            assert_eq!(context.len(), usize::from(!output.is_empty()), "the command's output is context of the task");
            facts
        };
        let output = "Command output:\nhttps://github.com/acme/storefront/pull/6\n";
        let plain = finished("make test", 0, output);
        for command in ["gh pr create --fill", "make test && gh pr create --fill", "gh pr view 6"] {
            assert_eq!(finished(command, 0, output), plain, "{command}");
        }
        assert!(finished("gh pr close 6", 0, "").contains(&Observation::PullRequestsChanged));
        assert!(!finished("gh pr close 6", 1, "").contains(&Observation::PullRequestsChanged), "a failed close changed nothing");
    }

    #[test]
    fn updates_of_another_session_are_not_this_task_s() {
        let mut state = session();
        sent(&mut state);
        let mut facts = vec![];
        let other = json!({"sessionId": "session-2", "update": {"sessionUpdate": "agent_message_chunk", "content": {"type": "text", "text": "x"}}});
        // `normalize` routes only this session's updates to `fold`; a stray one changes nothing.
        assert!(string(&other, "sessionId") != state.session.as_deref());
        fold(&batch(), &mut state, "unknown/method", &other, &mut facts, &mut vec![], input).unwrap();
        assert!(facts.is_empty());
    }

    #[test]
    fn a_reconciled_antigravity_work_is_noticed_as_antigravity_s() {
        let named = |work: &str| alondra_provider_contract::reconciled_continuation(&alondra_foundation::WorkId::new(work).unwrap()).as_str().to_owned();
        assert_eq!(named("antigravity:1:process-1:work:call_1"), "antigravity-reconciled:antigravity:1:process-1:work:call_1");
        assert_eq!(named("codex:1:process-1:work:call_1"), "codex-reconciled:codex:1:process-1:work:call_1");
        assert_eq!(named("antigravityish:call_1"), "claude-reconciled:antigravityish:call_1");
    }
}
