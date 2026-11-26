use super::*;
use crate::lifecycle_tests::{must, sdk_turn, world};
use std::sync::atomic::Ordering;

fn changed(snapshot: &ReadSnapshot, cursor: &NodeChangeCursor, expected: &[&str]) {
    assert_eq!(snapshot.node_changes_since(cursor), NodeChanges::Changed {
        cursor: snapshot.node_cursor(), nodes: expected.iter().map(|id| AgentId::from(*id)).collect(),
    });
}

// The reference follows the existing live row's reads, independently of the index's grouping.
fn assert_reference(snapshot: &ReadSnapshot) {
    let state = snapshot.state();
    for agent in state.agents.values() {
        let mine: Vec<_> = state.tasks.values().filter(|task| task.agent_id.as_ref() == Some(&agent.id)).collect();
        let row = snapshot.index().live_node(&agent.id).unwrap();
        assert_eq!(row.name, agent.name);
        assert_eq!(row.phase, agent.phase);
        assert_eq!(row.tasks, mine.iter().map(|task| task.id.clone()).collect::<Vec<_>>());
        assert_eq!(row.launch_requests, mine.iter().filter(|task| !task.archived).map(|task| task.request_id.clone()).collect::<Vec<_>>());
        assert_eq!(row.open_task, mine.iter().any(|task| !task.archived && !matches!(task.state, TaskState::Done | TaskState::Closing | TaskState::Cancelled)));
        assert_eq!(row.current_task, state.node_task(&agent.id).map(|task| task.id.clone()));
        let sdk = if agent.phase == NodePhase::ThroughSdk { state.sdk_live_of(&agent.id, state.node_task(&agent.id)) } else { None };
        assert_eq!(row.sdk, sdk);
        assert_eq!(row.waiting_on_pause, matches!(&sdk, Some(alondra_domain::LiveState::WaitingUser(cards)) if cards.iter().all(|card| state.node_pause_card(card))));
        let wait = mine.first().and_then(|task| {
            let lifecycle = state.lifecycle_of(&task.id).ok()?;
            Some((task.account_key().provider, lifecycle.execution()?.instance().as_str().to_owned()))
        });
        assert_eq!(row.reported_wait, wait);
    }
}

fn next(previous: &ReadSnapshot, change: impl FnOnce(&mut DomainState)) -> ReadSnapshot {
    let mut state = (**previous.state()).clone();
    change(&mut state);
    state.revision += 1;
    let snapshot = ReadSnapshot::build_after(Arc::new(state), previous);
    assert_reference(&snapshot);
    snapshot
}

#[test]
fn live_node_projection_preserves_selection_history_and_wait_inputs() {
    let mut state = world();
    sdk_turn(&mut state, "m-current", "a");
    let template = state.tasks[&TaskId::from("m-current")].clone();
    for (id, status, revision) in [("a-history", TaskState::Done, 900), ("z-latest", TaskState::NotStarted, 1000)] {
        let id = TaskId::from(id);
        state.tasks.insert(id.clone(), Task { id, state: status, revision, ..template.clone() });
    }
    let snapshot = ReadSnapshot::build(Arc::new(state));
    assert_reference(&snapshot);
    let row = snapshot.index().live_node(&"a".into()).unwrap();
    assert_eq!(row.current_task, Some("m-current".into()));
    assert_eq!(snapshot.index().task_of_node(&"a".into()), Some(&TaskId::from("z-latest")));
    assert!(row.reported_wait.is_none(), "the first historical task has no execution journal");
    let without_first = next(&snapshot, |state| { state.tasks.remove(&"a-history".into()); });
    assert_eq!(without_first.index().live_node(&"a".into()).unwrap().reported_wait.as_ref().unwrap().1, "instance-m-current");
    let closing = next(&without_first, |state| { state.tasks.get_mut(&"m-current".into()).unwrap().state = TaskState::Closing; });
    assert_eq!(closing.index().live_node(&"a".into()).unwrap().current_task, Some("z-latest".into()));
    let archived = next(&closing, |state| { for task in state.tasks.values_mut() { task.archived = true; } });
    let row = archived.index().live_node(&"a".into()).unwrap();
    assert_eq!(row.tasks, ["m-current", "z-latest"].map(TaskId::from));
    assert_eq!(row.current_task, Some("z-latest".into()));
    assert!(row.launch_requests.is_empty());
    assert!(!row.open_task);
    changed(&archived, &snapshot.node_cursor(), &["a"]);
}

