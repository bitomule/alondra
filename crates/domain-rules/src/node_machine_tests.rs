//! A node runs through the Agent SDK until its session closes; no other phase exists. Every phase
//! an older release stored for a node of `claude --bg` reads as `Closed`.
use super::lifecycle_tests::{exec, must};
use super::*;
use std::sync::Arc;

fn paused_snapshot_state() -> DomainState {
    let mut state = super::lifecycle_tests::world();
    super::lifecycle_tests::sdk_turn(&mut state, "m-paused", "a");
    must(&mut state, Actor::System, Command::MoveNode { agent_id: "a".into(), event: NodeEvent::Paused {
        pause: Pause { reason: PauseReason::Memory, since: 1, cut: None, turn_since: None, cleanup: PauseCleanup::Done, released: false },
    }});
    assert!(state.node_pause(&"a".into()).is_some());
    state
}

#[test]
fn snapshot_pause_lookup_reads_only_the_requested_nodes_tasks() {
    for unrelated in [8, 128] {
        let mut state = paused_snapshot_state();
        let template = state.tasks[&TaskId::from("m-paused")].clone();
        for n in 0..unrelated {
            let id = TaskId::from(format!("unrelated-{n:03}"));
            state.tasks.insert(id.clone(), Task { id, agent_id: Some("b".into()), state: TaskState::Done, ..template.clone() });
        }
        let snapshot = ReadSnapshot::build(Arc::new(state));
        let before = measure::tasks_read();
        for _ in 0..32 { assert!(snapshot.node_pause(&"a".into()).is_some()); }
        let read = measure::tasks_read() - before;
        assert_eq!(read, 32, "32 checks of one task must not read {unrelated} unrelated tasks");
    }
}

#[test]
fn snapshot_pause_preserves_task_id_selection_and_archived_history() {
    let mut state = paused_snapshot_state();
    let template = state.tasks[&TaskId::from("m-paused")].clone();
    for (id, status, revision) in [("a-other", TaskState::Done, 900), ("z-other", TaskState::NotStarted, 1000)] {
        let id = TaskId::from(id);
        state.tasks.insert(id.clone(), Task { id, state: status, revision, ..template.clone() });
    }
    let assert_same = |state: &DomainState, expected: &str| {
        let snapshot = ReadSnapshot::build(Arc::new(state.clone()));
        assert_eq!(state.node_task(&"a".into()).map(|task| task.id.0.as_str()), Some(expected));
        assert_eq!(snapshot.index().node(&"a".into()).unwrap().all_tasks,
            ["a-other", "m-paused", "z-other"].map(TaskId::from));
        for agent in ["a", "b", "missing"] {
            assert_eq!(snapshot.node_pause(&agent.into()), state.node_pause(&agent.into()), "{agent}");
        }
        snapshot
    };
    let initial = assert_same(&state, "m-paused");
    assert!(initial.node_pause(&"a".into()).is_some());
    assert_eq!(initial.index().task_of_node(&"a".into()), Some(&TaskId::from("z-other")),
        "the screen's latest task is intentionally not the node machine's current task");
    state.tasks.get_mut(&"m-paused".into()).unwrap().state = TaskState::Closing;
    assert!(assert_same(&state, "z-other").node_pause(&"a".into()).is_none());
    state.tasks.get_mut(&"z-other".into()).unwrap().state = TaskState::Done;
    assert_same(&state, "m-paused");
    for task in state.tasks.values_mut() { task.archived = true; }
    assert!(assert_same(&state, "z-other").node_pause(&"a".into()).is_none());
    state.agents.get_mut(&"a".into()).unwrap().phase = NodePhase::Closed;
    assert!(assert_same(&state, "z-other").node_pause(&"a".into()).is_none());
}

#[test]
fn snapshot_pause_keeps_an_old_reader_coherent_while_a_resume_is_published() {
    let mut state = paused_snapshot_state();
    let old = Arc::new(ReadSnapshot::build(Arc::new(state.clone())));
    let published = std::sync::RwLock::new(old.clone());
    let ready = std::sync::Barrier::new(2);
    std::thread::scope(|scope| {
        let (resume, resumed) = std::sync::mpsc::channel();
        let (published, ready) = (&published, &ready);
        scope.spawn(move || {
            let retained = published.read().unwrap().clone();
            ready.wait();
            resumed.recv().unwrap();
            assert!(retained.node_pause(&"a".into()).is_some());
            assert!(retained.index().describes(retained.state()));
            let latest = published.read().unwrap().clone();
            assert!(latest.node_pause(&"a".into()).is_none());
            assert!(latest.index().describes(latest.state()));
            assert!(latest.state().revision > retained.state().revision);
        });
        ready.wait();
        must(&mut state, Actor::System, Command::MoveNode { agent_id: "a".into(), event: NodeEvent::Resumed });
        *published.write().unwrap() = Arc::new(ReadSnapshot::build(Arc::new(state.clone())));
        resume.send(()).unwrap();
    });
    assert!(old.node_pause(&"a".into()).is_some());
}

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
