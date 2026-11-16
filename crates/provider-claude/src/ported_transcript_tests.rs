//! A turn a real Claude Code session ended with a 401, ported as an anonymized transcript and
//! replayed through the normalizer the daemon runs: the frames of the host in their order, with the
//! ids the transcript recorded, end the turn as an authentication failure with its status.

use super::*;
use alondra_agent_protocol::{Body, Frame};
use alondra_foundation::InputId;
use alondra_scenarios_transcript::Transcript;
use serde_json::json;

const PORTED: &str = include_str!("../../../fixtures/transcripts/claude-401-turn/transcript.ndjson");

/// What a host stamps on every event: the transcript does not record it.
fn stamped(frame: &Value, cursor: u64) -> Frame {
    let mut frame = frame.clone();
    let body = frame["body"].as_object_mut().expect("a frame has a body");
    body.insert("fence".into(), json!({"profile_id": "fixture", "account_id": "fixture", "data_epoch": "epoch", "host_id": "host"}));
    body.insert(
        "execution".into(),
        json!({"task_id": "task", "agent_id": "node", "conversation_id": "conversation", "process_instance_id": "process", "generation": 1}),
    );
    body.insert("cursor".into(), cursor.into());
    serde_json::from_value(frame).expect("the transcript holds frames of the protocol")
}

fn replay(transcript: &Transcript) -> Vec<Observation> {
    let mut state = Checkpoint { format: CHECKPOINT_FORMAT, initialized: true, ..Checkpoint::default() };
    let mut facts = Vec::new();
    for (index, frame) in transcript.inbound().enumerate() {
        let Body::Event(event) = stamped(frame, index as u64 + 1).body else { continue };
        let WireObservation::Sdk { payload, .. } = &event.observation else { continue };
        let session = SessionRecord { session_id: MessageUuid::new(payload["session_id"].as_str().expect("a session id")).unwrap(), continued: false };
        let input = |ids: &[MessageUuid]| Ok(ids.iter().map(|id| InputId::new(id.as_str()).unwrap()).collect());
        facts.extend(fold(&event, &session, &mut state, input).expect("the normalizer reads the frame"));
    }
    facts
}

#[test]
fn the_ported_turn_ends_as_an_authentication_failure_with_its_status() {
    let transcript = Transcript::parse(PORTED).unwrap();
    assert_eq!(transcript.header.origin, alondra_scenarios_transcript::Origin::PortedIncident);
    let facts = replay(&transcript);
    assert!(!facts.contains(&Observation::ModelAnswered), "{facts:?}");
    match facts.last() {
        Some(Observation::TurnEnded { outcome: TurnOutcome::ApiFailed(failure), .. }) => {
            assert_eq!(failure, &ApiFailure { error: "authentication_failed".into(), status: Some(401), resets_at: None });
        }
        other => panic!("the turn did not end as a failure of the API: {other:?}"),
    }
}
