//! Compares the frame the daemon sent with the one a transcript expected. Fields that change on
//! every run are not compared: the recording keeps its frames as they were and the comparison
//! decides what may differ. The ids the daemon assigns are recorded as they were too: the first
//! frame that carries one pairs it with the id of the run, and the frames the host answers with
//! carry the run's id wherever the recording had its own.

use serde_json::{Map, Value};
use std::collections::BTreeMap;

/// Fields of a command the daemon fills in on each run.
const RUN_FIELDS: &[&str] = &["command_id", "deadline_ms", "fence"];
/// Fields of an action that depend on the Mac or the client that ran it.
const ACTION_FIELDS: &[&str] = &["cwd", "client_version", "mcp_servers"];
/// Ids the daemon assigns: the first frame fixes which id of the run stands for the recorded one,
/// and every later frame has to keep that pairing.
const ASSIGNED_IDS: &[&str] = &["input_id", "conversation_id", "session_id", "request_id", "tool_use_id", "user_message_uuid"];

#[derive(Debug, Default)]
pub struct FrameComparator {
    learned: BTreeMap<String, String>,
}

/// The execution of a command names the conversation it belongs to; the rest is the run's.
fn without_run_fields(mut frame: Value) -> Value {
    if let Some(body) = frame.get_mut("body").and_then(Value::as_object_mut) {
        for key in RUN_FIELDS {
            body.remove(*key);
        }
        if let Some(execution) = body.remove("execution") {
            let conversation: Map<String, Value> = execution.get("conversation_id").map(|id| ("conversation_id".to_owned(), id.clone())).into_iter().collect();
            body.insert("execution".into(), Value::Object(conversation));
        }
        if let Some(action) = body.get_mut("action").and_then(Value::as_object_mut) {
            for key in ACTION_FIELDS {
                action.remove(*key);
            }
        }
    }
    frame
}

fn agree(expected: &Value, received: &Value, learned: &mut BTreeMap<String, String>, key: Option<&str>) -> bool {
    match (expected, received) {
        (Value::Object(expected), Value::Object(received)) => {
            expected.len() == received.len()
                && expected.iter().all(|(key, value)| received.get(key).is_some_and(|other| agree(value, other, learned, Some(key))))
        }
        (Value::Array(expected), Value::Array(received)) => {
            expected.len() == received.len() && expected.iter().zip(received).all(|(a, b)| agree(a, b, learned, key))
        }
        (Value::String(expected), Value::String(received)) if key.is_some_and(|key| ASSIGNED_IDS.contains(&key)) => {
            learned.entry(expected.clone()).or_insert_with(|| received.clone()) == received
        }
        _ => expected == received,
    }
}

impl FrameComparator {
    pub fn new() -> Self {
        Self::default()
    }

    /// Whether `received` is the frame `expected` stands for. A pairing of ids is kept only when
    /// the frames match.
    pub fn matches(&mut self, expected: &Value, received: &Value) -> bool {
        let mut learned = self.learned.clone();
        let same = agree(&without_run_fields(expected.clone()), &without_run_fields(received.clone()), &mut learned, None);
        if same {
            self.learned = learned;
        }
        same
    }

    /// The run's ids in place of the recorded ones a frame of the transcript carries.
    pub fn assign(&self, value: &mut Value) {
        match value {
            Value::String(text) => {
                if let Some(id) = self.learned.get(text.as_str()) {
                    *text = id.clone();
                }
            }
            Value::Object(map) => map.values_mut().for_each(|inner| self.assign(inner)),
            Value::Array(items) => items.iter_mut().for_each(|inner| self.assign(inner)),
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn offer(command: &str, input: &str, cwd: &str, prompt: &str) -> Value {
        json!({"body": {"type": "command", "command_id": command, "deadline_ms": 5, "execution": {"task_id": "t", "conversation_id": "conversation-1"}, "action": {"type": "offer", "input_id": input, "cwd": cwd, "client_version": "9.9", "mcp_servers": ["x"], "prompt": prompt}}})
    }

    #[test]
    fn what_changes_on_every_run_is_ignored() {
        let mut comparator = FrameComparator::new();
        assert!(comparator.matches(&offer("c1", "i1", "/a", "«user text 1»"), &offer("c2", "i2", "/b", "«user text 1»")));
    }

    #[test]
    fn a_different_prompt_is_a_mismatch() {
        assert!(!FrameComparator::new().matches(&offer("c1", "i1", "/a", "«user text 1»"), &offer("c1", "i1", "/a", "«user text 2»")));
    }

    #[test]
    fn an_assigned_id_keeps_the_pairing_the_first_frame_fixed() {
        let mut comparator = FrameComparator::new();
        assert!(comparator.matches(&offer("c", "i1", "/a", "p"), &offer("c", "run-1", "/a", "p")));
        assert!(comparator.matches(&offer("c", "i1", "/a", "p"), &offer("c", "run-1", "/a", "p")));
        assert!(!comparator.matches(&offer("c", "i1", "/a", "p"), &offer("c", "run-2", "/a", "p")));
    }

    #[test]
    fn a_frame_that_does_not_match_fixes_no_pairing() {
        let mut comparator = FrameComparator::new();
        assert!(!comparator.matches(&offer("c", "i1", "/a", "p"), &offer("c", "run-1", "/a", "other")));
        assert!(comparator.matches(&offer("c", "i1", "/a", "p"), &offer("c", "run-2", "/a", "p")));
    }

    #[test]
    fn the_ids_a_command_pairs_are_the_run_s_in_what_the_host_answers() {
        let mut comparator = FrameComparator::new();
        let recorded = |input: &str| json!({"body": {"type": "command", "execution": {"conversation_id": "00000000-0000-4000-8000-000000000001"}, "action": {"type": "offer", "input_id": input, "prompt": "p"}}});
        let sent = |input: &str| json!({"body": {"type": "command", "execution": {"task_id": "t", "conversation_id": "run-conversation"}, "action": {"type": "offer", "input_id": input, "prompt": "p"}}});
        assert!(comparator.matches(&recorded("00000000-0000-4000-8000-000000000101"), &sent("run-input-1")));
        assert!(comparator.matches(&recorded("00000000-0000-4000-8000-000000000102"), &sent("run-input-2")));
        let mut answer = json!({"params": {"inputId": "00000000-0000-4000-8000-000000000102", "items": ["00000000-0000-4000-8000-000000000101", "00000000-0000-4000-8000-000000000001", "kept"]}});
        comparator.assign(&mut answer);
        assert_eq!(answer, json!({"params": {"inputId": "run-input-2", "items": ["run-input-1", "run-conversation", "kept"]}}));
    }

    #[test]
    fn a_recording_that_does_not_name_the_conversation_or_the_input_is_a_mismatch() {
        let sent = offer("c", "run-1", "/a", "p");
        let mut without_execution = sent.clone();
        without_execution["body"].as_object_mut().unwrap().remove("execution");
        let mut without_input = sent.clone();
        without_input["body"]["action"].as_object_mut().unwrap().remove("input_id");
        assert!(!FrameComparator::new().matches(&without_execution, &sent));
        assert!(!FrameComparator::new().matches(&without_input, &sent));
    }
}
