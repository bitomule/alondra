use crate::records::{InputRecord, SessionRecord};
use alondra_agent_application::{Normalization, ObservationMetadata, ObservationNormalizer};
use alondra_codex_protocol::{Body, ControlResult, DecisionKind, DecisionOutcome, Fence, FramedReader, Observation as WireObservation, SdkEventKind, SignIn};
use alondra_foundation::{ContinuationId, InputId, MessageUuid, TurnId, WorkId};
use alondra_provider_contract::{pull_requests_named, runs_gh_pr_create, ENDED_WITHOUT_RESULT, AdapterError, ApiFailure, McpServerReport, McpSignInReport, Observation, OrderedObservation, TurnOutcome};
use alondra_store::{ContextSource, TaskContextItem};
use serde::{de::IgnoredAny, Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap,BTreeSet};
use std::io::Cursor;

pub(super) struct Normalizer {
    pub session: Option<SessionRecord>,
    pub fence: Option<Fence>,
    pub first_cursor: Option<u64>,
    pub host_ended: bool,
}
#[derive(Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Checkpoint {
    cursor: u64,
    initialized: bool,
    #[serde(default)]
    blocked_policy:Option<String>,
    #[serde(default)]
    process: String,
    thread: Option<String>,
    turn: Option<String>,
    #[serde(default)]
    model:Option<String>,
    #[serde(default)]
    model_turn:Option<String>,
    #[serde(default)]
    rerouted_turn:Option<String>,
    #[serde(default)]
    cwd:Option<String>,
    work: BTreeMap<String, Work>,
    asked: BTreeMap<u64, String>,
    continuation: Option<String>,
    #[serde(default)]
    continuations: BTreeSet<String>,
    #[serde(default)]
    child_continuations:BTreeMap<String,u64>,
    #[serde(default)]
    input_starts:BTreeMap<String,u64>,
    #[serde(default)]
    child_groups: BTreeMap<String,BTreeSet<String>>,
    #[serde(default)]
    child_activations: BTreeMap<String,String>,
    #[serde(default)]
    accepted: BTreeMap<String, String>,
    #[serde(default)]
    accepted_plans: BTreeMap<String, PlanAcceptance>,
    /// Codex said the thread went idle while its turn was open: that turn's turn/completed comes
    /// next, and anything else ends the turn without a result.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    idled: bool,
    /// Written while Alondra followed Code Mode scripts as work: read from older checkpoints, never written.
    #[serde(default, skip_serializing)]
    code_cells: Option<IgnoredAny>,
    #[serde(default, skip_serializing)]
    ended_cells: Option<IgnoredAny>,
    #[serde(default, skip_serializing)]
    completed_root_turn: Option<IgnoredAny>,
}
/// Work keys of the scripts older releases opened; they end on the first observation after an upgrade.
const LEGACY_CELL_WORK: &str = "code-cell:";
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct PlanAcceptance { epoch: u64, request: String, client: String }
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Work { id: String, turn: String, #[serde(default, skip_serializing_if = "std::ops::Not::not")] background: bool }

#[derive(Clone,Debug,PartialEq,Eq)]
pub struct LiveScope{pub thread:String,pub turn:Option<String>,pub model:Option<String>,pub model_turn:Option<String>,pub rerouted_turn:Option<String>,pub cwd:Option<String>}
pub fn live_scope(bytes:&[u8])->Option<LiveScope>{
    let state:Checkpoint=serde_json::from_slice(bytes).ok()?;
    Some(LiveScope{thread:state.thread.filter(|thread|!thread.is_empty())?,turn:state.turn,model:state.model,model_turn:state.model_turn,rerouted_turn:state.rerouted_turn,cwd:state.cwd})
}

pub fn blocked_policy(bytes:&[u8])->Option<String>{serde_json::from_slice::<Checkpoint>(bytes).ok()?.blocked_policy.filter(|reason|!reason.is_empty() && reason.len()<=1024)}
pub fn background_left(bytes: &[u8]) -> Vec<String> {
    serde_json::from_slice::<Checkpoint>(bytes).map(|state| state.work.into_values().map(|work| work.id).collect()).unwrap_or_default()
}
fn invalid<T>() -> Result<T, AdapterError> { Err(AdapterError::InvalidObservation) }
fn storage(error: impl std::fmt::Debug) -> AdapterError { AdapterError::Storage(format!("{error:?}")) }
fn string<'a>(value: &'a Value, key: &str) -> Option<&'a str> { value.get(key).and_then(Value::as_str) }
fn scoped(batch: &OrderedObservation, process: &str, kind: &str, id: &str) -> String {
    format!("codex:{}:{process}:{kind}:{id}", batch.execution.generation())
}
fn native_turn(batch: &OrderedObservation, process: &str, id: &str) -> Result<TurnId, AdapterError> {
    TurnId::new(scoped(batch, process, "turn", id)).map_err(|_| AdapterError::InvalidObservation)
}
fn lookup(metadata: &ObservationMetadata<'_, '_>, id: &str) -> Result<InputId, AdapterError> {
    let message_id = MessageUuid::new(id).map_err(|_| AdapterError::InvalidObservation)?;
    let bytes = serde_json::to_vec(&InputRecord { message_id }).map_err(|_| AdapterError::InvalidObservation)?;
    let records = metadata.lookup_inputs(&[bytes]).map_err(storage)?;
    if records.len() != 1 { return invalid(); }
    Ok(records[0].input().clone())
}

/// The checkpoint the observation before `batch` left, which `batch` must follow.
fn prior_checkpoint(batch: &OrderedObservation, metadata: &ObservationMetadata<'_, '_>) -> Result<Checkpoint, AdapterError> {
    match metadata.checkpoint().map_err(storage)? {
        Some(prior) => {
            let state: Checkpoint = serde_json::from_slice(prior.bytes()).map_err(|_| AdapterError::InvalidObservation)?;
            if state.cursor != prior.cursor() || state.cursor.checked_add(1) != Some(batch.cursor) { return invalid(); }
            Ok(state)
        }
        None if batch.cursor == 1 => Ok(Checkpoint::default()),
        None => invalid(),
    }
}

