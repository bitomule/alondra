use crate::records::{InputRecord, SessionRecord};
use alondra_agent_application::{Normalization, ObservationMetadata, ObservationNormalizer};
use alondra_agent_protocol::{
    Body, Event, Fence, FramedReader, NativeCorrelation, Observation as WireObservation,
    SdkEventKind,
};
use alondra_foundation::{ContinuationId, MessageUuid, TurnId, WorkId};
use alondra_provider_contract::{AdapterError, Observation, OrderedObservation};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
use std::io::Cursor;

const CHECKPOINT_FORMAT: u16 = 1;

pub(super) struct Normalizer {
    pub session: Option<SessionRecord>,
    pub fence: Option<Fence>,
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
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct LogicalTurn {
    // Alondra logical identity assigned at an observed SDK root-message boundary.
    id: String,
    consumed: BTreeSet<MessageUuid>,
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
        if event.cursor != batch.cursor
            || self.fence.as_ref() != Some(&event.fence)
            || event.execution.task_id != *batch.execution.task()
            || event.execution.agent_id != *batch.execution.node()
            || event.execution.conversation_id != batch.execution.conversation().as_str()
            || event.execution.process_instance_id != batch.execution.instance().as_str()
            || event.execution.generation != batch.execution.generation()
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
        let session = self
            .session
            .as_ref()
            .ok_or(AdapterError::InvalidObservation)?;
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
fn fold(
    event: &Event,
    session: &SessionRecord,
    state: &mut Checkpoint,
    lookup: impl FnOnce(&[MessageUuid]) -> Result<Vec<alondra_foundation::InputId>, AdapterError>,
) -> Result<Vec<Observation>, AdapterError> {
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
    if payload.get("session_id").and_then(Value::as_str) != Some(session.session_id.as_str()) {
        return Err(AdapterError::InvalidObservation);
    }
    let kind = string(payload, "type")?;
    let mut facts = Vec::new();
    if kind == "system" && payload.get("subtype").and_then(Value::as_str) == Some("init") {
        if state.initialized {
            return Err(AdapterError::InvalidObservation);
        }
        state.initialized = true;
        facts.push(Observation::Available);
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
                facts.push(Observation::WorkEnded {
                    work: WorkId::new(work.id).map_err(|_| AdapterError::InvalidObservation)?,
                    continuation: ContinuationId::new(format!(
                        "claude-notification:{}",
                        basis.as_str()
                    ))
                    .map_err(|_| AdapterError::InvalidObservation)?,
                });
            }
            Some("conversation_reset") => return Err(AdapterError::Unsupported),
            _ => {}
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
    if kind == "result" {
        // A singular failed-delivery echo alone cannot prove input consumption or a turn.
        let turn = state.turn.as_ref().ok_or(AdapterError::Unsupported)?;
        if consumed.is_empty() {
            let singular = native_uuid(payload, "user_message_uuid")?;
            if !turn.consumed.contains(&singular) {
                return Err(AdapterError::Unsupported);
            }
        }
        facts.push(Observation::TurnEnded {
            turn: TurnId::new(turn.id.clone()).map_err(|_| AdapterError::InvalidObservation)?,
        });
        state.turn = None;
    }
    Ok(facts)
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
    use alondra_agent_protocol::{Execution, NativeCorrelation, SdkEventKind};
    use alondra_foundation::{AgentId, DataEpoch, InputId, TaskId};
    use serde_json::json;

    const SESSION: &str = "10000000-0000-4000-8000-000000000001";
    const FIRST: &str = "10000000-0000-4000-8000-000000000002";
    const SECOND: &str = "10000000-0000-4000-8000-000000000003";
    const REPLY: &str = "10000000-0000-4000-8000-000000000004";
    fn event(mut payload: Value) -> Event {
        payload["session_id"] = json!(SESSION);
        Event {
            fence: Fence {
                profile_id: "fixture".into(),
                account_id: "fixture".into(),
                data_epoch: DataEpoch::from("epoch"),
                host_id: "host".into(),
            },
            execution: Execution {
                task_id: TaskId::from("task"),
                agent_id: AgentId::from("node"),
                conversation_id: "conversation".into(),
                process_instance_id: "process".into(),
                generation: 1,
            },
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
                    turn: TurnId::new(format!("claude-message:{REPLY}")).unwrap()
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
            1
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
                turn: TurnId::new(format!("claude-message:{REPLY}")).unwrap()
            }]
        );
    }
    #[test]
    fn restarted_or_wrong_run_notification_cannot_end_work() {
        let mut state = state();
        state.turn = Some(LogicalTurn {
            id: "turn".into(),
            consumed: BTreeSet::new(),
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
        assert_eq!(
            fold(&ended, &session(), &mut state, input).unwrap(),
            vec![Observation::WorkEnded {
                work: WorkId::new(format!("claude-task:{REPLY}")).unwrap(),
                continuation: ContinuationId::new(format!("claude-notification:{SECOND}")).unwrap()
            }]
        );
        assert!(state.work.is_empty());
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
}
#[cfg(test)]
mod diagnostic_tests {
    use super::*;
    use alondra_agent_protocol::{Execution, NativeCorrelation, SdkEventKind};
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
            execution: Execution {
                task_id: TaskId::from("task"),
                agent_id: AgentId::from("node"),
                conversation_id: "conversation".into(),
                process_instance_id: "process".into(),
                generation: 1,
            },
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
}
