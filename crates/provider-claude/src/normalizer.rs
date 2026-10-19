use crate::records::{InputRecord, SessionRecord};
use alondra_agent_application::{Normalization, ObservationMetadata, ObservationNormalizer};
use alondra_agent_protocol::{
    Body, DecisionKind, DecisionOutcome, Event, Fence, FramedReader, NativeCorrelation,
    Observation as WireObservation, SdkEventKind,
};
use alondra_foundation::{ContinuationId, MessageUuid, TurnId, WorkId};
use alondra_provider_contract::{AdapterError, ApiFailure, Observation, OrderedObservation};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
use std::io::Cursor;

const CHECKPOINT_FORMAT: u16 = 1;
/// More decisions open at once than the host ever holds pending.
const MAX_ASKED: usize = 64;

pub(super) struct Normalizer {
    pub session: Option<SessionRecord>,
    pub fence: Option<Fence>,
    /// The first cursor of a host launched after its execution's previous host ended: the turn and
    /// work that host's Claude had open ended with it.
    pub first_cursor: Option<u64>,
    /// The events being committed are the last of a host that ended: the exit of its Claude and the
    /// end of its SDK iterator are evidence, not facts.
    pub host_ended: bool,
}
#[derive(Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Checkpoint {
    format: u16,
    cursor: u64,
    initialized: bool,
    turn: Option<LogicalTurn>,
    work: BTreeMap<String, LogicalWork>,
    seen_tasks: BTreeSet<String>,
    /// The decisions Claude asked for and still waits on: the request id of each, by epoch.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    asked: BTreeMap<u64, String>,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct LogicalTurn {
    // Alondra logical identity assigned at an observed SDK root-message boundary.
    id: String,
    consumed: BTreeSet<MessageUuid>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    answered: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    error: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    resets_at: Option<u64>,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct LogicalWork {
    run: Option<String>,
    id: String,
}
impl ObservationNormalizer for Normalizer {
    fn normalize(
        &self,
        batch: &OrderedObservation,
        metadata: &ObservationMetadata<'_, '_>,
    ) -> Result<Normalization, AdapterError> {
        if &batch.execution != metadata.execution()
            || batch.cursor != metadata.cursor()
            || batch.evidence.is_empty()
            || batch.evidence.len() > alondra_agent_protocol::MAX_FRAME_BYTES
        {
            return Err(AdapterError::InvalidObservation);
        }
        let mut framed = (batch.evidence.len() as u32).to_be_bytes().to_vec();
        framed.extend_from_slice(&batch.evidence);
        let received = FramedReader::new(Cursor::new(framed))
            .read_received()
            .map_err(|_| AdapterError::InvalidObservation)?
            .ok_or(AdapterError::InvalidObservation)?;
        let Body::Event(event) = &received.frame().body else {
            return Err(AdapterError::InvalidObservation);
        };
        let alondra_agent_protocol::Execution::Task(execution) = &event.execution else {
            return Err(AdapterError::InvalidObservation);
        };
        if event.cursor != batch.cursor
            || self.fence.as_ref() != Some(&event.fence)
            || execution.task_id != *batch.execution.task()
            || execution.agent_id != *batch.execution.node()
            || execution.conversation_id != batch.execution.conversation().as_str()
            || execution.process_instance_id != batch.execution.instance().as_str()
            || execution.generation != batch.execution.generation()
            || event.fence.host_id != batch.execution.host().as_str()
        {
            return Err(AdapterError::InvalidObservation);
        }
        event
            .validate_for_application()
            .map_err(|_| AdapterError::Unsupported)?;
        let prior = metadata.checkpoint().map_err(storage)?;
        let mut checkpoint = if let Some(prior) = prior {
            let decoded: Checkpoint = serde_json::from_slice(prior.bytes())
                .map_err(|_| AdapterError::InvalidObservation)?;
            if decoded.format != CHECKPOINT_FORMAT
                || decoded.cursor != prior.cursor()
                || decoded.cursor.checked_add(1) != Some(batch.cursor)
            {
                return Err(AdapterError::InvalidObservation);
            }
            decoded
        } else {
            if batch.cursor != 1 {
                return Err(AdapterError::InvalidObservation);
            }
            Checkpoint {
                format: CHECKPOINT_FORMAT,
                ..Checkpoint::default()
            }
        };
        if self.first_cursor == Some(batch.cursor) {
            checkpoint.turn = None;
            checkpoint.work.clear();
        }
        let session = self
            .session
            .as_ref()
            .ok_or(AdapterError::InvalidObservation)?;
        if self.host_ended && ended_with_its_host(&event.observation) {
            checkpoint.cursor = batch.cursor;
            let bytes = serde_json::to_vec(&checkpoint).map_err(|_| AdapterError::InvalidObservation)?;
            return Normalization::new(Vec::new(), bytes);
        }
        let facts = fold(event, session, &mut checkpoint, |ids| {
            let keys = ids
                .iter()
                .map(|id| {
                    serde_json::to_vec(&InputRecord {
                        message_id: id.clone(),
                    })
                    .map_err(|_| AdapterError::InvalidObservation)
                })
                .collect::<Result<Vec<_>, _>>()?;
            let records = metadata.lookup_inputs(&keys).map_err(storage)?;
            if records.len() != ids.len() {
                return Err(AdapterError::InvalidObservation);
            }
            let mut mappings = BTreeMap::new();
            for record in records {
                let native: InputRecord = serde_json::from_slice(record.metadata())
                    .map_err(|_| AdapterError::InvalidObservation)?;
                if !ids.contains(&native.message_id)
                    || mappings
                        .insert(native.message_id, record.input().clone())
                        .is_some()
                {
                    return Err(AdapterError::InvalidObservation);
                }
            }
            ids.iter()
                .map(|id| mappings.remove(id).ok_or(AdapterError::InvalidObservation))
                .collect()
        })?;
        checkpoint.cursor = batch.cursor;
        let bytes =
            serde_json::to_vec(&checkpoint).map_err(|_| AdapterError::InvalidObservation)?;
        Normalization::new(facts, bytes)
    }
}
/// What a host leaves behind as it ends: its Claude's exit, and the end of the SDK iterator that
/// read it (closed by the host, or cut by the exit).
fn ended_with_its_host(observation: &WireObservation) -> bool {
    match observation {
        WireObservation::ProcessExited { .. } => true,
        WireObservation::Sdk { kind: SdkEventKind::Diagnostic, failures, .. } => {
            !failures.is_empty()
                && failures.iter().all(|failure| {
                    matches!(failure.surface, alondra_agent_protocol::FailureSurface::Control | alondra_agent_protocol::FailureSurface::Iterator)
                })
        }
        _ => false,
    }
}
fn fold(
    event: &Event,
    session: &SessionRecord,
    state: &mut Checkpoint,
    lookup: impl FnOnce(&[MessageUuid]) -> Result<Vec<alondra_foundation::InputId>, AdapterError>,
) -> Result<Vec<Observation>, AdapterError> {
    match &event.observation {
        WireObservation::DecisionRequested { request_id, tool_use_id, kind, payload } => {
            if !state.initialized
                || state.asked.len() >= MAX_ASKED
                || state.asked.values().any(|open| *open == request_id.0)
            {
                return Err(AdapterError::InvalidObservation);
            }
            state.asked.insert(event.cursor, request_id.0.clone());
            return Ok(vec![Observation::Asked {
                request: request_id.0.clone(),
                epoch: event.cursor,
                kind: match kind {
                    DecisionKind::Permission => alondra_provider_contract::DecisionKind::Permission,
                    DecisionKind::Question => alondra_provider_contract::DecisionKind::Question,
                    DecisionKind::Plan => alondra_provider_contract::DecisionKind::Plan,
                    DecisionKind::Elicitation => alondra_provider_contract::DecisionKind::Elicitation,
                },
                tool_use: tool_use_id.clone(),
                payload: serde_json::to_vec(payload).map_err(|_| AdapterError::InvalidObservation)?,
            }]);
        }
        WireObservation::DecisionSettled { request_id, epoch, outcome } => {
            if state.asked.get(epoch) != Some(&request_id.0) {
                return Err(AdapterError::InvalidObservation);
            }
            state.asked.remove(epoch);
            return Ok(vec![Observation::DecisionSettled {
                epoch: *epoch,
                answered: *outcome == DecisionOutcome::Answered,
                continued: state.turn.is_some(),
            }]);
        }
        _ => {}
    }
    let WireObservation::Sdk {
        kind,
        native,
        payload,
        failures,
    } = &event.observation
    else {
        return Err(AdapterError::Unsupported);
    };
    if *kind == SdkEventKind::Diagnostic
        && payload.get("source").and_then(Value::as_str) == Some("stderr")
    {
        let object = payload
            .as_object()
            .ok_or(AdapterError::InvalidObservation)?;
        if object.len() != 3
            || object.get("text").and_then(Value::as_str).is_none()
            || object.get("truncated_at_bytes").and_then(Value::as_u64) != Some(16_384)
            || native != &NativeCorrelation::default()
            || !failures.is_empty()
        {
            return Err(AdapterError::InvalidObservation);
        }
        return Ok(Vec::new());
    }
    if *kind == SdkEventKind::ControlResult {
        // What a read-only control answered, validated by the protocol: evidence, no fact.
        if native != &NativeCorrelation::default() || !failures.is_empty() {
            return Err(AdapterError::InvalidObservation);
        }
        return Ok(Vec::new());
    }
    if payload.get("session_id").and_then(Value::as_str) != Some(session.session_id.as_str()) {
        return Err(AdapterError::InvalidObservation);
    }
    let kind = string(payload, "type")?;
    let mut facts = Vec::new();
    if kind == "command_lifecycle" {
        // The native Claude's trace of one of our inputs, even before init: evidence, no fact.
        lookup(&[native_uuid(payload, "command_uuid")?])?;
        return Ok(facts);
    }
    if kind == "system" && payload.get("subtype").and_then(Value::as_str) == Some("init") {
        if state.initialized {
            // The native Claude repeats init as each new prompt starts; never inside a turn.
            if state.turn.is_some() {
                return Err(AdapterError::InvalidObservation);
            }
            return Ok(facts);
        }
        state.initialized = true;
        facts.push(Observation::Available);
        return Ok(facts);
    }
    if kind == "system"
        && matches!(
            payload.get("subtype").and_then(Value::as_str),
            Some("hook_started" | "hook_progress" | "hook_response")
        )
    {
        // The hooks of the user's and Alondra's plugins: SessionStart's run before init.
        return Ok(facts);
    }
    if !state.initialized {
        return Err(AdapterError::InvalidObservation);
    }
    if kind == "system" {
        match payload.get("subtype").and_then(Value::as_str) {
            Some("task_started") => {
                let task = string(payload, "task_id")?;
                let turn = state.turn.as_ref().ok_or(AdapterError::Unsupported)?;
                let run = optional_string(payload, "run_id")?;
                if state.work.contains_key(task)
                    || (state.seen_tasks.contains(task) && run.is_none())
                {
                    return Err(AdapterError::Unsupported);
                }
                let id = format!("claude-task:{}", native_uuid(payload, "uuid")?.as_str());
                state.seen_tasks.insert(task.into());
                state.work.insert(
                    task.into(),
                    LogicalWork {
                        run,
                        id: id.clone(),
                    },
                );
                facts.push(Observation::WorkStarted {
                    turn: TurnId::new(turn.id.clone())
                        .map_err(|_| AdapterError::InvalidObservation)?,
                    work: WorkId::new(id).map_err(|_| AdapterError::InvalidObservation)?,
                });
            }
            Some("task_notification") => {
                let task = string(payload, "task_id")?;
                if !matches!(
                    payload.get("status").and_then(Value::as_str),
                    Some("completed" | "failed" | "stopped")
                ) || payload.get("reason").is_some()
                {
                    return Err(AdapterError::Unsupported);
                }
                let run = optional_string(payload, "run_id")?;
                let work = state.work.get(task).ok_or(AdapterError::Unsupported)?;
                if work.run != run {
                    return Err(AdapterError::Unsupported);
                }
                let work = state.work.remove(task).ok_or(AdapterError::Unsupported)?;
                let basis = native_uuid(payload, "uuid")?;
                let continuation = ContinuationId::new(format!("claude-notification:{}", basis.as_str()))
                    .map_err(|_| AdapterError::InvalidObservation)?;
                facts.push(Observation::WorkEnded {
                    work: WorkId::new(work.id).map_err(|_| AdapterError::InvalidObservation)?,
                    continuation: continuation.clone(),
                });
                // Claude Code tracks a foreground command that runs for seconds as its own work:
                // it ends inside the turn that waits for it, and only work that outlives its turn
                // wakes another.
                if state.turn.is_some() {
                    facts.push(Observation::ContinuationConsumed { continuation });
                }
            }
            Some("conversation_reset") => return Err(AdapterError::Unsupported),
            _ => {}
        }
        return Ok(facts);
    }
    if kind == "rate_limit_event" {
        let info = payload.get("rate_limit_info");
        if info.and_then(|info| info.get("status")).and_then(Value::as_str) == Some("rejected") {
            if let Some(turn) = state.turn.as_mut() {
                turn.resets_at = info.and_then(|info| info.get("resetsAt")).and_then(Value::as_u64);
            }
        }
        return Ok(facts);
    }
    if kind != "assistant" && kind != "result" && kind != "stream_event" {
        return Ok(facts);
    }
    if payload
        .get("parent_tool_use_id")
        .is_some_and(|value| !value.is_null())
    {
        if payload.get("user_message_uuids").is_some() {
            return Err(AdapterError::InvalidObservation);
        }
        return Ok(facts);
    }
    if kind == "stream_event" {
        // A partial stream event is not the root assistant/result boundary for our logical turn.
        if payload.get("user_message_uuids").is_some() {
            return Err(AdapterError::Unsupported);
        }
        return Ok(facts);
    }
    let mut consumed = Vec::new();
    if let Some(list) = payload.get("user_message_uuids") {
        let list = list.as_array().ok_or(AdapterError::InvalidObservation)?;
        if list.is_empty() || list.len() > 64 {
            return Err(AdapterError::InvalidObservation);
        }
        let mut unique = BTreeSet::new();
        for value in list {
            let id = MessageUuid::new(value.as_str().ok_or(AdapterError::InvalidObservation)?)
                .map_err(|_| AdapterError::InvalidObservation)?;
            if !unique.insert(id.clone()) {
                return Err(AdapterError::InvalidObservation);
            }
            consumed.push(id);
        }
        let singular = native_uuid(payload, "user_message_uuid")?;
        if !unique.contains(&singular) {
            return Err(AdapterError::InvalidObservation);
        }
        let inputs = lookup(&consumed)?;
        if state.turn.is_none() {
            let basis = native_uuid(payload, "uuid")?;
            let logical = format!("claude-message:{}", basis.as_str());
            facts.push(Observation::TurnStarted {
                input: inputs[0].clone(),
                turn: TurnId::new(logical.clone()).map_err(|_| AdapterError::InvalidObservation)?,
            });
            state.turn = Some(LogicalTurn {
                id: logical,
                consumed: BTreeSet::from([consumed[0].clone()]),
                answered: false,
                error: None,
                resets_at: None,
            });
        }
        let turn = state
            .turn
            .as_mut()
            .ok_or(AdapterError::InvalidObservation)?;
        for (id, input) in consumed.iter().zip(inputs) {
            if turn.consumed.insert(id.clone()) {
                facts.push(Observation::InputConsumed { input });
            }
        }
        if turn.consumed.len() > 64 {
            return Err(AdapterError::InvalidObservation);
        }
    }
    if kind == "assistant" {
        if let Some(turn) = state.turn.as_mut() {
            match payload.get("error") {
                // The synthetic message Claude Code ends a turn with once a Claude API error
                // outlasted its own retries.
                Some(error) => turn.error = Some(error_code(error)?),
                None if !turn.answered && turn.error.is_none() => {
                    turn.answered = true;
                    facts.push(Observation::ModelAnswered);
                }
                None => {}
            }
        }
    }
    if kind == "result" {
        // A singular failed-delivery echo alone cannot prove input consumption or a turn.
        let turn = state.turn.as_ref().ok_or(AdapterError::Unsupported)?;
        if consumed.is_empty() {
            let singular = native_uuid(payload, "user_message_uuid")?;
            if !turn.consumed.contains(&singular) {
                return Err(AdapterError::Unsupported);
            }
        }
        let failure = match &turn.error {
            Some(error) => Some(ApiFailure {
                error: error.clone(),
                status: match payload.get("api_error_status") {
                    None | Some(Value::Null) => None,
                    Some(status) => Some(
                        status
                            .as_u64()
                            .and_then(|status| u16::try_from(status).ok())
                            .ok_or(AdapterError::InvalidObservation)?,
                    ),
                },
                resets_at: turn.resets_at,
            }),
            None => None,
        };
        facts.push(Observation::TurnEnded {
            turn: TurnId::new(turn.id.clone()).map_err(|_| AdapterError::InvalidObservation)?,
            failure,
        });
        state.turn = None;
    }
    Ok(facts)
}
fn error_code(value: &Value) -> Result<String, AdapterError> {
    let code = value.as_str().ok_or(AdapterError::InvalidObservation)?;
    if code.is_empty()
        || code.len() > 64
        || !code.bytes().all(|b| b.is_ascii_lowercase() || b == b'_')
    {
        return Err(AdapterError::InvalidObservation);
    }
    Ok(code.to_owned())
}
fn native_uuid(payload: &Value, key: &str) -> Result<MessageUuid, AdapterError> {
    MessageUuid::new(string(payload, key)?).map_err(|_| AdapterError::InvalidObservation)
}
fn string<'a>(payload: &'a Value, key: &str) -> Result<&'a str, AdapterError> {
    let value = payload
        .get(key)
        .and_then(Value::as_str)
        .ok_or(AdapterError::InvalidObservation)?;
    if value.is_empty() || value.len() > 256 || value.contains('\0') {
        return Err(AdapterError::InvalidObservation);
    }
    Ok(value)
}
fn storage(error: alondra_store::Error) -> AdapterError {
    AdapterError::Storage(error.to_string())
}