impl ObservationNormalizer for Normalizer {
    fn skip(&self, batch: &OrderedObservation, metadata: &ObservationMetadata<'_, '_>) -> Result<Normalization, AdapterError> {
        if &batch.execution != metadata.execution() || batch.cursor != metadata.cursor() { return invalid(); }
        let mut state = prior_checkpoint(batch, metadata)?;
        state.cursor = batch.cursor;
        Normalization::new(Vec::new(), serde_json::to_vec(&state).map_err(|_| AdapterError::InvalidObservation)?)
    }
    fn normalize(&self, batch: &OrderedObservation, metadata: &ObservationMetadata<'_, '_>) -> Result<Normalization, AdapterError> {
        if &batch.execution != metadata.execution() || batch.cursor != metadata.cursor() || self.session.is_none() || batch.evidence.is_empty() || batch.evidence.len() > alondra_codex_protocol::MAX_FRAME_BYTES { return invalid(); }
        let mut framed = (batch.evidence.len() as u32).to_be_bytes().to_vec(); framed.extend_from_slice(&batch.evidence);
        let received = FramedReader::new(Cursor::new(framed)).read_received().map_err(|_| AdapterError::InvalidObservation)?.ok_or(AdapterError::InvalidObservation)?;
        let Body::Event(event) = &received.frame().body else { return invalid() };
        let alondra_codex_protocol::Execution::Task(execution) = &event.execution else { return invalid() };
        if event.cursor != batch.cursor || self.fence.as_ref() != Some(&event.fence)
            || execution.task_id != *batch.execution.task() || execution.agent_id != *batch.execution.node()
            || execution.conversation_id != batch.execution.conversation().as_str()
            || execution.process_instance_id != batch.execution.instance().as_str()
            || execution.generation != batch.execution.generation() || event.fence.host_id != batch.execution.host().as_str() { return invalid(); }
        event.validate().map_err(|_| AdapterError::InvalidObservation)?;
        let mut state = prior_checkpoint(batch, metadata)?;
        if self.first_cursor == Some(batch.cursor) { state.turn = None; state.idled = false; state.model_turn=None;state.rerouted_turn=None;state.work.clear(); state.continuation = None; state.continuations.clear(); state.child_continuations.clear();state.input_starts.clear();state.child_groups.clear();state.child_activations.clear(); }
        let mut facts = Vec::new();
        end_legacy_cells(batch, &mut state, &mut facts)?;
        match &event.observation {
            WireObservation::DecisionRequested { request_id, tool_use_id, kind, payload } => {
                if state.asked.len() >= 64 || state.asked.values().any(|id| id == &request_id.0) { return invalid(); }
                state.asked.insert(batch.cursor, request_id.0.clone());
                facts.push(Observation::Asked {
                    request: request_id.0.clone(), epoch: batch.cursor, tool_use: tool_use_id.clone(),
                    kind: match kind { DecisionKind::Permission => alondra_provider_contract::DecisionKind::Permission, DecisionKind::Question => alondra_provider_contract::DecisionKind::Question, DecisionKind::Plan => alondra_provider_contract::DecisionKind::Plan, DecisionKind::Elicitation => alondra_provider_contract::DecisionKind::Elicitation },
                    payload: serde_json::to_vec(payload).map_err(|_| AdapterError::InvalidObservation)?,
                });
            }
            WireObservation::DecisionSettled { request_id, epoch, outcome } => {
                if state.asked.get(epoch) != Some(&request_id.0) { return invalid(); }
                state.asked.remove(epoch);
                facts.push(Observation::DecisionSettled { epoch: *epoch, answered: *outcome == DecisionOutcome::Answered, continued: state.turn.is_some() || !state.work.is_empty() });
            }
            WireObservation::Sdk { kind: SdkEventKind::ControlResult, payload, .. } => {
                let result: ControlResult = serde_json::from_value(payload.clone()).map_err(|_| AdapterError::InvalidObservation)?;
                facts.push(match result {
                    ControlResult::McpStatus { servers, .. } | ControlResult::McpReconnect { servers, .. } | ControlResult::McpClearAuth { servers, .. } => Observation::McpServers { servers: servers.into_iter().map(|server| McpServerReport { traits: alondra_foundation::McpTraits::default(), name: server.name, status: server.status, error: server.error, source: server.source, transport: server.transport, url: server.url }).collect() },
                    ControlResult::McpAuthenticate { server, sign_in, .. } => Observation::McpSignIn { server, sign_in: match sign_in {
                        SignIn::Browser { url, callback_port, state } => McpSignInReport::Browser { url, callback_port, state },
                        SignIn::Connected {} => McpSignInReport::Connected, SignIn::Refused { error } => McpSignInReport::Refused { error },
                    } },
                });
            }
            WireObservation::Sdk { payload, .. } => {
                let method = string(payload, "method").unwrap_or("");
                let params = payload.get("params").unwrap_or(&Value::Null);
                if method == "alondra/threadReady" {
                    let thread = string(params, "threadId").ok_or(AdapterError::InvalidObservation)?;
                    if state.thread.as_deref().is_some_and(|previous| previous != thread) { return invalid(); }
                    state.thread = Some(thread.to_owned());state.blocked_policy=None;
                    let process = string(params, "processGeneration").ok_or(AdapterError::InvalidObservation)?;
                    if state.initialized && state.process != process {
                        state.turn = None; state.idled = false; state.model_turn=None;state.rerouted_turn=None;state.work.clear(); state.asked.clear(); state.accepted.clear(); state.accepted_plans.clear(); state.continuation = None; state.continuations.clear(); state.child_continuations.clear();state.input_starts.clear();state.child_groups.clear();state.child_activations.clear();
                    }
                    state.model=string(params,"model").filter(|model|!model.is_empty()).map(str::to_owned);state.cwd=string(params,"cwd").map(str::to_owned);
                    state.process = process.to_owned();
                    if !state.initialized { state.initialized = true; facts.push(Observation::Available); }
                } else if method.starts_with("alondra/codeCell") {
                } else if method == "alondra/transportFailed" && !self.host_ended {
                    facts.push(Observation::TransportLost);
                } else if method=="alondra/inputRefused" && params.get("nativeSubmitted").and_then(Value::as_bool)==Some(false) {
                    if state.initialized{fold(batch,&mut state,method,params,&mut facts,|id|lookup(metadata,id))?;}
                } else if string(params, "threadId") == state.thread.as_deref() && state.thread.is_some() {
                    fold(batch, &mut state, method, params, &mut facts, |id| lookup(metadata, id))?;
                }
            }
            WireObservation::ProcessExited { .. } if !self.host_ended => facts.push(Observation::ResourceReleased),
            _ => {}
        }
        if let Some(reason)=crate::context::account_refusal(batch){state.blocked_policy=Some(reason.to_owned());}
        state.cursor = batch.cursor;
        let bytes = serde_json::to_vec(&state).map_err(|_| AdapterError::InvalidObservation)?;
        Normalization::new(facts, bytes)?.with_context(public_context(batch, &event.observation, state.thread.as_deref(), &state.process)?)
    }
}