#[test]
fn node_changes_accumulate_for_independent_readers_and_include_removed_and_reassigned_nodes() {
    let mut state = world();
    sdk_turn(&mut state, "task", "a");
    let initial = ReadSnapshot::build(Arc::new(state));
    let renamed = next(&initial, |state| { state.agents.get_mut(&"a".into()).unwrap().name = "Project/Node".into(); });
    changed(&renamed, &initial.node_cursor(), &["a"]);
    let retitled = next(&renamed, |state| { state.tasks.get_mut(&"task".into()).unwrap().title = "A new title".into(); });
    changed(&retitled, &renamed.node_cursor(), &[]);
    let reassigned = next(&retitled, |state| { state.tasks.get_mut(&"task".into()).unwrap().agent_id = Some("b".into()); });
    changed(&reassigned, &retitled.node_cursor(), &["a", "b"]);
    let removed = next(&reassigned, |state| { state.agents.remove(&"a".into()); });
    changed(&removed, &reassigned.node_cursor(), &["a"]);
    changed(&removed, &initial.node_cursor(), &["a", "b"]);
    changed(&removed, &initial.node_cursor(), &["a", "b"]);
    changed(&removed, &removed.node_cursor(), &[]);
    assert_eq!(initial.index().live_node(&"a".into()).unwrap().name, "a");
    assert!(removed.index().live_node(&"a".into()).is_none());
    assert_eq!(initial.index().live_node(&"a".into()).unwrap().tasks, [TaskId::from("task")]);
    assert_eq!(removed.index().live_node(&"b".into()).unwrap().tasks, [TaskId::from("task")]);
}

#[test]
fn pause_card_kind_launch_request_and_cancellation_are_live_node_dependencies() {
    let mut state = world();
    sdk_turn(&mut state, "task", "a");
    must(&mut state, Actor::System, Command::MoveNode { agent_id: "a".into(), event: NodeEvent::Paused {
        pause: Pause { reason: PauseReason::ByUser, since: 1, cut: None, turn_since: None, cleanup: PauseCleanup::Done, released: false },
    }});
    let paused = ReadSnapshot::build(Arc::new(state));
    assert!(paused.index().live_node(&"a".into()).unwrap().waiting_on_pause);
    let ordinary_card = next(&paused, |state| {
        let Some(alondra_domain::LiveState::WaitingUser(cards)) = &paused.index().live_node(&"a".into()).unwrap().sdk else { panic!("paused on a user card") };
        state.interactions.get_mut(cards.iter().next().unwrap()).unwrap().system_card = None;
    });
    assert_eq!(ordinary_card.index().live_node(&"a".into()).unwrap().sdk, paused.index().live_node(&"a".into()).unwrap().sdk);
    assert!(!ordinary_card.index().live_node(&"a".into()).unwrap().waiting_on_pause);
    changed(&ordinary_card, &paused.node_cursor(), &["a"]);
    let request = next(&ordinary_card, |state| { state.tasks.get_mut(&"task".into()).unwrap().request_id = "another-request".into(); });
    changed(&request, &ordinary_card.node_cursor(), &["a"]);
    assert_eq!(request.index().live_node(&"a".into()).unwrap().launch_requests, [RequestId::from("another-request")]);

    let cancelling = ReadSnapshot::build(paused.state().clone());
    let mut state = (**cancelling.state()).clone();
    must(&mut state, crate::lifecycle_tests::user(), Command::CancelTask { task_id: "task".into(), reason: None });
    let cancelled = ReadSnapshot::build_after(Arc::new(state), &cancelling);
    assert_reference(&cancelled);
    assert!(!cancelled.index().live_node(&"a".into()).unwrap().open_task);
    changed(&cancelled, &cancelling.node_cursor(), &["a"]);
    let closed = next(&cancelled, |state| { state.agents.get_mut(&"a".into()).unwrap().phase = NodePhase::Closed; });
    assert!(closed.index().live_node(&"a".into()).unwrap().sdk.is_none());
    changed(&closed, &cancelled.node_cursor(), &["a"]);
}

