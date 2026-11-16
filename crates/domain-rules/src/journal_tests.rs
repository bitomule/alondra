//! A command copies no task's journal: it adds its new records to the journals it decides on,
//! the tasks it touched are found by their journals' length, and the store's writes for those
//! records are replayed from the lifecycle the state before it kept, never from the whole journal.
use super::lifecycle_cache_tests::{three_tasks, run, step};
use super::lifecycle_tests::{must, task};
use super::*;
use proptest::prelude::*;
use std::collections::BTreeSet;

/// The tasks whose journal differs, record by record, between `before` and `after`.
fn changed(before: &DomainState, after: &DomainState) -> BTreeSet<TaskId> {
    after.lifecycle_journal.iter().filter(|(task, journal)| before.lifecycle_journal.get(task) != Some(*journal)).map(|(task, _)| task.clone()).collect()
}

fn grown(before: &DomainState, after: &DomainState) -> BTreeSet<TaskId> {
    before.lifecycle_journal.grown(&after.lifecycle_journal).map(|(task, _)| task.clone()).collect()
}

/// The store's writes for what `task`'s journal added since `before`, from the lifecycle
/// `before` kept, and the events computing them applied.
fn new_writes(before: &DomainState, after: &DomainState, task: &TaskId) -> (Vec<JournalWrite>, u64) {
    let known = before.lifecycle_journal.records(task);
    let lifecycle = (known > 0).then(|| before.lifecycle_of(task).unwrap());
    let applied = alondra_domain::measure::applied();
    let writes = journal_writes_after(task, lifecycle.as_deref(), after.lifecycle_journal[task].since(known)).unwrap();
    (writes, alondra_domain::measure::applied() - applied)
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(256))]

    #[test]
    fn a_command_adds_only_its_new_records_and_the_store_replays_only_those(steps in prop::collection::vec(step(), 1..40)) {
        let mut s = three_tasks();
        for (n, step) in steps.iter().enumerate() {
            let before = s.clone();
            let _ = run(&mut s, step, n);
            let changed = changed(&before, &s);
            prop_assert_eq!(&grown(&before, &s), &changed, "after {:?} in {:?}", step, steps);
            prop_assert!(changed.is_subset(&before.touched_tasks(&s)), "{:?} touched every task whose journal it moved", step);
            for (task, journal) in &before.lifecycle_journal {
                let after = &s.lifecycle_journal[task];
                prop_assert!(after.starts_with(journal), "{} only grew", task);
                prop_assert_eq!(after.shares_records_with(journal), !changed.contains(task), "{} was copied only if {:?} moved it", task, step);
            }
            for task in &changed {
                let (writes, applied) = new_writes(&before, &s, task);
                let known = before.lifecycle_journal.records(task);
                prop_assert_eq!(&writes, &journal_writes(task, &s.lifecycle_journal[task], known).unwrap());
                let events = s.lifecycle_journal[task].len() - known.max(1);
                prop_assert_eq!(applied, events as u64, "{} applied only its {} new records", task, events);
            }
        }
    }
}

#[test]
fn among_many_tasks_a_command_touches_and_copies_only_the_ones_it_moves() {
    let mut s = three_tasks();
    let many: Vec<String> = (0..50).map(|n| format!("many-{n:02}")).collect();
    for id in &many {
        task(&mut s, id, None);
    }
    let asked = TaskId::from("s-task");
    let before = s.clone();
    must(&mut s, Actor::System, Command::TellNode { task_id: asked.clone(), text: "Notice".into(), from_user: false, answers: None });
    assert_eq!(grown(&before, &s), BTreeSet::from([asked.clone()]));
    assert_eq!(before.touched_tasks(&s), BTreeSet::from([asked.clone()]));
    for (task, journal) in &before.lifecycle_journal {
        assert_eq!(s.lifecycle_journal[task].shares_records_with(journal), *task != asked, "{task}");
    }
    let registered = s.clone();
    must(&mut s, Actor::System, Command::RegisterAgent { agent_id: "z".into(), name: "z".into(), at: 0 });
    assert_eq!(registered.touched_tasks(&s), BTreeSet::new(), "registering a node moves no task");
    assert!(s.lifecycle_journal.iter().all(|(task, journal)| journal.shares_records_with(&registered.lifecycle_journal[task])));
}

#[test]
fn a_journal_serializes_as_the_list_of_its_records() {
    let mut s = three_tasks();
    must(&mut s, Actor::System, Command::TellNode { task_id: "s-task".into(), text: "Notice".into(), from_user: false, answers: None });
    let journal = &s.lifecycle_journal[&TaskId::from("s-task")];
    let records: Vec<LifecycleRecord> = journal.iter().cloned().collect();
    assert_eq!(serde_json::to_value(journal).unwrap(), serde_json::to_value(&records).unwrap());
    let read: Journal = serde_json::from_value(serde_json::to_value(&records).unwrap()).unwrap();
    assert_eq!(&read, journal);
    assert!(!read.shares_records_with(journal));
}
