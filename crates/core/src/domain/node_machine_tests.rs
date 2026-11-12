//! A node runs through the Agent SDK until its session closes; no other phase exists. Every phase
//! an older release stored for a node of `claude --bg` reads as `Closed`.
use super::lifecycle_tests::{exec, must};
use super::*;

/// The phases a release with the `claude --bg` node machine wrote to `agents.phase`, one per key,
/// with the fields it stored beside the key.
const OLD_PHASES: &[&str] = &[
    r#"{"kind":"starting","reviving":false}"#,
    r#"{"kind":"starting","reviving":true,"idle_since":1700000000000}"#,
    r#"{"kind":"lost"}"#,
    r#"{"kind":"working"}"#,
    r#"{"kind":"waiting_background","work":"shell","label":"bazel test //...","since":1700000000000,"others":2,"gone":1700000060000}"#,
    r#"{"kind":"waiting_user","in_turn":true}"#,
    r#"{"kind":"paused","reason":{"kind":"by_user"},"since":1700000000000,"cleanup":"done","released":false}"#,
    r#"{"kind":"paused","reason":{"kind":"declared","reason":"The build queue is full"},"since":1700000000000,"cut":"make test"}"#,
    r#"{"kind":"delivered","in_turn":false}"#,
    r#"{"kind":"resuming","idle_since":1700000000000,"after_wait":true}"#,
    r#"{"kind":"waiting_dependency"}"#,
    r#"{"kind":"no_task"}"#,
    r#"{"kind":"closing"}"#,
];

#[test]
fn an_old_node_phase_decodes_as_closed() {
    for stored in OLD_PHASES {
        let phase: NodePhase = serde_json::from_str(stored).unwrap_or_else(|error| panic!("{stored} did not decode: {error}"));
        assert_eq!(phase, NodePhase::Closed, "{stored}");
        let agent: Agent = serde_json::from_value(serde_json::json!({
            "id": "a", "name": "a", "phase": serde_json::from_str::<serde_json::Value>(stored).unwrap(), "since": 0,
        })).unwrap_or_else(|error| panic!("an agent stored with {stored} did not decode: {error}"));
        assert_eq!(agent.phase, NodePhase::Closed, "{stored}");
    }
    for (stored, phase) in [(r#"{"kind":"through_sdk"}"#, NodePhase::ThroughSdk), (r#"{"kind":"closed"}"#, NodePhase::Closed)] {
        assert_eq!(serde_json::from_str::<NodePhase>(stored).unwrap(), phase);
        assert_eq!(serde_json::to_string(&phase).unwrap(), stored);
    }
}

#[test]
fn a_node_runs_through_the_sdk_from_its_registration_until_its_session_closes() {
    let mut s = DomainState::new("epoch");
    must(&mut s, Actor::System, Command::RegisterAgent { agent_id: "a".into(), name: "a".into(), at: 0 });
    assert_eq!(s.node_phase(&"a".into()), Some(&NodePhase::ThroughSdk));
    must(&mut s, Actor::System, Command::MoveNode { agent_id: "a".into(), event: NodeEvent::SessionClosed });
    assert_eq!(s.node_phase(&"a".into()), Some(&NodePhase::Closed));
    let pause = Pause { reason: PauseReason::Memory, since: 1, cut: None, turn_since: None, cleanup: PauseCleanup::Pending, released: false };
    let refused = exec(&mut s, Actor::System, Command::MoveNode { agent_id: "a".into(), event: NodeEvent::Paused { pause } });
    assert_eq!(refused.map_err(|error| error.code), Err("node_closed".into()), "nothing moves a closed node");
    assert_eq!(s.node_phase(&"a".into()), Some(&NodePhase::Closed));
}