#[test]
fn a_reload_epoch_or_unrelated_snapshot_resets_a_cursor_even_at_the_same_revision() {
    let initial = ReadSnapshot::build(Arc::new(world()));
    let cursor = initial.node_cursor();
    let reused = ReadSnapshot::build_after(initial.state().clone(), &initial);
    assert_eq!(reused.node_cursor(), cursor);
    for changed_epoch in [false, true] {
        let mut state = (**initial.state()).clone();
        state.agents.get_mut(&"a".into()).unwrap().name = "Reloaded".into();
        if changed_epoch { state.data_epoch = "another-epoch".into(); state.revision += 1; }
        let reloaded = ReadSnapshot::build_after(Arc::new(state), &initial);
        assert!(matches!(reloaded.node_changes_since(&cursor), NodeChanges::Reset { .. }));
        assert_eq!(reloaded.index().live_node(&"a".into()).unwrap().name, "Reloaded");
        changed(&reloaded, &reloaded.node_cursor(), &[]);
    }
    let unrelated = ReadSnapshot::build(initial.state().clone());
    assert!(matches!(unrelated.node_changes_since(&cursor), NodeChanges::Reset { .. }));
    let future = next(&initial, |_| {});
    assert!(matches!(initial.node_changes_since(&future.node_cursor()), NodeChanges::Reset { .. }));
}

#[test]
fn two_successors_of_one_snapshot_cannot_accept_each_others_cursor() {
    let initial = ReadSnapshot::build(Arc::new(world()));
    let first = next(&initial, |state| { state.agents.get_mut(&"a".into()).unwrap().name = "First branch".into(); });
    let second = next(&initial, |state| { state.agents.get_mut(&"b".into()).unwrap().name = "Second branch".into(); });
    changed(&first, &initial.node_cursor(), &["a"]);
    assert!(matches!(second.node_changes_since(&initial.node_cursor()), NodeChanges::Reset { .. }));
    assert!(matches!(second.node_changes_since(&first.node_cursor()), NodeChanges::Reset { .. }));
    assert!(matches!(first.node_changes_since(&second.node_cursor()), NodeChanges::Reset { .. }));
    assert_ne!(first.node_cursor(), second.node_cursor());
    let continued = next(&second, |state| { state.agents.get_mut(&"b".into()).unwrap().name = "Continued branch".into(); });
    changed(&continued, &second.node_cursor(), &["b"]);
    changed(&first, &initial.node_cursor(), &["a"]);
    assert_eq!(initial.index().live_node(&"a".into()).unwrap().name, "a");
}

#[test]
fn a_history_gap_discards_whole_publications_and_never_returns_partial_node_sets() {
    let mut initial = ReadSnapshot::build(Arc::new(world()));
    // Seed the retention boundary without thousands of unrelated domain rebuilds.
    initial.node_history.generation = 1;
    initial.node_history.lineage.store(1, Ordering::Relaxed);
    for n in 0..RETAINED_NODE_IDS {
        initial.node_history.entries.push_back((1, AgentId::from(format!("historical-{n}"))));
    }
    let expired = NodeChangeCursor { lineage: initial.node_history.lineage.clone(), generation: 0 };
    let retained = initial.node_cursor();
    let after = next(&initial, |state| { state.agents.get_mut(&"a".into()).unwrap().name = "Changed".into(); });
    assert_eq!(after.node_history.entries.len(), 1);
    assert!(matches!(after.node_changes_since(&expired), NodeChanges::Reset { .. }));
    changed(&after, &retained, &["a"]);

    let oversized = next(&after, |state| {
        for n in 0..=RETAINED_NODE_IDS {
            let id = AgentId::from(format!("new-{n}"));
            state.agents.insert(id.clone(), Agent { id, name: "Node".into(), phase: NodePhase::Closed, since: 0 });
        }
    });
    assert!(oversized.node_history.entries.is_empty());
    assert!(matches!(oversized.node_changes_since(&after.node_cursor()), NodeChanges::Reset { .. }));
    changed(&oversized, &oversized.node_cursor(), &[]);
}

