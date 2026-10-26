use super::*;
use crate::domain_store::metrics_fold::tests::{done_task, insert_task, spend, steps, usage};
use crate::domain_store::tests::Fixture;
use alondra_core::metrics::Role;
use alondra_core::metrics::report::{Metric, Query, report};

const DAY_MS: Millis = 24 * 60 * 60 * 1_000;

#[test]
fn reading_the_metrics_folds_the_tasks_that_ended_since_the_last_fold() {
    let fixture = Fixture::new();
    drop(done_task(&fixture));
    let runtime = crate::domain_runtime::DomainRuntime::open_at(&fixture.0, &fixture.path()).unwrap();
    let inputs = runtime.metrics_inputs(10 * DAY_MS).unwrap();
    assert_eq!(inputs.tasks.len(), 1, "task-1 ended and was never folded");
    let task = &inputs.tasks[0];
    assert_eq!((task.model.as_deref(), task.effort.as_deref(), task.change_rounds, task.clean), (Some("sonnet"), Some("medium"), 1, false));
    let spend = task.spend.unwrap();
    assert_eq!(spend.cost_micros, Some(2_100_000), "both models its session spent on");
    assert_eq!(spend.tokens.input, 1_200);
}

#[test]
fn the_inputs_hold_tasks_weeks_spend_without_a_task_and_releases() {
    let fixture = Fixture::new();
    let mut store = DomainStore::open(&fixture.path()).unwrap();
    let now = 20_752 * DAY_MS;
    insert_task(&store, "t-recent", now - DAY_MS, "opus", true, Some(300), Some(5_000_000));
    insert_task(&store, "t-unpriced", now - 2 * DAY_MS, "codex", false, None, None);
    insert_task(&store, "t-old", now - 100 * DAY_MS, "opus", true, Some(100), Some(1_000_000));
    let tx = store.conn.transaction().unwrap();
    crate::domain_store::metrics_fold::tasks_into_weeks(&tx, now, 10).unwrap();
    tx.commit().unwrap();
    spend(&mut store, "orchestrator-1", None, Role::Orchestrator, &[usage("claude-opus-5-5", Some(700_000), 1_000)], steps(3, 1), now - DAY_MS);
    store.record_release("0.3.5", now - 3 * DAY_MS).unwrap();
    let inputs = store.metrics_inputs(now).unwrap();
    assert_eq!(inputs.tasks.iter().map(|t| (t.model.as_deref(), t.spend.and_then(|s| s.cost_micros))).collect::<Vec<_>>(),
        [(Some("codex"), None), (Some("opus"), Some(5_000_000))], "an unpriced task's cost stays unknown");
    assert_eq!(inputs.weeks.len(), 1);
    assert_eq!((inputs.weeks[0].model.as_str(), inputs.weeks[0].sums.tasks, inputs.weeks[0].sums.cost_micros), ("opus", 1, 1_000_000));
    assert_eq!(inputs.days.iter().map(|d| (d.model.as_str(), d.cost_micros, d.sessions)).collect::<Vec<_>>(),
        [("claude-opus-5-5", 700_000, 1)], "the old task's spend by day is already in its week");
    assert_eq!(inputs.releases.iter().map(|r| r.version.as_str()).collect::<Vec<_>>(), ["0.3.5"]);
    let all = report(&inputs, &Query::parse("period=all").unwrap());
    assert_eq!(all.value(Metric::CostTotal).unwrap().value, Some(6_700_000.0));
    assert_eq!(all.value(Metric::Tasks).unwrap().value, Some(3.0));
}