fn fold(batch: &OrderedObservation, state: &mut Checkpoint, method: &str, params: &Value, facts: &mut Vec<Observation>, lookup: impl FnOnce(&str) -> Result<InputId, AdapterError>) -> Result<(), AdapterError> {
    if state.thread.as_deref().is_some_and(|root|string(params,"threadId").is_some_and(|thread|thread!=root)){return Ok(())}
    let status = (method == "thread/status/changed").then(|| params.pointer("/status/type").and_then(Value::as_str)).flatten();
    if state.idled && method != "turn/completed" && status != Some("idle") {
        end_turn(batch, state, TurnOutcome::Failed { code: ENDED_WITHOUT_RESULT.into() }, facts)?;
    }
    match method {
        // Codex's own word on the thread: idle as the turn is over, just before its
        // turn/completed. An input of ours is taken only when Codex records it as the turn's user
        // message, which names it.
        "thread/status/changed" => match status {
            Some("idle") if state.turn.is_some() => state.idled = true,
            Some("idle") => reconcile(state, facts)?,
            _ => {}
        },
        "alondra/inputStarting"=>{
            let input=string(params,"inputId").filter(|id|!id.is_empty() && id.len()<=256).ok_or(AdapterError::InvalidObservation)?;
            if state.input_starts.len()>=64 || state.input_starts.contains_key(input){return invalid()}
            state.input_starts.insert(input.into(),batch.cursor);
        }
        "alondra/inputRefused" => {
            if params.get("nativeSubmitted").and_then(Value::as_bool)!=Some(false){return invalid()}
            let client=string(params,"inputId").ok_or(AdapterError::InvalidObservation)?;
            state.input_starts.remove(client);facts.push(Observation::InputDropped{input:lookup(client)?});
        }
        // Codex answered the turn/start of an input of ours with an error: it refused it.
        "alondra/inputRejected" => {
            let client=string(params,"inputId").ok_or(AdapterError::InvalidObservation)?;
            if state.accepted.values().any(|accepted|accepted==client){return invalid()}
            state.input_starts.remove(client);facts.push(Observation::InputDropped{input:lookup(client)?});
        }
        "alondra/planContinuationAccepted" => {
            let epoch = params.get("epoch").and_then(Value::as_u64).ok_or(AdapterError::InvalidObservation)?;
            let request = string(params, "requestId").ok_or(AdapterError::InvalidObservation)?;
            if state.asked.get(&epoch).map(String::as_str) != Some(request) || !request.starts_with("plan:") || state.turn.is_some() || state.accepted_plans.len() >= 64 { return invalid(); }
            let turn = string(params, "turnId").ok_or(AdapterError::InvalidObservation)?;
            let client = string(params, "inputId").ok_or(AdapterError::InvalidObservation)?;
            state.accepted_plans.insert(turn.into(), PlanAcceptance { epoch, request: request.into(), client: client.into() });
        }
        "alondra/inputAccepted" => {
            let client = string(params, "inputId").ok_or(AdapterError::InvalidObservation)?;
            let turn = string(params, "turnId").ok_or(AdapterError::InvalidObservation)?;
            if state.accepted.len() >= 64 { return invalid(); }
            if state.accepted.get(turn).is_some_and(|previous| previous != client) { return invalid(); }
            state.accepted.insert(turn.into(), client.into());
            if let Ok(input) = lookup(client) {
                facts.push(Observation::InputHeld { input });
            }
            if state.rerouted_turn.as_deref()!=Some(turn){state.model=string(params,"model").filter(|model|!model.is_empty()).map(str::to_owned).or_else(||state.model.clone());}
            if string(params,"model").is_some() || state.rerouted_turn.as_deref()==Some(turn){state.model_turn=Some(turn.into());}
        }
        "model/rerouted"=>{
            if let (Some(turn),Some(from),Some(to))=(string(params,"turnId"),string(params,"fromModel"),string(params,"toModel"))
                && (state.turn.as_deref()==Some(turn) || state.accepted.contains_key(turn)) && !to.is_empty() && string(params,"reason")==Some("highRiskCyberActivity") && state.model.as_deref().is_none_or(|model|model==from){state.model=Some(to.into());state.model_turn=Some(turn.into());state.rerouted_turn=Some(turn.into());}
        }
        "alondra/childQuiescent"=>{
            let child=string(params,"childThreadId").ok_or(AdapterError::InvalidObservation)?;
            let activation=string(params,"activationKey").ok_or(AdapterError::InvalidObservation)?;
            if state.child_activations.get(child).map(String::as_str)!=Some(activation){return Ok(())}
            let mut ended=vec![];
            for (id,children) in &mut state.child_groups{
                if children.remove(child) && children.is_empty(){ended.push(id.clone())}
            }
            for id in ended{state.child_groups.remove(&id);end_work(batch,state,&id,facts,true)?;}
        }
        "turn/started" => {
            if let Some(id) = params.pointer("/turn/id").and_then(Value::as_str) {
                if state.turn.is_none() && !state.accepted.contains_key(id) && !state.accepted_plans.contains_key(id) {
                    let turn = native_turn(batch, &state.process, id)?;
                    if let Some(legacy)=state.continuation.take(){state.continuations.insert(legacy);}
                    let resumed=state.continuations.iter().find(|id|!state.child_continuations.contains_key(*id)).cloned();if let Some(id)=&resumed{state.continuations.remove(id);}
                    match resumed{
                        Some(continuation)=>facts.push(Observation::TurnResumed{continuation:ContinuationId::new(continuation).map_err(|_|AdapterError::InvalidObservation)?,turn}),
                        None=>facts.push(Observation::TurnUnprompted{turn}),
                    }
                    consume_continuations(state,facts,None)?;
                    state.turn = Some(id.to_owned());if state.model_turn.as_deref()!=Some(id){state.model_turn=None;state.rerouted_turn=None;}
                }
            }
        }
        "turn/completed" => {
            let id = params.pointer("/turn/id").and_then(Value::as_str);
            if let Some(turn) = id && state.turn.as_deref() != Some(turn) && let Some(client) = state.accepted.remove(turn) {
                // The turn Codex started for an input of ours ended without recording it.
                state.input_starts.remove(&client);
                facts.push(Observation::InputDropped { input: lookup(&client)? });
            } else if id.is_some() && id == state.turn.as_deref() {
                let status = params.pointer("/turn/status").and_then(Value::as_str).unwrap_or("");
                let outcome = match status {
                    "completed" => Some(TurnOutcome::Completed),
                    "interrupted" => Some(TurnOutcome::Failed { code: "interrupted".into() }),
                    "failed" => Some(failed_outcome(params.get("turn").unwrap_or(&Value::Null), state.thread.as_deref().is_some_and(|thread| string(params, "threadId") == Some(thread)))),
                    _ => None,
                };
                if let Some(outcome) = outcome {
                    end_turn(batch, state, outcome, facts)?;
                }
            }
        }
        // A command whose call returned while it runs on as a session the model reads and writes
        // later: its first event may carry no process, and this one names it.
        "item/commandExecution/terminalInteraction" => {
            if let (Some(id), Some(_)) = (string(params, "itemId"), string(params, "processId").filter(|process| !process.is_empty()))
                && let Some(work) = state.work.get_mut(id)
            {
                work.background = true;
            }
        }
        "thread/closed" => {
            let open: Vec<String> = state.work.keys().cloned().collect();
            for key in open { cancel_work(batch, state, &key, facts)?; }
            state.child_groups.clear();
        }
        "item/started" | "item/completed" => {
            let item = params.get("item").unwrap_or(&Value::Null);
            let item_type = string(item, "type").unwrap_or("");
            let id = string(item, "id").unwrap_or("");
            if item_type == "userMessage" {
                if let (Some(turn), Some(client)) = (string(params, "turnId"), string(item, "clientId")) {
                    if state.accepted.get(turn).map(String::as_str) == Some(client) {
                        let input = lookup(client)?;
                        match state.turn.as_deref() {
                            Some(previous) if previous == turn => facts.push(Observation::InputConsumed { input }),
                            Some(_) => return invalid(),
                            None => { state.turn = Some(turn.into()); facts.push(Observation::TurnStarted { input, turn: native_turn(batch, &state.process, turn)? }); }
                        }
                        state.accepted.remove(turn);
                        let started=state.input_starts.remove(client);consume_continuations(state,facts,started)?;
                    } else if state.accepted_plans.get(turn).is_some_and(|plan| plan.client == client) {
                        if state.turn.is_some() { return invalid(); }
                        let plan = state.accepted_plans.remove(turn).unwrap();
                        if state.asked.remove(&plan.epoch).as_deref() != Some(&plan.request) { return invalid(); }
                        state.turn = Some(turn.into());
                        facts.push(Observation::TurnUnprompted { turn: native_turn(batch, &state.process, turn)? });
                        facts.push(Observation::DecisionSettled { epoch: plan.epoch, answered: true, continued: true });
                        let started=state.input_starts.remove(client);consume_continuations(state,facts,started)?;
                    }
                }
            }
            if item_type=="subAgentActivity"{
                if matches!(string(item,"kind"),Some("started"|"interacted")) && let Some(child)=string(item,"agentThreadId"){
                    if state.child_activations.contains_key(child){state.child_activations.insert(child.into(),id.into());}
                }
            }
            if item_type=="collabAgentToolCall"{
                let activating=matches!(string(item,"tool"),Some("spawnAgent"|"sendInput"|"resumeAgent"|"followupTask"));
                let key=format!("collab:{id}");
                if activating && (method=="item/started" || !state.work.contains_key(&key)){start_work(batch,state,&key,facts)?;}
                if activating{
                    let receivers:BTreeSet<_>=item.get("receiverThreadIds").and_then(Value::as_array).into_iter().flatten().filter_map(Value::as_str).filter(|id|!id.is_empty()).map(str::to_owned).collect();
                    if receivers.len()>64 || state.child_activations.len().saturating_add(receivers.len())>128{return invalid()}
                    for receiver in &receivers{state.child_activations.insert(receiver.clone(),id.into());}
                    if !receivers.is_empty(){state.child_groups.entry(key.clone()).or_default().extend(receivers);}
                    else if method=="item/completed" && string(item,"status")==Some("failed"){end_work(batch,state,&key,facts,true)?;}
                }
            }
            if method == "item/completed" && item_type == "agentMessage" && string(item, "text").is_some() { facts.push(Observation::ModelAnswered); }
            if method == "item/completed" && item_type == "commandExecution" && item.get("exitCode").and_then(Value::as_i64) == Some(0)
                && string(item, "command").is_some_and(runs_gh_pr_create)
            {
                let named = pull_requests_named(string(item, "aggregatedOutput").unwrap_or(""));
                if named.is_empty() { facts.push(Observation::PullRequestNotNamed); }
                facts.extend(named.into_iter().map(Observation::PullRequestOpened));
            }
            if ["commandExecution", "mcpToolCall", "dynamicToolCall"].contains(&item_type) && !id.is_empty() {
                let finished = method == "item/completed" && !(item_type=="commandExecution" && string(item,"status")==Some("inProgress"));
                let background = item_type == "commandExecution" && string(item, "processId").is_some_and(|process| !process.is_empty());
                if !finished && let Some(work) = state.work.get_mut(id) { work.background |= background; }
                else if !finished && state.work.len() < 64 {
                    if let Some(turn) = state.turn.as_deref() {
                        let work = scoped(batch, &state.process, "work", id);
                        facts.push(Observation::WorkStarted { turn: native_turn(batch, &state.process, turn)?, work: WorkId::new(work.clone()).map_err(|_| AdapterError::InvalidObservation)? });
                        state.work.insert(id.into(), Work { id: work, turn: turn.into(), background });
                    }
                } else if finished {
                    end_work(batch,state,id,facts,false)?;
                    if item_type == "mcpToolCall" && item.get("error").is_some_and(|error| !error.is_null()) { facts.push(Observation::McpToolFailed); }
                }
            }
        }
        _ => {}
    }
    Ok(())
}
/// Ends the open turn with `outcome`, and with it the foreground work it ran; after an idle thread,
/// what still runs is reconciled with the task's record.
fn end_turn(batch: &OrderedObservation, state: &mut Checkpoint, outcome: TurnOutcome, facts: &mut Vec<Observation>) -> Result<(), AdapterError> {
    let Some(turn) = state.turn.take() else { return Ok(()) };
    let left: Vec<String> = state.work.iter().filter(|(key, work)| work.turn == turn && !work.background && !key.starts_with("collab:")).map(|(key, _)| key.clone()).collect();
    for key in left { cancel_work(batch, state, &key, facts)?; }
    facts.push(Observation::TurnEnded { turn: native_turn(batch, &state.process, &turn)?, outcome });
    if std::mem::take(&mut state.idled) { reconcile(state, facts)?; }
    Ok(())
}
/// The thread is idle: of its work only what a command left in the background and the subagents
/// it started go on.
fn reconcile(state: &mut Checkpoint, facts: &mut Vec<Observation>) -> Result<(), AdapterError> {
    state.work.retain(|key, work| work.background || key.starts_with("collab:"));
    let live = state.work.values().map(|work| WorkId::new(work.id.clone()).map_err(|_| AdapterError::InvalidObservation)).collect::<Result<_, _>>()?;
    facts.push(Observation::Reconciled { idle: true, live });
    Ok(())
}
fn consume_continuations(state:&mut Checkpoint,facts:&mut Vec<Observation>,input_started:Option<u64>)->Result<(),AdapterError>{
    if let Some(legacy)=state.continuation.take(){state.continuations.insert(legacy);}
    let eligible:Vec<_>=state.continuations.iter().filter(|id|state.child_continuations.get(*id).is_none_or(|cutoff|input_started.is_some_and(|started|started>*cutoff))).cloned().collect();
    for continuation in eligible{state.continuations.remove(&continuation);state.child_continuations.remove(&continuation);facts.push(Observation::ContinuationConsumed{continuation:ContinuationId::new(continuation).map_err(|_|AdapterError::InvalidObservation)?});}Ok(())
}
fn start_work(batch:&OrderedObservation,state:&mut Checkpoint,id:&str,facts:&mut Vec<Observation>)->Result<(),AdapterError>{
    if state.work.contains_key(id){return Ok(())}
    if state.work.len()>=128{return invalid()}
    let Some(turn)=state.turn.as_deref() else{return invalid()};
    let work=scoped(batch,&state.process,"work",id);
    facts.push(Observation::WorkStarted{turn:native_turn(batch,&state.process,turn)?,work:WorkId::new(&work).map_err(|_|AdapterError::InvalidObservation)?});
    state.work.insert(id.into(),Work{id:work,turn:turn.into(),background:false});Ok(())
}
fn end_work(batch:&OrderedObservation,state:&mut Checkpoint,id:&str,facts:&mut Vec<Observation>,await_next_input:bool)->Result<(),AdapterError>{
    if let Some(work)=state.work.remove(id){
        let value=scoped(batch,&state.process,"continuation",id);let continuation=ContinuationId::new(&value).map_err(|_|AdapterError::InvalidObservation)?;
        facts.push(Observation::WorkEnded{work:WorkId::new(work.id).map_err(|_|AdapterError::InvalidObservation)?,continuation:continuation.clone()});
        if await_next_input{state.child_continuations.insert(value.clone(),batch.cursor);}
        if await_next_input || state.turn.is_none(){state.continuations.insert(value);}else{facts.push(Observation::ContinuationConsumed{continuation});}
    }Ok(())
}
fn end_legacy_cells(batch:&OrderedObservation,state:&mut Checkpoint,facts:&mut Vec<Observation>)->Result<(),AdapterError>{
    let legacy:Vec<String>=state.work.keys().filter(|key|key.starts_with(LEGACY_CELL_WORK)).cloned().collect();
    for key in legacy{cancel_work(batch,state,&key,facts)?;}Ok(())
}
/// Ends native work Codex will not finish on its own: nothing waits for it, so no turn is woken.
fn cancel_work(batch:&OrderedObservation,state:&mut Checkpoint,id:&str,facts:&mut Vec<Observation>)->Result<(),AdapterError>{
    if let Some(work)=state.work.remove(id){
        let continuation=ContinuationId::new(scoped(batch,&state.process,"continuation",id)).map_err(|_|AdapterError::InvalidObservation)?;
        facts.push(Observation::WorkEnded{work:WorkId::new(work.id).map_err(|_|AdapterError::InvalidObservation)?,continuation:continuation.clone()});
        facts.push(Observation::ContinuationConsumed{continuation});
    }Ok(())
}
fn public_context(batch: &OrderedObservation, observation: &WireObservation, thread: Option<&str>, process: &str) -> Result<Vec<TaskContextItem>, AdapterError> {
    let WireObservation::Sdk { payload, .. } = observation else { return Ok(Vec::new()) };
    if string(payload,"method")==Some("alondra/childQuiescent"){
        let params=payload.get("params").unwrap_or(&Value::Null);
        if thread.is_none() || string(params,"threadId")!=thread{return Ok(Vec::new())}
        let Some(report)=string(params,"report") else{return Ok(Vec::new())};
        let child=string(params,"childThreadId").ok_or(AdapterError::InvalidObservation)?;
        let activation=string(params,"activationKey").ok_or(AdapterError::InvalidObservation)?;
        let identity=format!("codex:{}:{process}:{}:child:{child}:{activation}",batch.execution.generation(),thread.unwrap());
        return Ok(vec![TaskContextItem::tool_result(alondra_store::native_context_id(&identity),report,None,None).map_err(storage)?]);
    }
    if string(payload, "method") != Some("item/completed") { return Ok(Vec::new()); }
    let params = payload.get("params").unwrap_or(&Value::Null);
    if thread.is_none() || string(params, "threadId") != thread { return Ok(Vec::new()); }
    let item = params.get("item").unwrap_or(&Value::Null);
    let Some(id) = string(item, "id") else { return Ok(Vec::new()) };
    let turn = string(params, "turnId").unwrap_or("unknown");
    let identity = format!("codex:{}:{process}:{}:{turn}:{id}", batch.execution.generation(), thread.unwrap());
    let identity = alondra_store::native_context_id(&identity);
    let item = match string(item, "type") {
        Some("agentMessage") => match string(item, "text") { Some(text) => Some(TaskContextItem::text(identity, ContextSource::Assistant, text).map_err(storage)?), None => None },
        Some("commandExecution") => {
            let output = string(item, "aggregatedOutput").unwrap_or("");
            Some(TaskContextItem::tool_result(identity, output, item.get("exitCode").and_then(Value::as_i64), None).map_err(storage)?)
        }
        Some("mcpToolCall" | "dynamicToolCall") => {
            let content = item.pointer("/result/content").or_else(|| item.get("contentItems")).and_then(Value::as_array);
            let text = content.into_iter().flatten().filter(|content| string(content, "type") == Some("text")).filter_map(|content| string(content, "text")).collect::<Vec<_>>().join("\n");
            (!text.is_empty()).then(|| TaskContextItem::tool_result(identity, text, None, None)).transpose().map_err(storage)?
        }
        _ => None,
    };
    Ok(item.into_iter().collect())
}