#[test]
fn a_current_cursor_reads_nothing_and_a_recent_cursor_reads_only_the_changed_suffix() {
    let mut initial = ReadSnapshot::build(Arc::new(world()));
    initial.node_history.generation = 1;
    initial.node_history.lineage.store(1, Ordering::Relaxed);
    for n in 0..RETAINED_NODE_IDS - 1 {
        initial.node_history.entries.push_back((1, AgentId::from(format!("historical-{n}"))));
    }
    let after = next(&initial, |state| { state.agents.get_mut(&"a".into()).unwrap().name = "Changed".into(); });
    assert_eq!(after.node_history.entries.len(), RETAINED_NODE_IDS);
    let before = crate::measure::node_change_entries_read();
    for _ in 0..32 { changed(&after, &after.node_cursor(), &[]); }
    assert_eq!(crate::measure::node_change_entries_read() - before, 0);
    changed(&after, &initial.node_cursor(), &["a"]);
    assert_eq!(crate::measure::node_change_entries_read() - before, 2, "one changed entry and the boundary");
}

#[test]
fn concurrent_readers_keep_the_projection_and_delta_of_their_own_snapshot() {
    let mut state = world();
    sdk_turn(&mut state, "task", "a");
    let old = Arc::new(ReadSnapshot::build(Arc::new(state.clone())));
    let published = std::sync::RwLock::new(old.clone());
    let ready = std::sync::Barrier::new(2);
    std::thread::scope(|scope| {
        let (send, receive) = std::sync::mpsc::channel();
        let (published, ready) = (&published, &ready);
        scope.spawn(move || {
            let retained = published.read().unwrap().clone();
            ready.wait();
            receive.recv().unwrap();
            assert!(matches!(retained.index().live_node(&"a".into()).unwrap().sdk, Some(alondra_domain::LiveState::Working(_))));
            changed(&retained, &retained.node_cursor(), &[]);
            let latest = published.read().unwrap().clone();
            assert!(latest.index().live_node(&"a".into()).unwrap().waiting_on_pause);
            changed(&latest, &retained.node_cursor(), &["a"]);
            assert_reference(&retained);
            assert_reference(&latest);
        });
        ready.wait();
        must(&mut state, Actor::System, Command::MoveNode { agent_id: "a".into(), event: NodeEvent::Paused {
            pause: Pause { reason: PauseReason::ByUser, since: 1, cut: None, turn_since: None, cleanup: PauseCleanup::Done, released: false },
        }});
        *published.write().unwrap() = Arc::new(ReadSnapshot::build_after(Arc::new(state), &old));
        send.send(()).unwrap();
    });
}

#[test]
fn delta_and_projection_reads_do_not_revisit_the_domains_tasks_or_cards() {
    for unrelated in [8, 128] {
        let mut state = world();
        sdk_turn(&mut state, "task", "a");
        let template = state.tasks[&TaskId::from("task")].clone();
        for n in 0..unrelated {
            let id = TaskId::from(format!("history-{n}"));
            state.tasks.insert(id.clone(), Task { id, agent_id: Some("b".into()), state: TaskState::Done, ..template.clone() });
        }
        let initial = ReadSnapshot::build(Arc::new(state));
        let after = next(&initial, |state| { state.agents.get_mut(&"a".into()).unwrap().name = "Renamed".into(); });
        let before = (crate::measure::tasks_read(), crate::measure::cards_read(), crate::measure::node_change_entries_read());
        for _ in 0..32 {
            changed(&after, &initial.node_cursor(), &["a"]);
            assert_eq!(after.index().live_node(&"a".into()).unwrap().name, "Renamed");
        }
        assert_eq!((crate::measure::tasks_read() - before.0, crate::measure::cards_read() - before.1, crate::measure::node_change_entries_read() - before.2),
            (0, 0, 32), "the retained changes, not the {unrelated} unrelated tasks, bound the work");
    }
}