fn optional_string(payload: &Value, key: &str) -> Result<Option<String>, AdapterError> {
    payload
        .get(key)
        .map(|_| string(payload, key).map(str::to_owned))
        .transpose()
}

#[cfg(test)]
mod tests {
    use super::*;
    use alondra_agent_protocol::{Execution, NativeCorrelation, SdkEventKind, TaskExecution};
    use alondra_foundation::{AgentId, DataEpoch, InputId, TaskId};
    use serde_json::json;

    const SESSION: &str = "10000000-0000-4000-8000-000000000001";
    const FIRST: &str = "10000000-0000-4000-8000-000000000002";
    const SECOND: &str = "10000000-0000-4000-8000-000000000003";
    const REPLY: &str = "10000000-0000-4000-8000-000000000004";
    const NOTIFIED: &str = "10000000-0000-4000-8000-000000000005";
    fn event(mut payload: Value) -> Event {
        payload["session_id"] = json!(SESSION);
        Event {
            fence: Fence {
                profile_id: "fixture".into(),
                account_id: "fixture".into(),
                data_epoch: DataEpoch::from("epoch"),
                host_id: "host".into(),
            },
            execution: Execution::Task(TaskExecution {
                task_id: TaskId::from("task"),
                agent_id: AgentId::from("node"),
                conversation_id: "conversation".into(),
                process_instance_id: "process".into(),
                generation: 1,
            }),
            cursor: 1,
            observation: WireObservation::Sdk {
                kind: SdkEventKind::Message,
                native: NativeCorrelation::default(),
                payload,
                failures: vec![],
            },
        }
    }
    fn session() -> SessionRecord {
        SessionRecord {
            session_id: MessageUuid::new(SESSION).unwrap(),
        }
    }
    fn state() -> Checkpoint {
        Checkpoint {
            format: CHECKPOINT_FORMAT,
            initialized: true,
            ..Checkpoint::default()
        }
    }
    fn input(ids: &[MessageUuid]) -> Result<Vec<InputId>, AdapterError> {
        Ok(ids
            .iter()
            .map(|id| InputId::new(id.as_str()).unwrap())
            .collect())
    }
    // What the native Claude 2.1.300 sends around each prompt: its lifecycle, from before init,
    // and init again as the next prompt starts. Evidence of our own inputs, with no fact.
    #[test]
    fn the_native_prompt_lifecycle_and_a_repeated_init_between_turns_record_nothing() {
        let mut state = Checkpoint { format: CHECKPOINT_FORMAT, ..Checkpoint::default() };
        let queued = event(json!({"type":"command_lifecycle","command_uuid":FIRST,"state":"queued","uuid":REPLY}));
        assert_eq!(fold(&queued, &session(), &mut state, input).unwrap(), vec![]);
        let foreign = |_: &[MessageUuid]| Err(AdapterError::InvalidObservation);
        assert!(matches!(fold(&queued, &session(), &mut state, foreign), Err(AdapterError::InvalidObservation)), "only an input of ours");
        let init = event(json!({"type":"system","subtype":"init","uuid":REPLY}));
        assert_eq!(fold(&init, &session(), &mut state, input).unwrap(), vec![Observation::Available]);
        let again = fold(&init, &session(), &mut state, input);
        assert!(matches!(&again, Ok(facts) if facts.is_empty()), "init again between turns records nothing: {again:?}");
        let assistant = event(json!({"type":"assistant","uuid":REPLY,"parent_tool_use_id":null,"user_message_uuid":FIRST,"user_message_uuids":[FIRST]}));
        fold(&assistant, &session(), &mut state, input).unwrap();
        assert!(matches!(fold(&init, &session(), &mut state, input), Err(AdapterError::InvalidObservation)), "never inside a turn");
    }
    #[test]
    fn result_only_plural_preserves_consumption_and_pending_terminal_boundary() {
        let result = event(
            json!({"type":"result","uuid":REPLY,"subtype":"success","user_message_uuid":SECOND,"user_message_uuids":[FIRST,SECOND]}),
        );
        let mut state = state();
        let facts = fold(&result, &session(), &mut state, input).unwrap();
        assert_eq!(
            facts,
            vec![
                Observation::TurnStarted {
                    input: InputId::new(FIRST).unwrap(),
                    turn: TurnId::new(format!("claude-message:{REPLY}")).unwrap()
                },
                Observation::InputConsumed {
                    input: InputId::new(SECOND).unwrap()
                },
                Observation::TurnEnded {
                    turn: TurnId::new(format!("claude-message:{REPLY}")).unwrap(),
                    failure: None,
                }
            ]
        );
        assert!(state.turn.is_none());
    }
    #[test]
    fn failed_delivery_singular_does_not_establish_consumption_or_turn() {
        let result = event(
            json!({"type":"result","uuid":REPLY,"subtype":"error_during_execution","is_error":true,"num_turns":0,"user_message_uuid":FIRST}),
        );
        let mut state = state();
        assert!(matches!(
            fold(&result, &session(), &mut state, |_| panic!(
                "singular echo must not query consumption"
            )),
            Err(AdapterError::Unsupported)
        ));
        assert!(state.turn.is_none());
    }
    #[test]
    fn error_result_plural_still_proves_actual_consumption() {
        let result = event(
            json!({"type":"result","uuid":REPLY,"subtype":"error_during_execution","is_error":true,"user_message_uuid":FIRST,"user_message_uuids":[FIRST]}),
        );
        let facts = fold(&result, &session(), &mut state(), input).unwrap();
        assert!(facts.contains(&Observation::TurnStarted {
            input: InputId::new(FIRST).unwrap(),
            turn: TurnId::new(format!("claude-message:{REPLY}")).unwrap(),
        }));
        assert!(!facts.contains(&Observation::InputConsumed {
            input: InputId::new(FIRST).unwrap()
        }));
        assert!(matches!(facts.last(), Some(Observation::TurnEnded { .. })));
    }
    #[test]
    fn repeated_assistant_consumption_does_not_start_another_turn() {
        let assistant = event(
            json!({"type":"assistant","uuid":REPLY,"parent_tool_use_id":null,"user_message_uuid":FIRST,"user_message_uuids":[FIRST]}),
        );
        let mut state = state();
        assert_eq!(
            fold(&assistant, &session(), &mut state, input)
                .unwrap()
                .len(),
            2,
            "the turn starts and the model answered"
        );
        assert!(
            fold(&assistant, &session(), &mut state, input)
                .unwrap()
                .is_empty()
        );
        let result = event(json!({"type":"result","uuid":SECOND,"user_message_uuid":FIRST}));
        assert_eq!(
            fold(&result, &session(), &mut state, input).unwrap(),
            vec![Observation::TurnEnded {
                turn: TurnId::new(format!("claude-message:{REPLY}")).unwrap(),
                failure: None,
            }]
        );
    }
    #[test]
    fn restarted_or_wrong_run_notification_cannot_end_work() {
        let mut state = state();
        state.turn = Some(LogicalTurn {
            id: "turn".into(),
            consumed: BTreeSet::new(),
            answered: false,
            error: None,
            resets_at: None,
        });
        let started = event(
            json!({"type":"system","subtype":"task_started","uuid":REPLY,"task_id":"task-native","run_id":"run-first"}),
        );
        let facts = fold(&started, &session(), &mut state, input).unwrap();
        assert_eq!(
            facts,
            vec![Observation::WorkStarted {
                turn: TurnId::new("turn").unwrap(),
                work: WorkId::new(format!("claude-task:{REPLY}")).unwrap()
            }]
        );
        for payload in [
            json!({"type":"system","subtype":"task_notification","uuid":SECOND,"task_id":"task-native","run_id":"run-other","status":"completed"}),
            json!({"type":"system","subtype":"task_notification","uuid":SECOND,"task_id":"task-native","run_id":"run-first","status":"stopped","reason":"worker_restart"}),
        ] {
            assert!(matches!(
                fold(&event(payload), &session(), &mut state, input),
                Err(AdapterError::Unsupported)
            ));
            assert!(state.work.contains_key("task-native"));
        }
        let ended = event(
            json!({"type":"system","subtype":"task_notification","uuid":SECOND,"task_id":"task-native","run_id":"run-first","status":"completed"}),
        );
        let continuation = ContinuationId::new(format!("claude-notification:{SECOND}")).unwrap();
        assert_eq!(
            fold(&ended, &session(), &mut state, input).unwrap(),
            vec![
                Observation::WorkEnded { work: WorkId::new(format!("claude-task:{REPLY}")).unwrap(), continuation: continuation.clone() },
                Observation::ContinuationConsumed { continuation },
            ]
        );
        assert!(state.work.is_empty());
    }
    // What the native Claude 2.1.300 sends for a foreground Bash that runs ≈3 s or longer: its own
    // work, not backgrounded, that ends before the turn's result.
    #[test]
    fn foreground_work_that_ends_inside_its_turn_is_consumed_by_that_turn_which_then_ends() {
        let mut state = state();
        let assistant = event(json!({"type":"assistant","uuid":REPLY,"parent_tool_use_id":null,"user_message_uuid":FIRST,"user_message_uuids":[FIRST]}));
        fold(&assistant, &session(), &mut state, input).unwrap();
        let turn = TurnId::new(format!("claude-message:{REPLY}")).unwrap();
        let started = event(json!({"type":"system","subtype":"task_started","uuid":SECOND,"task_id":"bash-1","tool_use_id":"toolu_1","task_type":"local_bash","is_backgrounded":false}));
        let work = WorkId::new(format!("claude-task:{SECOND}")).unwrap();
        assert_eq!(fold(&started, &session(), &mut state, input).unwrap(), vec![Observation::WorkStarted { turn: turn.clone(), work: work.clone() }]);
        let ended = event(json!({"type":"system","subtype":"task_notification","uuid":NOTIFIED,"task_id":"bash-1","tool_use_id":"toolu_1","status":"completed"}));
        let continuation = ContinuationId::new(format!("claude-notification:{NOTIFIED}")).unwrap();
        assert_eq!(fold(&ended, &session(), &mut state, input).unwrap(), vec![
            Observation::WorkEnded { work, continuation: continuation.clone() },
            Observation::ContinuationConsumed { continuation },
        ], "the turn that waited for the command goes on with it");
        let result = event(json!({"type":"result","uuid":SECOND,"user_message_uuid":FIRST}));
        assert_eq!(fold(&result, &session(), &mut state, input).unwrap(), vec![Observation::TurnEnded { turn, failure: None }]);
    }
    // Work the turn leaves running ends after the turn's result: its continuation stays, to wake
    // the turn that takes it.
    #[test]
    fn background_work_that_ends_after_its_turn_keeps_its_continuation() {
        let mut state = state();
        let assistant = event(json!({"type":"assistant","uuid":REPLY,"parent_tool_use_id":null,"user_message_uuid":FIRST,"user_message_uuids":[FIRST]}));
        fold(&assistant, &session(), &mut state, input).unwrap();
        let started = event(json!({"type":"system","subtype":"task_started","uuid":SECOND,"task_id":"bash-1","tool_use_id":"toolu_1","task_type":"local_bash","is_backgrounded":true}));
        fold(&started, &session(), &mut state, input).unwrap();
        let result = event(json!({"type":"result","uuid":SECOND,"user_message_uuid":FIRST}));
        fold(&result, &session(), &mut state, input).unwrap();
        let ended = event(json!({"type":"system","subtype":"task_notification","uuid":NOTIFIED,"task_id":"bash-1","tool_use_id":"toolu_1","status":"completed"}));
        assert_eq!(fold(&ended, &session(), &mut state, input).unwrap(), vec![Observation::WorkEnded {
            work: WorkId::new(format!("claude-task:{SECOND}")).unwrap(),
            continuation: ContinuationId::new(format!("claude-notification:{NOTIFIED}")).unwrap(),
        }]);
    }
    #[test]
    fn a_control_result_is_evidence_without_facts_and_carries_no_failure() {
        let mut result = event(json!({"control":"mcp_status","command_id":"lifecycle-control-1","servers":[{"name":"fixture","status":"connected"}]}));
        let WireObservation::Sdk { kind, payload, .. } = &mut result.observation else { unreachable!() };
        *kind = SdkEventKind::ControlResult;
        payload.as_object_mut().unwrap().remove("session_id");
        result.validate_for_application().unwrap();
        let mut state = state();
        assert_eq!(fold(&result, &session(), &mut state, |_| panic!("a control result consumes no input")).unwrap(), vec![]);
        let WireObservation::Sdk { failures, .. } = &mut result.observation else { unreachable!() };
        failures.push(alondra_agent_protocol::SdkFailure { surface: alondra_agent_protocol::FailureSurface::Result, code: "error_during_execution".into(), details: json!({}) });
        assert!(matches!(fold(&result, &session(), &mut state, input), Err(AdapterError::InvalidObservation)), "a failed control result is not evidence");
    }
    // What a `/compact` input produces: the boundary records nothing, and its result ends the
    // turn it consumed like any other input.
    #[test]
    fn a_compaction_turn_records_its_boundary_as_nothing_and_ends_like_any_input_turn() {
        let mut state = state();
        for status in [json!({"type":"system","subtype":"status","status":"compacting","uuid":REPLY}),
            json!({"type":"system","subtype":"compact_boundary","compact_metadata":{"trigger":"manual","pre_tokens":1},"uuid":REPLY})] {
            assert_eq!(fold(&event(status), &session(), &mut state, input).unwrap(), vec![]);
        }
        let result = event(json!({"type":"result","uuid":REPLY,"subtype":"success","is_error":false,"user_message_uuid":FIRST,"user_message_uuids":[FIRST]}));
        let turn = TurnId::new(format!("claude-message:{REPLY}")).unwrap();
        assert_eq!(fold(&result, &session(), &mut state, input).unwrap(), vec![
            Observation::TurnStarted { input: InputId::new(FIRST).unwrap(), turn: turn.clone() },
            Observation::TurnEnded { turn, failure: None },
        ]);
    }
    fn decision(cursor: u64, observation: WireObservation) -> Event {
        let mut decision = event(json!({}));
        decision.cursor = cursor;
        decision.observation = observation;
        decision
    }
    fn requested(cursor: u64, request: &str) -> Event {
        decision(cursor, WireObservation::DecisionRequested {
            request_id: alondra_foundation::RequestId(request.into()),
            tool_use_id: Some("toolu_1".into()),
            kind: DecisionKind::Permission,
            payload: json!({"tool": "Bash", "input": {"command": "ls"}, "request": {}}),
        })
    }
    fn settled(cursor: u64, request: &str, epoch: u64, outcome: DecisionOutcome) -> Event {
        decision(cursor, WireObservation::DecisionSettled { request_id: alondra_foundation::RequestId(request.into()), epoch, outcome })
    }
    // A decision Claude asks for is asked at the host's cursor of its request, with the payload
    // the host kept; its settlement says whether the turn that asked goes on with it.
    #[test]
    fn a_decision_request_is_asked_at_its_cursor_and_settles_once_with_the_turn_it_continues() {
        let mut state = state();
        let assistant = event(json!({"type":"assistant","uuid":REPLY,"parent_tool_use_id":null,"user_message_uuid":FIRST,"user_message_uuids":[FIRST]}));
        fold(&assistant, &session(), &mut state, input).unwrap();
        let asked = fold(&requested(7, "decision-1"), &session(), &mut state, input).unwrap();
        let [Observation::Asked { request, epoch, kind, tool_use, payload }] = asked.as_slice() else { panic!("{asked:?}") };
        assert_eq!((request.as_str(), *epoch, *kind, tool_use.as_deref()), ("decision-1", 7, alondra_provider_contract::DecisionKind::Permission, Some("toolu_1")));
        assert_eq!(serde_json::from_slice::<Value>(payload).unwrap(), json!({"tool": "Bash", "input": {"command": "ls"}, "request": {}}));
        assert_eq!(fold(&settled(8, "decision-1", 7, DecisionOutcome::Answered), &session(), &mut state, input).unwrap(),
            vec![Observation::DecisionSettled { epoch: 7, answered: true, continued: true }]);
        assert!(state.asked.is_empty());
        assert!(matches!(fold(&settled(9, "decision-1", 7, DecisionOutcome::Answered), &session(), &mut state, input), Err(AdapterError::InvalidObservation)), "settled once");
        state.turn = None;
        fold(&requested(10, "decision-2"), &session(), &mut state, input).unwrap();
        assert_eq!(fold(&settled(11, "decision-2", 10, DecisionOutcome::Cancelled), &session(), &mut state, input).unwrap(),
            vec![Observation::DecisionSettled { epoch: 10, answered: false, continued: false }]);
    }
    #[test]
    fn a_settlement_of_an_unknown_epoch_or_another_request_and_a_repeated_request_are_refused() {
        let mut state = state();
        fold(&requested(3, "decision-1"), &session(), &mut state, input).unwrap();
        assert!(matches!(fold(&requested(4, "decision-1"), &session(), &mut state, input), Err(AdapterError::InvalidObservation)), "the same request twice");
        assert!(matches!(fold(&settled(5, "decision-1", 4, DecisionOutcome::Answered), &session(), &mut state, input), Err(AdapterError::InvalidObservation)), "an epoch nobody asked at");
        assert!(matches!(fold(&settled(5, "decision-9", 3, DecisionOutcome::Answered), &session(), &mut state, input), Err(AdapterError::InvalidObservation)), "another request at that epoch");
        assert_eq!(state.asked, BTreeMap::from([(3, "decision-1".to_owned())]));
        let mut fresh = Checkpoint { format: CHECKPOINT_FORMAT, ..Checkpoint::default() };
        assert!(matches!(fold(&requested(1, "decision-1"), &session(), &mut fresh, input), Err(AdapterError::InvalidObservation)), "nothing is asked before Claude is up");
    }
    #[test]
    fn provider_owned_retry_and_allowed_rate_limit_do_not_end_business_execution() {
        for payload in [
            json!({"type":"system","subtype":"api_retry","attempt":1,"error_status":401}),
            json!({"type":"rate_limit_event","rate_limit_info":{"status":"allowed_warning"}}),
        ] {
            assert!(
                fold(&event(payload), &session(), &mut state(), input)
                    .unwrap()
                    .is_empty()
            );
        }
    }
    fn turn_end(state: &mut Checkpoint, payloads: Vec<Value>) -> Vec<Observation> {
        payloads
            .into_iter()
            .flat_map(|payload| fold(&event(payload), &session(), state, input).unwrap())
            .collect()
    }
    fn failure(facts: &[Observation]) -> Option<ApiFailure> {
        match facts.last() {
            Some(Observation::TurnEnded { failure, .. }) => failure.clone(),
            other => panic!("the turn did not end: {other:?}"),
        }
    }
    // Claude Code 2.1.300 retries a 401 itself and says so with api_retry, also between two
    // requests of one turn; the turn then goes on with an answer of the model. A retry is never a
    // logout.
    #[test]
    fn a_retried_authentication_error_that_the_model_then_answers_ends_the_turn_without_failure() {
        let mut state = state();
        let facts = turn_end(&mut state, vec![
            json!({"type":"system","subtype":"api_retry","attempt":1,"max_retries":10,"retry_delay_ms":500,"error_status":401,"error":"authentication_failed","uuid":SECOND}),
            json!({"type":"assistant","uuid":REPLY,"parent_tool_use_id":null,"user_message_uuid":FIRST,"user_message_uuids":[FIRST]}),
            json!({"type":"system","subtype":"api_retry","attempt":1,"max_retries":10,"retry_delay_ms":500,"error_status":401,"error":"authentication_failed","uuid":SECOND}),
            json!({"type":"assistant","uuid":SECOND,"parent_tool_use_id":null}),
            json!({"type":"result","uuid":SECOND,"subtype":"success","is_error":false,"user_message_uuid":FIRST}),
        ]);
        assert!(facts.contains(&Observation::ModelAnswered), "{facts:?}");
        assert_eq!(failure(&facts), None);
    }
    // The synthetic message Claude Code ends a turn with when the API refused its sign-in.
    #[test]
    fn a_turn_ended_by_an_authentication_error_carries_it_and_proves_no_answer() {
        let mut state = state();
        let facts = turn_end(&mut state, vec![
            json!({"type":"assistant","uuid":REPLY,"parent_tool_use_id":null,"user_message_uuid":FIRST,"user_message_uuids":[FIRST],"error":"authentication_failed"}),
            json!({"type":"result","uuid":SECOND,"subtype":"success","is_error":true,"api_error_status":401,"terminal_reason":"api_error","user_message_uuid":FIRST}),
        ]);
        assert!(!facts.contains(&Observation::ModelAnswered), "{facts:?}");
        assert_eq!(failure(&facts), Some(ApiFailure { error: "authentication_failed".into(), status: Some(401), resets_at: None }));
    }
    #[test]
    fn a_forbidden_request_keeps_its_status_and_a_rejected_limit_its_reset() {
        let forbidden = turn_end(&mut state(), vec![
            json!({"type":"assistant","uuid":REPLY,"parent_tool_use_id":null,"user_message_uuid":FIRST,"user_message_uuids":[FIRST],"error":"authentication_failed"}),
            json!({"type":"result","uuid":SECOND,"is_error":true,"api_error_status":403,"user_message_uuid":FIRST}),
        ]);
        assert_eq!(failure(&forbidden).and_then(|failure| failure.status), Some(403));
        let limited = turn_end(&mut state(), vec![
            json!({"type":"assistant","uuid":REPLY,"parent_tool_use_id":null,"user_message_uuid":FIRST,"user_message_uuids":[FIRST],"error":"rate_limit"}),
            json!({"type":"rate_limit_event","uuid":SECOND,"rate_limit_info":{"status":"rejected","resetsAt":4_102_444_800_u64,"rateLimitType":"five_hour"}}),
            json!({"type":"result","uuid":SECOND,"is_error":true,"api_error_status":429,"terminal_reason":"blocking_limit","user_message_uuid":FIRST}),
        ]);
        assert_eq!(failure(&limited), Some(ApiFailure { error: "rate_limit".into(), status: Some(429), resets_at: Some(4_102_444_800) }));
    }
    // Claude Code 2.1.300 always reports SessionStart hooks, before init: a plugin with hooks,
    // the user's or Alondra's, must not stop the task.
    #[test]
    fn hooks_reported_before_init_are_evidence_without_facts() {
        let mut state = Checkpoint { format: CHECKPOINT_FORMAT, ..Checkpoint::default() };
        for subtype in ["hook_started", "hook_progress", "hook_response"] {
            let hook = event(json!({"type":"system","subtype":subtype,"hook_id":"h","hook_name":"SessionStart:startup","hook_event":"SessionStart","uuid":REPLY}));
            assert_eq!(fold(&hook, &session(), &mut state, input).unwrap(), vec![], "{subtype}");
        }
        let init = event(json!({"type":"system","subtype":"init","uuid":REPLY}));
        assert_eq!(fold(&init, &session(), &mut state, input).unwrap(), vec![Observation::Available]);
    }
    #[test]
    fn an_error_code_outside_claude_codes_grammar_is_refused() {
        let assistant = event(json!({"type":"assistant","uuid":REPLY,"parent_tool_use_id":null,"user_message_uuid":FIRST,"user_message_uuids":[FIRST],"error":"Not Logged In"}));
        assert!(matches!(fold(&assistant, &session(), &mut state(), input), Err(AdapterError::InvalidObservation)));
    }
}
#[cfg(test)]
mod diagnostic_tests {
    use super::*;
    use alondra_agent_protocol::{Execution, NativeCorrelation, SdkEventKind, TaskExecution};
    use alondra_foundation::{AgentId, DataEpoch, InputId, TaskId};
    use serde_json::json;
    const SESSION: &str = "30000000-0000-4000-8000-000000000001";
    const INPUT: &str = "30000000-0000-4000-8000-000000000002";
    fn event(kind: SdkEventKind, payload: Value) -> Event {
        Event {
            fence: Fence {
                profile_id: "fixture".into(),
                account_id: "fixture".into(),
                data_epoch: DataEpoch::from("epoch"),
                host_id: "host".into(),
            },
            execution: Execution::Task(TaskExecution {
                task_id: TaskId::from("task"),
                agent_id: AgentId::from("node"),
                conversation_id: "conversation".into(),
                process_instance_id: "process".into(),
                generation: 1,
            }),
            cursor: 1,
            observation: WireObservation::Sdk {
                kind,
                native: NativeCorrelation::default(),
                payload,
                failures: vec![],
            },
        }
    }
    fn session() -> SessionRecord {
        SessionRecord {
            session_id: MessageUuid::new(SESSION).unwrap(),
        }
    }
    fn state() -> Checkpoint {
        Checkpoint {
            format: CHECKPOINT_FORMAT,
            initialized: true,
            ..Checkpoint::default()
        }
    }
    fn assistant() -> Value {
        json!({"type":"assistant","session_id":SESSION,"uuid":"30000000-0000-4000-8000-000000000003","parent_tool_use_id":null,"user_message_uuid":INPUT,"user_message_uuids":[INPUT]})
    }
    fn input(_: &[MessageUuid]) -> Result<Vec<InputId>, AdapterError> {
        Ok(vec![InputId::new("initial").unwrap()])
    }
    #[test]
    fn real_host_stderr_diagnostic_needs_no_sdk_session_and_produces_no_facts() {
        let diagnostic = event(
            SdkEventKind::Diagnostic,
            json!({"source":"stderr","text":"Redacted fixture diagnostic","truncated_at_bytes":16384}),
        );
        assert!(
            fold(&diagnostic, &session(), &mut state(), |_| panic!(
                "diagnostic is not consumption"
            ))
            .unwrap()
            .is_empty()
        );
    }
    #[test]
    fn opaque_stderr_source_on_assistant_cannot_hide_consumption() {
        let plain = fold(
            &event(SdkEventKind::Message, assistant()),
            &session(),
            &mut state(),
            input,
        )
        .unwrap();
        assert!(matches!(
            plain.first(),
            Some(Observation::TurnStarted { .. })
        ));
        let mut payload = assistant();
        payload["source"] = json!("stderr");
        let with_source = fold(
            &event(SdkEventKind::Message, payload),
            &session(),
            &mut state(),
            input,
        )
        .unwrap();
        assert_eq!(
            with_source, plain,
            "opaque SDK source must not become host diagnostic authority"
        );
    }
    #[test]
    fn opaque_stderr_source_cannot_bypass_sdk_session_identity() {
        let mut payload = assistant();
        payload["session_id"] = json!("wrong-session");
        assert!(matches!(
            fold(
                &event(SdkEventKind::Message, payload.clone()),
                &session(),
                &mut state(),
                input
            ),
            Err(AdapterError::InvalidObservation)
        ));
        payload["source"] = json!("stderr");
        assert!(matches!(
            fold(
                &event(SdkEventKind::Message, payload),
                &session(),
                &mut state(),
                input
            ),
            Err(AdapterError::InvalidObservation)
        ));
    }

    #[test]
    fn only_the_exit_and_the_iterator_end_are_what_a_host_leaves_as_it_ends() {
        use alondra_agent_protocol::{FailureSurface, SdkFailure};
        let failure = |surface| SdkFailure { surface, code: "aborted".into(), details: json!({}) };
        let diagnostic = |failures| WireObservation::Sdk { kind: SdkEventKind::Diagnostic, native: NativeCorrelation::default(), payload: json!({}), failures };
        assert!(ended_with_its_host(&WireObservation::ProcessExited { exit_code: Some(0), signal: None }));
        assert!(ended_with_its_host(&diagnostic(vec![failure(FailureSurface::Control)])));
        assert!(ended_with_its_host(&diagnostic(vec![failure(FailureSurface::Iterator)])));
        assert!(!ended_with_its_host(&diagnostic(vec![failure(FailureSurface::Auth)])), "a sign-in failure is a fact, not the host's end");
        assert!(!ended_with_its_host(&diagnostic(vec![])));
        assert!(!ended_with_its_host(&event(SdkEventKind::Result, json!({"type": "result"})).observation));
    }
}