fn failed_outcome(turn: &Value, scoped: bool) -> TurnOutcome {
    let error = turn.get("error").unwrap_or(&Value::Null);
    match error.get("codexErrorInfo").and_then(Value::as_str) {
        Some("badRequest") if scoped => TurnOutcome::Failed { code: "codex_request_rejected".into() },
        Some("contextWindowExceeded") if scoped => TurnOutcome::Failed { code: "codex_context_exceeded".into() },
        Some("usageLimitExceeded") => TurnOutcome::ApiFailed(ApiFailure { error: "rate_limit".into(), status: Some(429), resets_at: None }),
        Some("unauthorized") => TurnOutcome::ApiFailed(ApiFailure { error: "authentication_failed".into(), status: Some(401), resets_at: None }),
        _ => TurnOutcome::Failed { code: "codex_turn_failed".into() },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alondra_foundation::{AgentId, ConversationId, ExecutionStamp, HostId, InputId, InstanceId, TaskId};
    use serde_json::json;

    fn batch() -> OrderedObservation {
        OrderedObservation { owner_generation: 1, execution: ExecutionStamp::new(TaskId("task".into()), AgentId("agent".into()), ConversationId::new("conversation").unwrap(), InstanceId::new("instance").unwrap(), HostId::new("alondra-sdk").unwrap(), 1).unwrap(), cursor: 1, evidence: vec![1] }
    }
    #[test]
    fn a_pr_a_command_opened_is_a_fact_of_its_task() {
        let batch=batch();let mut state=Checkpoint{thread:Some("root".into()),process:"process".into(),turn:Some("origin".into()),..Checkpoint::default()};
        let finished=|state:&mut Checkpoint,command:&str,exit:i64,output:&str|{
            let mut facts=vec![];
            fold(&batch,state,"item/completed",&json!({"threadId":"root","item":{"type":"commandExecution","id":"pr","status":"completed","command":command,"exitCode":exit,"aggregatedOutput":output}}),&mut facts,|_|invalid()).unwrap();
            facts.retain(|fact|matches!(fact,Observation::PullRequestOpened(_)|Observation::PullRequestNotNamed));
            facts
        };
        assert_eq!(finished(&mut state,"bash -lc 'gh pr create --fill'",0,"https://github.com/acme/storefront/pull/6\n"),
            vec![Observation::PullRequestOpened(alondra_provider_contract::OpenedPullRequest{host:"github.com".into(),repository:"acme/storefront".into(),number:6})]);
        assert_eq!(finished(&mut state,"gh pr create --fill",0,""),vec![Observation::PullRequestNotNamed]);
        assert_eq!(finished(&mut state,"gh pr create --fill",1,"https://github.com/o/r/pull/1"),vec![],"a command that failed opened nothing");
        assert_eq!(finished(&mut state,"gh pr view 1",0,"https://github.com/o/r/pull/1"),vec![],"naming a PR is not opening it");
    }

    #[test]
    fn a_tool_still_open_when_its_turn_ends_is_cancelled_without_waking_a_turn() {
        let batch=batch();let mut state=Checkpoint{thread:Some("root".into()),process:"process".into(),turn:Some("origin".into()),..Checkpoint::default()};let mut facts=vec![];
        fold(&batch,&mut state,"item/started",&json!({"threadId":"root","item":{"type":"mcpToolCall","id":"exec-paused","status":"inProgress"}}),&mut facts,|_|invalid()).unwrap();
        fold(&batch,&mut state,"item/started",&json!({"threadId":"root","item":{"type":"commandExecution","id":"server","processId":"42","status":"inProgress"}}),&mut facts,|_|invalid()).unwrap();
        fold(&batch,&mut state,"item/started",&json!({"threadId":"root","item":{"type":"collabAgentToolCall","id":"spawn","tool":"spawnAgent","receiverThreadIds":["child"]}}),&mut facts,|_|invalid()).unwrap();
        facts.clear();
        fold(&batch,&mut state,"turn/completed",&json!({"threadId":"root","turn":{"id":"origin","status":"completed"}}),&mut facts,|_|invalid()).unwrap();
        assert!(matches!(&facts[..],[Observation::WorkEnded{work,continuation},Observation::ContinuationConsumed{continuation:consumed},Observation::TurnEnded{..}] if work.as_str()=="codex:1:process:work:exec-paused" && continuation==consumed));
        assert_eq!(state.work.keys().map(String::as_str).collect::<Vec<_>>(),["collab:spawn","server"],"background commands and children outlive their turn");
        assert!(state.continuations.is_empty());
        facts.clear();
        fold(&batch,&mut state,"item/completed",&json!({"threadId":"root","item":{"type":"mcpToolCall","id":"exec-paused","status":"completed"}}),&mut facts,|_|invalid()).unwrap();
        assert!(facts.is_empty(),"the late end of a cancelled tool changes nothing");
    }
    // An idle thread whose turn/completed never came: the next word of the thread ends that turn,
    // and the tool it ran; the background command and the subagent go on.
    #[test]
    fn an_idle_thread_ends_its_turn_and_keeps_only_what_runs_in_the_background() {
        let batch=batch();let mut state=Checkpoint{thread:Some("root".into()),process:"process".into(),turn:Some("origin".into()),..Checkpoint::default()};let mut facts=vec![];
        fold(&batch,&mut state,"item/started",&json!({"threadId":"root","item":{"type":"mcpToolCall","id":"exec-paused","status":"inProgress"}}),&mut facts,|_|invalid()).unwrap();
        fold(&batch,&mut state,"item/started",&json!({"threadId":"root","item":{"type":"commandExecution","id":"server","processId":"42","status":"inProgress"}}),&mut facts,|_|invalid()).unwrap();
        fold(&batch,&mut state,"item/started",&json!({"threadId":"root","item":{"type":"collabAgentToolCall","id":"spawn","tool":"spawnAgent","receiverThreadIds":["child"]}}),&mut facts,|_|invalid()).unwrap();
        facts.clear();
        fold(&batch,&mut state,"thread/status/changed",&json!({"threadId":"root","status":{"type":"active","activeFlags":["waitingOnApproval"]}}),&mut facts,|_|invalid()).unwrap();
        assert!(facts.is_empty() && state.turn.is_some(),"an active thread still runs its turn");
        fold(&batch,&mut state,"thread/status/changed",&json!({"threadId":"root","status":{"type":"idle"}}),&mut facts,|_|invalid()).unwrap();
        assert!(facts.is_empty() && state.turn.is_some(),"its turn/completed comes right after");
        fold(&batch,&mut state,"thread/status/changed",&json!({"threadId":"root","status":{"type":"active","activeFlags":[]}}),&mut facts,|_|invalid()).unwrap();
        let live=BTreeSet::from([WorkId::new("codex:1:process:work:server").unwrap(),WorkId::new("codex:1:process:work:collab:spawn").unwrap()]);
        assert_eq!(facts.len(),4,"{facts:?}");
        assert!(matches!(&facts[0],Observation::WorkEnded{work,..} if work.as_str()=="codex:1:process:work:exec-paused"));
        assert_eq!(facts[2],Observation::TurnEnded{turn:TurnId::new("codex:1:process:turn:origin").unwrap(),outcome:TurnOutcome::Failed{code:ENDED_WITHOUT_RESULT.into()}});
        assert_eq!(facts[3],Observation::Reconciled{idle:true,live});
        assert!(state.turn.is_none());
        assert_eq!(state.work.keys().map(String::as_str).collect::<Vec<_>>(),["collab:spawn","server"]);
    }
    /// Two turns of a real `codex app-server` 0.163, recorded with only the thread's own messages
    /// kept; the host's acceptance of each input, which the recording has no part in, is added
    /// where the host sends it, right after the answer to turn/start.
    fn recorded_codex()->Vec<(String,Value)>{
        let mut inputs=0;let mut out=vec![];
        for line in include_str!("../fixtures/native/two-turns.jsonl").lines(){
            let mut message:Value=serde_json::from_str(line).unwrap();
            if message["response"]=="turn/start"{
                inputs+=1;
                out.push(("alondra/inputAccepted".to_owned(),json!({"threadId":"root","turnId":message["turn"]["id"],"inputId":format!("input-{inputs}")})));
                continue;
            }
            let method=message["method"].as_str().unwrap().to_owned();
            message["params"]["threadId"]=json!("root");
            if message["params"]["item"]["type"]=="userMessage"{message["params"]["item"]["clientId"]=json!(format!("input-{inputs}"));}
            out.push((method,message["params"].clone()));
        }
        out
    }
    // Codex records each input as its turn's user message, naming it, and says the thread is idle
    // just before the turn's turn/completed: the first turn starts at its user message and
    // completes; the second, whose turn/completed the recording stops before, ends at the
    // thread's next word.
    #[test]
    fn a_codex_thread_that_goes_idle_ends_its_turn() {
        let batch=batch();let mut state=Checkpoint{thread:Some("root".into()),process:"process".into(),initialized:true,..Checkpoint::default()};
        let mut per=vec![];
        for (method,params) in recorded_codex(){
            let mut facts=vec![];
            fold(&batch,&mut state,&method,&params,&mut facts,|id|Ok(InputId::new(id).unwrap())).unwrap();
            per.push((method,params,facts));
        }
        let started:Vec<_>=per.iter().filter(|(_,_,facts)|facts.iter().any(|fact|matches!(fact,Observation::TurnStarted{..}))).collect();
        assert_eq!(started.len(),2);
        assert!(started.iter().all(|(method,params,_)|method=="item/started" && params["item"]["type"]=="userMessage"),"each turn starts as Codex records its input");
        let ended:Vec<&Observation>=per.iter().flat_map(|(_,_,facts)|facts).filter(|fact|matches!(fact,Observation::TurnEnded{..})).collect();
        assert_eq!(ended.len(),1);
        assert!(matches!(ended[0],Observation::TurnEnded{outcome:TurnOutcome::Completed,..}),"an idle thread whose turn/completed follows keeps its outcome");
        assert!(state.idled && state.turn.is_some());
        let mut facts=vec![];
        fold(&batch,&mut state,"thread/status/changed",&json!({"threadId":"root","status":{"type":"active","activeFlags":[]}}),&mut facts,|id|Ok(InputId::new(id).unwrap())).unwrap();
        assert!(matches!(&facts[0],Observation::TurnEnded{outcome:TurnOutcome::Failed{code},..} if code==ENDED_WITHOUT_RESULT),"{facts:?}");
        assert!(state.turn.is_none() && !state.idled);
    }
    // Codex answers the turn/start of an input of ours with an error, or ends the turn it started
    // for it without recording it as the turn's user message: it never took it.
    #[test]
    fn an_input_codex_refuses_or_never_records_is_dropped() {
        let batch=batch();let input=|id:&str|{assert_eq!(id,"client");Ok(InputId::new("registered").unwrap())};
        let dropped=|facts:&[Observation]|matches!(facts,[Observation::InputDropped{input}] if input.as_str()=="registered");
        let mut state=Checkpoint{thread:Some("root".into()),process:"process".into(),initialized:true,..Checkpoint::default()};let mut facts=vec![];
        fold(&batch,&mut state,"alondra/inputStarting",&json!({"threadId":"root","inputId":"client"}),&mut facts,|_|invalid()).unwrap();
        fold(&batch,&mut state,"alondra/inputRejected",&json!({"threadId":"root","inputId":"client","code":"-32600"}),&mut facts,input).unwrap();
        assert!(dropped(&facts),"{facts:?}");assert!(state.input_starts.is_empty());

        let mut state=Checkpoint{thread:Some("root".into()),process:"process".into(),initialized:true,..Checkpoint::default()};let mut facts=vec![];
        fold(&batch,&mut state,"alondra/inputAccepted",&json!({"threadId":"root","turnId":"turn","inputId":"client"}),&mut facts,|_|invalid()).unwrap();
        fold(&batch,&mut state,"thread/status/changed",&json!({"threadId":"root","status":{"type":"active","activeFlags":[]}}),&mut facts,|_|invalid()).unwrap();
        fold(&batch,&mut state,"turn/started",&json!({"threadId":"root","turn":{"id":"turn"}}),&mut facts,|_|invalid()).unwrap();
        assert!(facts.is_empty(),"an accepted input is not taken until Codex records it: {facts:?}");
        fold(&batch,&mut state,"thread/status/changed",&json!({"threadId":"root","status":{"type":"idle"}}),&mut facts,|_|invalid()).unwrap();
        facts.clear();
        fold(&batch,&mut state,"turn/completed",&json!({"threadId":"root","turn":{"id":"turn","status":"failed"}}),&mut facts,input).unwrap();
        assert!(dropped(&facts),"{facts:?}");assert!(state.accepted.is_empty() && state.turn.is_none());
    }
    // 7 Nov, t-model-spread-by-margin: a long check started with no process named, its call returned
    // a session that kept running, and the turn ended. The command was taken for a tool its turn
    // left open: Alondra said no work of the node's own ran, and ignored the command's end.
    #[test]
    fn a_command_that_runs_on_as_a_session_outlives_its_turn_and_its_end_wakes_the_node() {
        let batch=batch();let mut state=Checkpoint{thread:Some("root".into()),process:"process".into(),turn:Some("origin".into()),..Checkpoint::default()};let mut facts=vec![];
        fold(&batch,&mut state,"item/started",&json!({"threadId":"root","item":{"type":"commandExecution","id":"check","status":"inProgress"}}),&mut facts,|_|invalid()).unwrap();
        fold(&batch,&mut state,"item/commandExecution/terminalInteraction",&json!({"threadId":"root","turnId":"origin","itemId":"check","processId":"11310","stdin":""}),&mut facts,|_|invalid()).unwrap();
        facts.clear();
        fold(&batch,&mut state,"turn/completed",&json!({"threadId":"root","turn":{"id":"origin","status":"completed"}}),&mut facts,|_|invalid()).unwrap();
        assert!(matches!(&facts[..],[Observation::TurnEnded{..}]),"the session is work of the node's own: {facts:?}");
        assert_eq!(state.work.keys().map(String::as_str).collect::<Vec<_>>(),["check"]);
        facts.clear();
        fold(&batch,&mut state,"item/completed",&json!({"threadId":"root","item":{"type":"commandExecution","id":"check","status":"completed","processId":"11310","exitCode":0}}),&mut facts,|_|invalid()).unwrap();
        assert!(matches!(&facts[..],[Observation::WorkEnded{work,continuation}] if work.as_str()=="codex:1:process:work:check" && continuation.as_str()=="codex:1:process:continuation:check"),"its end wakes the node: {facts:?}");
        assert!(state.work.is_empty());
    }
    #[test]
    fn a_closed_thread_ends_all_its_open_work_without_waking_a_turn() {
        let batch=batch();let mut state=Checkpoint{thread:Some("root".into()),process:"process".into(),turn:Some("origin".into()),..Checkpoint::default()};let mut facts=vec![];
        fold(&batch,&mut state,"item/started",&json!({"threadId":"root","item":{"type":"commandExecution","id":"server","processId":"42","status":"inProgress"}}),&mut facts,|_|invalid()).unwrap();
        fold(&batch,&mut state,"turn/completed",&json!({"threadId":"root","turn":{"id":"origin","status":"completed"}}),&mut facts,|_|invalid()).unwrap();
        facts.clear();
        fold(&batch,&mut state,"thread/closed",&json!({"threadId":"root"}),&mut facts,|_|invalid()).unwrap();
        assert!(state.work.is_empty() && state.continuations.is_empty());
        assert!(matches!(&facts[..],[Observation::WorkEnded{..},Observation::ContinuationConsumed{..}]));
    }
    #[test]
    fn a_checkpoint_that_followed_scripts_still_reads_and_its_script_work_ends() {
        let old=json!({"cursor":4,"initialized":true,"process":"process","thread":"root","turn":null,"work":{"code-cell:[\"root\",\"origin\",\"exec\"]":{"id":"codex:1:process:cell:x","turn":"origin"},"collab:spawn":{"id":"codex:1:process:work:collab:spawn","turn":"origin"}},"asked":{},"continuation":null,
            "code_cells":{"[\"root\",\"origin\",\"exec\"]":{"origin":"origin","cell":"17","parent":null,"work":"codex:1:process:cell:x","work_key":"code-cell:[\"root\",\"origin\",\"exec\"]","finished":false,"woken":true,"cursor":3}},"ended_cells":{"wait":2},"completed_root_turn":"origin"});
        let mut state:Checkpoint=serde_json::from_value(old).unwrap();
        let rewritten=serde_json::to_value(&state).unwrap();
        assert!(rewritten.get("code_cells").is_none() && rewritten.get("ended_cells").is_none() && rewritten.get("completed_root_turn").is_none());
        let mut facts=vec![];
        end_legacy_cells(&batch(),&mut state,&mut facts).unwrap();
        assert_eq!(state.work.keys().map(String::as_str).collect::<Vec<_>>(),["collab:spawn"]);
        assert!(matches!(&facts[..],[Observation::WorkEnded{work,..},Observation::ContinuationConsumed{..}] if work.as_str()=="codex:1:process:cell:x"));
    }
    #[test]
    fn native_plan_approval_settles_the_ended_turn_before_starting_its_new_turn() {
        for background in [false, true] {
            let batch = batch();
            let mut state = Checkpoint { thread: Some("root".into()), ..Checkpoint::default() };
            state.asked.insert(7, "plan:request".into());
            if background {
                state.work.insert("child".into(), Work { id: "background-work".into(), turn: "previous-turn".into(), background: true });
            }
            let mut facts = vec![];
            fold(&batch, &mut state, "alondra/inputStarting", &json!({"threadId":"root","inputId":"plan:7"}), &mut facts, |_| invalid()).unwrap();
            fold(&batch, &mut state, "alondra/planContinuationAccepted", &json!({"threadId":"root","requestId":"plan:request","epoch":7,"inputId":"plan:7","turnId":"new-turn"}), &mut facts, |_| invalid()).unwrap();
            fold(&batch, &mut state, "turn/started", &json!({"threadId":"root","turn":{"id":"new-turn"}}), &mut facts, |_| invalid()).unwrap();
            assert!(facts.is_empty(), "acceptance waits for the exact native user item");
            let params = json!({"threadId":"root","turnId":"new-turn","item":{"type":"userMessage","id":"native-user-item","clientId":"plan:7"}});
            fold(&batch, &mut state, "item/started", &params, &mut facts, |_| invalid()).unwrap();
            assert_eq!(facts.len(), 2);
            assert!(matches!(facts[0], Observation::TurnUnprompted { .. }), "the positively consumed native user item starts the implementation turn before Core settles its decision");
            assert!(matches!(facts[1], Observation::DecisionSettled { epoch:7, answered:true, continued:true }));
            assert!(!facts.iter().any(|fact| matches!(fact, Observation::ContinuationConsumed { .. })), "Core consumes its decision continuation through RecordDecisionSettled, never generic work consumption");
            assert!(state.asked.is_empty());assert!(state.accepted_plans.is_empty());
            fold(&batch, &mut state, "item/completed", &params, &mut facts, |_| invalid()).unwrap();
            assert_eq!(facts.len(), 2, "the completed copy cannot settle, start or consume the turn again");
        }
    }

    #[test]
    fn long_native_message_ids_keep_public_content_without_fabricating_complete_empty_history() {
        let observed = |id:&str| serde_json::from_value::<WireObservation>(json!({"type":"sdk","kind":"message","native":{},"failures":[],"payload":{"method":"item/completed","params":{"threadId":"root","turnId":"turn","item":{"type":"agentMessage","id":id,"text":"The durable public result"}}}})).unwrap();
        let first=public_context(&batch(),&observed(&"native-id".repeat(1024)),Some("root"),"process").unwrap();
        assert_eq!(first.len(),1);assert_eq!(first[0].content,"The durable public result");assert!(first[0].native_id.len()<=4096);
        assert_eq!(first,public_context(&batch(),&observed(&"native-id".repeat(1024)),Some("root"),"process").unwrap());
        assert_ne!(first[0].native_id,public_context(&batch(),&observed(&"other-native-id".repeat(1024)),Some("root"),"process").unwrap()[0].native_id);
    }
    #[test]
    fn native_completion_ends_only_the_known_turn_and_keeps_background_work() {
        let batch = batch();
        let mut state = Checkpoint { turn: Some("turn".into()), ..Checkpoint::default() };
        let mut facts = Vec::new();
        fold(&batch, &mut state, "item/started", &json!({"item":{"id":"terminal","type":"commandExecution","processId":"42"}}), &mut facts, |_| invalid()).unwrap();
        fold(&batch, &mut state, "turn/completed", &json!({"turn":{"id":"other","status":"completed"}}), &mut facts, |_| invalid()).unwrap();
        assert_eq!(state.turn.as_deref(), Some("turn"));
        fold(&batch, &mut state, "turn/completed", &json!({"turn":{"id":"turn","status":"completed"}}), &mut facts, |_| invalid()).unwrap();
        assert!(matches!(facts.last(), Some(Observation::TurnEnded { outcome: TurnOutcome::Completed, .. })));
        assert!(state.turn.is_none()); assert_eq!(state.work.len(), 1);
        fold(&batch, &mut state, "item/completed", &json!({"item":{"id":"terminal","type":"commandExecution"}}), &mut facts, |_| invalid()).unwrap();
        assert!(matches!(facts.last(), Some(Observation::WorkEnded { .. })));
    }
    #[test]
    fn native_request_rejections_are_task_local_and_require_the_exact_thread_and_turn() {
        for (kind, code) in [("badRequest", "codex_request_rejected"), ("contextWindowExceeded", "codex_context_exceeded")] {
            for (thread, turn, expected) in [("root", "turn", true), ("child", "turn", false), ("root", "other", false)] {
                let mut state = Checkpoint { thread: Some("root".into()), turn: Some("turn".into()), ..Checkpoint::default() };
                let mut facts = Vec::new();
                fold(&batch(), &mut state, "turn/completed", &json!({"threadId":thread,"turn":{"id":turn,"status":"failed","error":{"codexErrorInfo":kind,"message":"The requested model cannot be used for this request"}}}), &mut facts, |_| invalid()).unwrap();
                if expected { assert!(matches!(facts.as_slice(), [Observation::TurnEnded { outcome: TurnOutcome::Failed { code: actual }, .. }] if actual == code)); }
                else { assert!(facts.is_empty()); assert_eq!(state.turn.as_deref(), Some("turn")); }
            }
        }
        for kind in [json!("other"), json!("serverOverloaded"), json!({"httpConnectionFailed":{"httpStatusCode":400}})] {
            assert!(matches!(failed_outcome(&json!({"error":{"codexErrorInfo":kind,"message":"Model not found or quota exceeded"}}), true), TurnOutcome::Failed { code } if code == "codex_turn_failed"));
        }
        assert!(matches!(failed_outcome(&json!({"error":{"codexErrorInfo":"badRequest"}}), false), TurnOutcome::Failed { code } if code == "codex_turn_failed"));
        assert!(matches!(failed_outcome(&json!({"error":{"codexErrorInfo":"unauthorized"}}), true), TurnOutcome::ApiFailed(ApiFailure { status: Some(401), .. })));
        assert!(matches!(failed_outcome(&json!({"error":{"codexErrorInfo":"usageLimitExceeded"}}), true), TurnOutcome::ApiFailed(ApiFailure { status: Some(429), .. })));
    }

    #[test]
    fn native_acceptance_without_correlated_user_item_cannot_consume_input() {
        let batch = batch(); let mut state = Checkpoint::default(); let mut facts = Vec::new();
        fold(&batch, &mut state, "alondra/inputAccepted", &json!({"turnId":"turn","inputId":"client-message"}), &mut facts, |_| Ok(InputId::new("input").unwrap())).unwrap();
        assert_eq!(facts, vec![Observation::InputHeld { input: InputId::new("input").unwrap() }], "turn/start response proves the provider holds the input, not that it read it");
        facts.clear();
        fold(&batch, &mut state, "turn/started", &json!({"turn":{"id":"turn"}}), &mut facts, |_| invalid()).unwrap();
        assert!(facts.is_empty(), "a native turn without its identified user item cannot consume this input");
        fold(&batch, &mut state, "item/started", &json!({"turnId":"turn","item":{"id":"user","type":"userMessage","clientId":"wrong-message"}}), &mut facts, |_| panic!("an unrelated native user item is not consumption proof")).unwrap();
        assert!(facts.is_empty());
        fold(&batch, &mut state, "item/started", &json!({"turnId":"turn","item":{"id":"user","type":"userMessage","clientId":"client-message"}}), &mut facts, |id| { assert_eq!(id, "client-message"); Ok(InputId::new("input").unwrap()) }).unwrap();
        assert!(matches!(facts.as_slice(), [Observation::TurnStarted { input, .. }] if input.as_str() == "input"));
        fold(&batch, &mut state, "item/completed", &json!({"turnId":"turn","item":{"id":"user","type":"userMessage","clientId":"client-message"}}), &mut facts, |_| panic!("the completed echo must not consume twice")).unwrap();
        assert_eq!(facts.len(), 1);
    }
    #[test]
    fn unknown_turn_status_is_retained_without_claiming_completion() {
        let batch = batch(); let mut state = Checkpoint { turn: Some("turn".into()), ..Checkpoint::default() }; let mut facts = Vec::new();
        fold(&batch, &mut state, "turn/completed", &json!({"turn":{"id":"turn","status":"futureStatus"}}), &mut facts, |_| invalid()).unwrap();
        assert!(facts.is_empty()); assert!(state.turn.is_some());
    }
    #[test]
    fn a_native_child_outlives_its_parent_turn_until_positive_scoped_quiescence(){
        let batch=batch();let mut state=Checkpoint{thread:Some("parent".into()),turn:Some("root-turn".into()),..Default::default()};let mut facts=vec![];
        fold(&batch,&mut state,"item/started",&json!({"threadId":"parent","item":{"type":"collabAgentToolCall","id":"spawn","tool":"spawnAgent","receiverThreadIds":[]}}),&mut facts,|_|invalid()).unwrap();
        fold(&batch,&mut state,"item/completed",&json!({"threadId":"parent","item":{"type":"collabAgentToolCall","id":"spawn","tool":"spawnAgent","status":"completed","receiverThreadIds":["child"],"agentsStates":{"child":{"status":"running"}}}}),&mut facts,|_|invalid()).unwrap();
        fold(&batch,&mut state,"turn/completed",&json!({"threadId":"parent","turn":{"id":"root-turn","status":"completed"}}),&mut facts,|_|invalid()).unwrap();
        assert_eq!(state.work.len(),1);assert!(state.turn.is_none());assert!(!facts.iter().any(|fact|matches!(fact,Observation::WorkEnded{..})));
        for (thread,child,epoch) in [("foreign","child","spawn"),("parent","unknown","spawn"),("parent","child","stale")]{fold(&batch,&mut state,"alondra/childQuiescent",&json!({"threadId":thread,"childThreadId":child,"activationKey":epoch}),&mut facts,|_|invalid()).unwrap();}
        assert_eq!(state.work.len(),1,"foreign, unknown and stale completion cannot release a managed child");
        fold(&batch,&mut state,"alondra/childQuiescent",&json!({"threadId":"parent","childThreadId":"child","activationKey":"spawn"}),&mut facts,|_|invalid()).unwrap();
        assert!(state.work.is_empty());assert_eq!(state.continuations.len(),1);assert!(matches!(facts.last(),Some(Observation::WorkEnded{..})));
        let checkpoint=serde_json::to_vec(&state).unwrap();let mut restored:Checkpoint=serde_json::from_slice(&checkpoint).unwrap();let before=facts.len();
        fold(&batch,&mut restored,"alondra/childQuiescent",&json!({"threadId":"parent","childThreadId":"child","activationKey":"spawn"}),&mut facts,|_|invalid()).unwrap();assert_eq!(facts.len(),before);
    }
    #[test]
    fn multiple_native_completions_survive_checkpoint_and_are_taken_up_once(){
        let batch=batch();let mut state=Checkpoint{turn:Some("turn".into()),..Default::default()};let mut facts=vec![];
        for id in ["first","second"]{fold(&batch,&mut state,"item/started",&json!({"item":{"type":"commandExecution","id":id,"processId":id}}),&mut facts,|_|invalid()).unwrap();}
        fold(&batch,&mut state,"turn/completed",&json!({"turn":{"id":"turn","status":"completed"}}),&mut facts,|_|invalid()).unwrap();
        for id in ["first","second"]{fold(&batch,&mut state,"item/completed",&json!({"item":{"type":"commandExecution","id":id,"status":"completed"}}),&mut facts,|_|invalid()).unwrap();}
        assert_eq!(state.continuations.len(),2);
        let mut restored:Checkpoint=serde_json::from_slice(&serde_json::to_vec(&state).unwrap()).unwrap();let before=facts.len();
        fold(&batch,&mut restored,"turn/started",&json!({"turn":{"id":"continued"}}),&mut facts,|_|invalid()).unwrap();
        assert!(matches!(facts[before],Observation::TurnResumed{..}));assert!(matches!(facts[before+1],Observation::ContinuationConsumed{..}));assert!(restored.continuations.is_empty());
    }
    #[test]
    fn a_child_user_item_cannot_consume_its_parents_registered_input(){
        let batch=batch();let mut state=Checkpoint{thread:Some("parent".into()),..Default::default()};let mut facts=vec![];
        fold(&batch,&mut state,"alondra/inputAccepted",&json!({"threadId":"parent","turnId":"turn","inputId":"registered"}),&mut facts,|_|invalid()).unwrap();
        fold(&batch,&mut state,"item/started",&json!({"threadId":"child","turnId":"turn","item":{"type":"userMessage","clientId":"registered"}}),&mut facts,|_|panic!("child cannot look up parent input metadata")).unwrap();assert!(facts.is_empty());assert_eq!(state.accepted.len(),1);
    }
    #[test]
    fn background_command_item_completion_with_running_status_does_not_release_work(){
        let batch=batch();let mut state=Checkpoint{turn:Some("turn".into()),..Default::default()};let mut facts=vec![];
        fold(&batch,&mut state,"item/started",&json!({"item":{"type":"commandExecution","id":"background"}}),&mut facts,|_|invalid()).unwrap();
        fold(&batch,&mut state,"item/completed",&json!({"item":{"type":"commandExecution","id":"background","status":"inProgress"}}),&mut facts,|_|invalid()).unwrap();assert_eq!(state.work.len(),1);assert!(!facts.iter().any(|fact|matches!(fact,Observation::WorkEnded{..})));
    }

    #[test]
    fn child_completion_during_an_existing_parent_turn_requires_a_later_positive_input(){
        let batch=batch();let mut state=Checkpoint{thread:Some("parent".into()),turn:Some("already-running".into()),..Default::default()};let mut facts=vec![];
        fold(&batch,&mut state,"item/completed",&json!({"threadId":"parent","item":{"type":"collabAgentToolCall","id":"spawn","tool":"spawnAgent","receiverThreadIds":["child"],"agentsStates":{"child":{"status":"running"}}}}),&mut facts,|_|invalid()).unwrap();
        fold(&batch,&mut state,"alondra/childQuiescent",&json!({"threadId":"parent","childThreadId":"child","activationKey":"spawn","report":"Public child report"}),&mut facts,|_|invalid()).unwrap();
        assert_eq!(state.continuations.len(),1);assert!(!facts.iter().any(|fact|matches!(fact,Observation::ContinuationConsumed{..})),"an already running turn cannot have consumed a later child report");
        fold(&batch,&mut state,"turn/completed",&json!({"threadId":"parent","turn":{"id":"already-running","status":"completed"}}),&mut facts,|_|invalid()).unwrap();
        state=serde_json::from_slice(&serde_json::to_vec(&state).unwrap()).unwrap();
        let mut next=batch.clone();next.cursor+=1;
        fold(&next,&mut state,"alondra/inputStarting",&json!({"threadId":"parent","inputId":"next-client"}),&mut facts,|_|invalid()).unwrap();
        fold(&batch,&mut state,"alondra/inputAccepted",&json!({"threadId":"parent","turnId":"next","inputId":"next-client"}),&mut facts,|_|invalid()).unwrap();
        fold(&batch,&mut state,"turn/started",&json!({"threadId":"parent","turn":{"id":"next"}}),&mut facts,|_|invalid()).unwrap();assert_eq!(state.continuations.len(),1,"RPC acceptance is not report consumption");
        fold(&batch,&mut state,"item/started",&json!({"threadId":"parent","turnId":"next","item":{"id":"user","type":"userMessage","clientId":"next-client"}}),&mut facts,|_|Ok(InputId::new("next-input").unwrap())).unwrap();
        fold(&batch,&mut state,"item/completed",&json!({"threadId":"parent","turnId":"next","item":{"id":"user","type":"userMessage","clientId":"next-client"}}),&mut facts,|_|panic!("duplicate user echo must not consume twice")).unwrap();
        assert!(state.continuations.is_empty());assert_eq!(facts.iter().filter(|fact|matches!(fact,Observation::ContinuationConsumed{..})).count(),1);
    }

    #[test]
    fn native_auth_refusal_is_positive_no_submission_not_input_consumption(){
        let batch=batch();let mut state=Checkpoint::default();let mut facts=vec![];
        fold(&batch,&mut state,"alondra/inputRefused",&json!({"inputId":"client","nativeSubmitted":false,"code":"paid_authorization_required"}),&mut facts,|id|{assert_eq!(id,"client");Ok(InputId::new("registered").unwrap())}).unwrap();
        assert!(matches!(facts.as_slice(),[Observation::InputDropped{input}] if input.as_str()=="registered"));assert!(state.turn.is_none());assert!(state.accepted.is_empty());
        assert!(fold(&batch,&mut state,"alondra/inputRefused",&json!({"inputId":"client","nativeSubmitted":true}),&mut facts,|_|panic!("uncertain submission cannot be reported as a refusal")).is_err());
    }

    #[test]
    fn compacted_live_scope_keeps_root_turn_and_actual_rerouted_model_without_private_content(){
        let batch=batch();let mut state=Checkpoint{thread:Some("root".into()),model:Some("selected".into()),cwd:Some("/workspace".into()),..Default::default()};let mut facts=vec![];
        fold(&batch,&mut state,"alondra/inputAccepted",&json!({"threadId":"root","turnId":"active","inputId":"client","model":"selected"}),&mut facts,|_|invalid()).unwrap();
        fold(&batch,&mut state,"model/rerouted",&json!({"threadId":"root","turnId":"active","fromModel":"selected","toModel":"actual","reason":"highRiskCyberActivity"}),&mut facts,|_|invalid()).unwrap();
        fold(&batch,&mut state,"item/started",&json!({"threadId":"root","turnId":"active","item":{"id":"user","type":"userMessage","clientId":"client"}}),&mut facts,|_|Ok(InputId::new("input").unwrap())).unwrap();
        let bytes=serde_json::to_vec(&state).unwrap();let scope=live_scope(&bytes).unwrap();assert_eq!(scope.thread,"root");assert_eq!(scope.turn.as_deref(),Some("active"));assert_eq!(scope.model.as_deref(),Some("actual"));assert_eq!(scope.model_turn.as_deref(),Some("active"));assert_eq!(scope.rerouted_turn.as_deref(),Some("active"));
        assert!(!String::from_utf8(bytes).unwrap().contains("reasoning"));assert!(live_scope(b"{}").is_none());
    }

    #[test]
    fn delayed_user_echo_for_an_input_started_before_child_quiescence_cannot_consume_its_later_report(){
        let mut batch=batch();let mut state=Checkpoint{thread:Some("parent".into()),turn:Some("first".into()),..Default::default()};let mut facts=vec![];
        let mut note=|state:&mut Checkpoint,method:&str,params:Value,facts:&mut Vec<Observation>|{batch.cursor+=1;fold(&batch,state,method,&params,facts,|client|Ok(InputId::new(format!("input-{client}")).unwrap())).unwrap();};
        note(&mut state,"item/completed",json!({"threadId":"parent","item":{"type":"collabAgentToolCall","id":"spawn","tool":"spawnAgent","receiverThreadIds":["child"],"agentsStates":{"child":{"status":"running"}}}}),&mut facts);
        note(&mut state,"turn/completed",json!({"threadId":"parent","turn":{"id":"first","status":"completed"}}),&mut facts);
        note(&mut state,"alondra/inputStarting",json!({"threadId":"parent","inputId":"earlier-client"}),&mut facts);
        note(&mut state,"alondra/inputAccepted",json!({"threadId":"parent","turnId":"earlier","inputId":"earlier-client"}),&mut facts);
        note(&mut state,"turn/started",json!({"threadId":"parent","turn":{"id":"earlier"}}),&mut facts);
        note(&mut state,"alondra/childQuiescent",json!({"threadId":"parent","childThreadId":"child","activationKey":"spawn","report":"Public report now available"}),&mut facts);
        state=serde_json::from_slice(&serde_json::to_vec(&state).unwrap()).unwrap();
        note(&mut state,"item/started",json!({"threadId":"parent","turnId":"earlier","item":{"type":"userMessage","id":"late-echo","clientId":"earlier-client"}}),&mut facts);
        assert_eq!(state.continuations.len(),1);assert!(!facts.iter().any(|fact|matches!(fact,Observation::ContinuationConsumed{..})),"arrival of an older accepted input cannot prove a later child report was read");
        note(&mut state,"turn/completed",json!({"threadId":"parent","turn":{"id":"earlier","status":"completed"}}),&mut facts);
        note(&mut state,"alondra/inputStarting",json!({"threadId":"parent","inputId":"new-client"}),&mut facts);
        note(&mut state,"alondra/inputAccepted",json!({"threadId":"parent","turnId":"new","inputId":"new-client"}),&mut facts);
        note(&mut state,"item/started",json!({"threadId":"parent","turnId":"new","item":{"type":"userMessage","id":"new-echo","clientId":"new-client"}}),&mut facts);
        note(&mut state,"item/completed",json!({"threadId":"parent","turnId":"new","item":{"type":"userMessage","id":"new-echo","clientId":"new-client"}}),&mut facts);
        assert!(state.continuations.is_empty());assert!(state.child_continuations.is_empty());assert_eq!(facts.iter().filter(|fact|matches!(fact,Observation::ContinuationConsumed{..})).count(),1);
    }

    #[test]
    fn a_policy_refusal_survives_raw_ack_and_checkpoint_roundtrip(){
        let state=Checkpoint{cursor:3,blocked_policy:Some("Restore the original native account".into()),..Default::default()};let bytes=serde_json::to_vec(&state).unwrap();
        assert_eq!(blocked_policy(&bytes).as_deref(),Some("Restore the original native account"));assert!(live_scope(&bytes).is_none(),"blocked initial auth never fabricates a native thread");
        let legacy:Checkpoint=serde_json::from_slice(br#"{"cursor":0,"initialized":false,"thread":null,"turn":null,"work":{},"asked":{},"continuation":null}"#).unwrap();assert!(legacy.blocked_policy.is_none());
    }

}
