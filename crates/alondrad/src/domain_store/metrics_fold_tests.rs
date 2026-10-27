use super::*;
use crate::domain_store::model_metrics::{Decision as Chosen, DecisionKind as ChosenKind};
use crate::domain_store::tests::{Fixture, seeded};
use alondra_core::domain::*;
use alondra_core::options::Approach;

fn envelope(store: &mut DomainStore, id: &str, command: Command) -> CommandEnvelope {
    let state = store.load().unwrap();
    CommandEnvelope { protocol_version: PROTOCOL_VERSION, data_epoch: state.data_epoch.clone(), command_id: id.into(), expected_revision: state.revision, command }
}
fn run(store: &mut DomainStore, actor: &Actor, id: &str, command: Command) {
    let e = envelope(store, id, command);
    store.execute_authenticated(actor, &e, None).unwrap();
}
fn user() -> Actor {
    Actor::User { user_id: "user-1".into() }
}
fn agent() -> Actor {
    Actor::Agent { agent_id: "agent-1".into() }
}
pub(in crate::domain_store) fn usage(model: &str, cost: Option<i64>, tokens: i64) -> Usage {
    Usage { model: model.into(), cost_micros: cost, input: tokens, output: tokens / 10, cache_read: tokens * 10, cache_write: tokens }
}
pub(in crate::domain_store) fn spend(store: &mut DomainStore, session: &str, task: Option<&str>, role: Role, usage: &[Usage], steps: StepCounts, at: Millis) {
    store.record_spend(&Spent { session_id: session, task_id: task, role, provider: "claude", usage, steps, at }).unwrap();
}
pub(in crate::domain_store) fn steps(steps: i64, looks: i64) -> StepCounts {
    StepCounts { steps, subagent_steps: 1, looks, polls: 1, tool_calls: steps }
}
fn present(id: &str) -> Command {
    let targets = vec![DeliveryTarget::Text { text: "The answer".into() }];
    Command::PresentResult {
        checklist: vec![], task_id: "task-1".into(), interaction_id: id.into(), body: "Finished".into(), targets: targets.clone(),
        is_final: true, nothing_to_review: false, attachments: vec![], card: CardSpec::delivery("Review the result", &targets, true),
    }
}
fn text() -> Vec<DeliveryTarget> {
    vec![DeliveryTarget::Text { text: "The answer".into() }]
}

/// task-1, launched on sonnet at medium effort, asks one question, gets changes asked on its first
/// delivery and is accepted on its second, its session having spent on two models.
pub(in crate::domain_store) fn done_task(fixture: &Fixture) -> DomainStore {
    let mut store = DomainStore::open(&fixture.path()).unwrap();
    seeded(&mut store);
    store.record_model_decision(&Chosen {
        task_id: "task-1".into(), request_id: "request-1".into(), agent_id: Some("agent-1".into()), at: 0, kind: ChosenKind::Launch,
        model: Some("sonnet".into()), effort: Some("medium".into()), approach: Some(Approach::Solo), chosen_by: ModelChooser::Orchestrator,
        reason: None, title: "Goal".into(), provider: None, tags: None,
    }).unwrap();
    run(&mut store, &agent(), "start", Command::StartTask { task_id: "task-1".into() });
    run(&mut store, &agent(), "ask", Command::AskQuestion {
        attachments: vec![], interaction_id: "ask-1".into(), scope: Scope::Task { task_id: "task-1".into() }, recipient: Recipient::User,
        body: "Which table goes first?".into(), blocking: true,
        options: vec![QuestionOption { id: "cards".into(), label: "Cards".into(), description: None }, QuestionOption { id: "inputs".into(), label: "Inputs".into(), description: None }],
        card: CardSpec::new(CardKind::Decide, "Choose the table to compact first").why("The cards weigh most"),
    });
    run(&mut store, &user(), "answer", Command::AnswerQuestion { attachments: vec![], interaction_id: "ask-1".into(), option_id: "cards".into(), inputs: Default::default() });
    run(&mut store, &agent(), "present-1", present("delivery-1"));
    run(&mut store, &user(), "changes", Command::SubmitFeedback {
        attachments: vec![], presentation_id: "delivery-1".into(), targets: text(), body: "Shorter, please".into(), instruction: Instruction::Changes,
    });
    let spent = [usage("claude-sonnet-5-5", Some(2_000_000), 1_000), usage("claude-haiku-4-5", Some(100_000), 200)];
    spend(&mut store, "agent-1", Some("task-1"), Role::Node, &spent, steps(40, 10), 1);
    run(&mut store, &agent(), "present-2", present("delivery-2"));
    run(&mut store, &user(), "accept", Command::AcceptDelivery { presentation_id: "delivery-2".into(), targets: text() });
    run(&mut store, &Actor::System, "end", Command::RecordNodeEnd { task_id: "task-1".into(), end: NodeEnd::Closed, agent_id: None });
    assert_eq!(store.load().unwrap().tasks[&TaskId::from("task-1")].state, TaskState::Done);
    store.conn.execute("UPDATE command_audit SET at=revision*1000", []).unwrap();
    store
}

#[derive(Debug, PartialEq)]
struct Row {
    outcome: String,
    model: Option<String>,
    effort: Option<String>,
    approach: Option<String>,
    clean: bool,
    change_rounds: i64,
    interventions: i64,
    measured: bool,
    sessions: i64,
    steps: [i64; 4],
    times: [i64; 4],
    finished_at: i64,
}

fn row(store: &DomainStore) -> Row {
    store.conn.query_row(
        "SELECT outcome,model,effort,approach,clean,change_rounds,interventions,measured,sessions,steps,subagent_steps,looks,polls,
          work_ms,waiting_user_ms,queued_ms,blocked_ms,finished_at FROM task_metrics WHERE task_id='task-1'",
        [],
        |r| Ok(Row {
            outcome: r.get(0)?, model: r.get(1)?, effort: r.get(2)?, approach: r.get(3)?, clean: r.get(4)?, change_rounds: r.get(5)?,
            interventions: r.get(6)?, measured: r.get(7)?, sessions: r.get(8)?, steps: [r.get(9)?, r.get(10)?, r.get(11)?, r.get(12)?],
            times: [r.get(13)?, r.get(14)?, r.get(15)?, r.get(16)?], finished_at: r.get(17)?,
        }),
    ).unwrap()
}

fn spent_by_model(store: &DomainStore) -> Vec<(String, Option<i64>, i64)> {
    let mut statement = store.conn.prepare("SELECT model,cost_micros,input FROM task_model_usage WHERE task_id='task-1' ORDER BY model").unwrap();
    statement.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?))).unwrap().collect::<rusqlite::Result<_>>().unwrap()
}

fn pass(store: &mut DomainStore, policy: fn(&Transaction<'_>, i64, i64) -> rusqlite::Result<usize>, now: i64, limit: i64) -> usize {
    let tx = store.conn.transaction().unwrap();
    let changed = policy(&tx, now, limit).unwrap();
    tx.commit().unwrap();
    changed
}

#[test]
fn a_session_keeps_its_highest_totals_ignores_an_empty_report_and_a_drop_marks_it_partial() {
    let fixture = Fixture::new();
    let mut store = DomainStore::open(&fixture.path()).unwrap();
    seeded(&mut store);
    let model = "claude-opus-5-5";
    spend(&mut store, "s", Some("task-1"), Role::Node, &[usage(model, Some(1_000_000), 100)], steps(3, 1), 1);
    spend(&mut store, "s", Some("task-1"), Role::Node, &[usage(model, Some(0), 0)], StepCounts::default(), 2);
    assert_eq!(store.usage_row("s", model), Some((Some("task-1".into()), Some(1_000_000), [100, 10, 1_000, 100], true)), "a crashed run's empty result changes nothing");
    spend(&mut store, "s", None, Role::Node, &[usage(model, Some(3_000_000), 300)], steps(2, 2), 3);
    spend(&mut store, "s", Some("task-1"), Role::Node, &[usage(model, Some(500_000), 50)], StepCounts::default(), 4);
    assert_eq!(store.usage_row("s", model), Some((Some("task-1".into()), Some(3_000_000), [300, 30, 3_000, 300], false)),
        "the highest totals stay and the drop says the measure is partial");
    assert_eq!(store.steps_row("s"), Some((Some("task-1".into()), StepCounts { steps: 5, subagent_steps: 2, looks: 3, polls: 2, tool_calls: 5 })), "steps add up turn after turn");
}

#[test]
fn a_session_without_a_task_adds_what_it_grew_by_to_the_day_it_spent_it() {
    let fixture = Fixture::new();
    let mut store = DomainStore::open(&fixture.path()).unwrap();
    let day = |n: i64| n * DAY_MS + 1;
    spend(&mut store, "o", None, Role::Orchestrator, &[usage("claude-opus-5-5", Some(1_000_000), 100)], StepCounts::default(), day(1));
    spend(&mut store, "o", None, Role::Orchestrator, &[usage("claude-opus-5-5", Some(3_000_000), 300)], StepCounts::default(), day(2));
    spend(&mut store, "o", None, Role::Orchestrator, &[usage("claude-opus-5-5", Some(2_000_000), 250)], StepCounts::default(), day(2));
    spend(&mut store, "lost", None, Role::Node, &[usage("codex", None, 40)], StepCounts::default(), day(2));
    let mut statement = store.conn.prepare("SELECT day,model,role,cost_micros,costed,input,sessions FROM usage_days ORDER BY day,role").unwrap();
    let days: Vec<(String, String, String, i64, i64, i64, i64)> = statement.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?, r.get(6)?)))
        .unwrap().collect::<rusqlite::Result<_>>().unwrap();
    assert_eq!(days, [
        ("1970-01-02".into(), "claude-opus-5-5".into(), "orchestrator".into(), 1_000_000, 1, 100, 1),
        ("1970-01-03".into(), "claude-opus-5-5".into(), "orchestrator".into(), 2_000_000, 0, 200, 0),
        ("1970-01-03".into(), "codex".into(), "unattributed".into(), 0, 0, 40, 1),
    ], "a drop adds nothing, and a provider without prices adds tokens without a cost");
}

#[test]
fn an_ended_task_folds_its_history_feedback_launch_and_spend_once_until_it_spends_again() {
    let fixture = Fixture::new();
    let mut store = done_task(&fixture);
    assert_eq!(store.fold_ended_tasks(64).unwrap(), 1);
    let questions: i64 = store.conn.query_row("SELECT count(*) FROM task_history WHERE task_id='task-1' AND state='needs_you'", [], |r| r.get(0)).unwrap();
    assert!(questions >= 1);
    let folded = row(&store);
    assert_eq!((folded.outcome.as_str(), folded.model.as_deref(), folded.effort.as_deref(), folded.approach.as_deref()), ("done", Some("sonnet"), Some("medium"), Some("solo")));
    assert_eq!((folded.clean, folded.change_rounds, folded.interventions), (false, 1, questions + 1), "the round of changes is rework, the answer and the changes are interventions");
    assert_eq!((folded.measured, folded.sessions, folded.steps), (true, 1, [40, 1, 10, 1]));
    assert!(folded.times[0] > 0 && folded.times[1] > 0, "{:?}", folded.times);
    assert_eq!(spent_by_model(&store), [("claude-haiku-4-5".into(), Some(100_000), 200), ("claude-sonnet-5-5".into(), Some(2_000_000), 1_000)]);

    assert_eq!(store.fold_ended_tasks(64).unwrap(), 0, "nothing changed since");
    spend(&mut store, "agent-1", Some("task-1"), Role::Node, &[usage("claude-sonnet-5-5", Some(2_500_000), 1_100)], steps(2, 0), 2);
    assert_eq!(store.fold_ended_tasks(64).unwrap(), 1, "a last result after the task ended is folded in");
    assert_eq!(row(&store).steps, [42, 2, 10, 2]);
    assert_eq!(spent_by_model(&store)[1], ("claude-sonnet-5-5".into(), Some(2_500_000), 1_100));
}

#[test]
fn a_task_folds_the_same_numbers_after_its_cards_are_compacted() {
    let fixture = Fixture::new();
    let mut store = done_task(&fixture);
    store.fold_ended_tasks(64).unwrap();
    let before = row(&store);
    run(&mut store, &Actor::System, "compact", Command::CompactEndedTasks { task_ids: vec!["task-1".into()] });
    store.conn.execute_batch("DELETE FROM task_metrics; UPDATE usage_sessions SET folded=0;").unwrap();
    assert_eq!(store.fold_ended_tasks(64).unwrap(), 1);
    assert_eq!(row(&store), before);
}

#[test]
fn a_task_folded_again_after_its_samples_went_keeps_what_it_spent() {
    let fixture = Fixture::new();
    let mut store = done_task(&fixture);
    store.fold_ended_tasks(64).unwrap();
    let finished = row(&store).finished_at;
    assert_eq!(pass(&mut store, samples_of_folded_tasks, finished + SAMPLES_MS, 256), 0, "a week has not gone by");
    assert_eq!(pass(&mut store, samples_of_folded_tasks, finished + SAMPLES_MS + 1, 256), 3, "the session's spend on two models and its steps");
    assert_eq!(store.usage_row("agent-1", "claude-sonnet-5-5"), None);

    store.conn.execute("UPDATE task_metrics SET folded_revision=0", []).unwrap();
    assert_eq!(store.fold_ended_tasks(64).unwrap(), 1, "as when its history moved on");
    let refolded = row(&store);
    assert_eq!((refolded.measured, refolded.sessions, refolded.steps), (true, 1, [40, 1, 10, 1]), "the spend folded before stays");
    assert_eq!(spent_by_model(&store).len(), 2);
}

#[test]
fn samples_of_an_open_task_or_spent_after_the_fold_stay() {
    let fixture = Fixture::new();
    let mut store = done_task(&fixture);
    store.fold_ended_tasks(64).unwrap();
    let finished = row(&store).finished_at;
    spend(&mut store, "agent-1", Some("task-1"), Role::Node, &[usage("claude-sonnet-5-5", Some(2_500_000), 1_100)], StepCounts::default(), 3);
    assert_eq!(pass(&mut store, samples_of_folded_tasks, finished + 30 * DAY_MS, 256), 0, "a task keeps all its samples until all are folded");
    assert!(store.usage_row("agent-1", "claude-haiku-4-5").is_some() && store.steps_row("agent-1").is_some());
    spend(&mut store, "other", Some("task-open"), Role::Node, &[usage("claude-opus-5-5", Some(1), 1)], StepCounts::default(), 0);
    store.fold_ended_tasks(64).unwrap();
    assert_eq!(pass(&mut store, samples_of_folded_tasks, finished + 30 * DAY_MS, 256), 3);
    assert!(store.usage_row("other", "claude-opus-5-5").is_some(), "an open task's samples stay");
}

#[test]
fn sessions_without_a_task_go_after_a_week_without_spending() {
    let fixture = Fixture::new();
    let mut store = DomainStore::open(&fixture.path()).unwrap();
    spend(&mut store, "o", None, Role::Orchestrator, &[usage("claude-opus-5-5", Some(1), 1)], steps(1, 0), 10);
    assert_eq!(pass(&mut store, idle_sessions_without_task, 10 + SAMPLES_MS, 256), 0);
    assert_eq!(pass(&mut store, idle_sessions_without_task, 11 + SAMPLES_MS, 256), 2);
    let days: i64 = store.conn.query_row("SELECT count(*) FROM usage_days", [], |r| r.get(0)).unwrap();
    assert_eq!(days, 1, "its day keeps what it spent");
}

pub(in crate::domain_store) fn insert_task(store: &DomainStore, task: &str, finished_at: Millis, model: &str, merged: bool, lines: Option<i64>, cost: Option<i64>) {
    store.conn.execute(
        "INSERT INTO task_metrics(task_id,project,model,effort,approach,role,outcome,started_at,finished_at,clean,change_rounds,reviewer_rounds,
          interventions,reopened,relaunched,work_ms,waiting_user_ms,queued_ms,blocked_ms,measured,sessions,steps,subagent_steps,looks,polls,
          pr_merged,pr_lines,folded_revision)
         VALUES(?1,'alondra',?2,'high','solo','implementation','done',?3,?3,1,1,0,2,0,0,3600000,600000,60000,0,1,2,180,40,70,6,?4,?5,12345)",
        params![task, model, finished_at, merged, lines],
    ).unwrap();
    for (name, share) in [("claude-opus-5-5", 9), ("claude-haiku-4-5-20251001", 1)] {
        store.conn.execute(
            "INSERT INTO task_model_usage(task_id,model,provider,cost_micros,input,output,cache_read,cache_write) VALUES(?1,?2,'claude',?3,?4,?5,?6,?7)",
            params![task, name, cost.map(|cost| cost * share / 10), 12_000 * share, 90_000 * share, 30_000_000 * share, 400_000 * share],
        ).unwrap();
    }
}

#[test]
fn tasks_older_than_90_days_add_up_exactly_into_their_week_and_their_models_day() {
    let fixture = Fixture::new();
    let mut store = DomainStore::open(&fixture.path()).unwrap();
    let monday = 20_017 * DAY_MS;
    insert_task(&store, "t-a", monday + 1, "opus", true, Some(120), Some(4_000_000));
    insert_task(&store, "t-b", monday + 3 * DAY_MS, "opus", true, None, Some(6_000_000));
    insert_task(&store, "t-c", monday + 7 * DAY_MS, "opus", false, None, None);
    let now = monday + 8 * DAY_MS + TASKS_MS;
    for expected in [1, 1, 1, 0] {
        assert_eq!(pass(&mut store, tasks_into_weeks, now, 1), expected);
    }
    let mut statement = store.conn.prepare("SELECT week,tasks,clean,merged_prs,pr_lines,lined_prs,cost_micros,costed,merged_cost_micros,lined_cost_micros,input,steps FROM metric_weeks ORDER BY week").unwrap();
    let weeks: Vec<(String, [i64; 11])> = statement.query_map([], |r| Ok((r.get(0)?, [r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?, r.get(6)?, r.get(7)?, r.get(8)?, r.get(9)?, r.get(10)?, r.get(11)?])))
        .unwrap().collect::<rusqlite::Result<_>>().unwrap();
    assert_eq!(weeks, [
        ("2024-10-21".into(), [2, 2, 2, 120, 1, 10_000_000, 2, 10_000_000, 4_000_000, 240_000, 360]),
        ("2024-10-28".into(), [1, 1, 0, 0, 0, 0, 0, 0, 0, 120_000, 180]),
    ], "tasks of the same week folded in separate passes add up");
    let days: i64 = store.conn.query_row("SELECT count(*) FROM usage_days WHERE role='node'", [], |r| r.get(0)).unwrap();
    let opus: i64 = store.conn.query_row("SELECT sum(cost_micros) FROM usage_days WHERE model='claude-opus-5-5'", [], |r| r.get(0)).unwrap();
    assert_eq!((days, opus), (6, 9_000_000), "each model on the day each task ended");
    let left: i64 = store.conn.query_row("SELECT (SELECT count(*) FROM task_metrics)+(SELECT count(*) FROM task_model_usage)", [], |r| r.get(0)).unwrap();
    assert_eq!(left, 0);
}

#[test]
fn a_folded_task_keeps_the_provider_of_its_last_launch_and_the_kind_it_was_tagged_with() {
    let fixture = Fixture::new();
    let mut store = done_task(&fixture);
    let tags = serde_json::from_value(serde_json::json!({"kind": "fix", "areas": ["daemon"]})).unwrap();
    let decision = |kind, provider, tags| Chosen {
        task_id: "task-1".into(), request_id: "request-1".into(), agent_id: Some("agent-2".into()), at: 0, kind,
        model: Some("codex/gpt-6.1-sol".into()), effort: None, approach: None, chosen_by: ModelChooser::Orchestrator,
        reason: None, title: "Goal".into(), provider, tags,
    };
    store.record_model_decision(&decision(ChosenKind::Relaunch, Some(alondra_core::accounts::Provider::Codex), Some(tags))).unwrap();
    store.record_model_decision(&decision(ChosenKind::Send, Some(alondra_core::accounts::Provider::Codex), None)).unwrap();
    assert_eq!(store.fold_ended_tasks(64).unwrap(), 1);
    let folded: (Option<String>, Option<String>) = store.conn.query_row("SELECT provider,kind FROM task_metrics WHERE task_id='task-1'", [],
        |r| Ok((r.get(0)?, r.get(1)?))).unwrap();
    assert_eq!(folded, (Some("codex".into()), Some("fix".into())), "a message without tags keeps the kind the launch gave");
    let read = store.metrics_inputs(1).unwrap();
    assert_eq!((read.tasks[0].provider.as_deref(), read.tasks[0].kind.as_deref()), (Some("codex"), Some("fix")));

    let untagged = Fixture::new();
    let mut store = done_task(&untagged);
    store.fold_ended_tasks(64).unwrap();
    let read = store.metrics_inputs(1).unwrap();
    assert_eq!((read.tasks[0].provider.as_deref(), read.tasks[0].kind.as_deref()), (None, None), "older decisions leave both unknown");
}

#[test]
fn a_folded_task_takes_at_most_400_bytes() {
    let fixture = Fixture::new();
    let store = DomainStore::open(&fixture.path()).unwrap();
    let size = |store: &DomainStore| store.conn.query_row("SELECT page_count*page_size FROM pragma_page_count(),pragma_page_size()", [], |r| r.get::<_, i64>(0)).unwrap();
    let before = size(&store);
    for n in 0..1_000 {
        insert_task(&store, &format!("t-metrics-section-plan-{n:05}"), 1_792_800_000_000 + n * 600_000, "opus", n % 3 == 0, Some(400 + n), Some(3_000_000 + n));
    }
    let per_task = (size(&store) - before) / 1_000;
    assert!(per_task <= 400, "{per_task} bytes per task");
}

#[test]
fn a_release_keeps_the_time_it_first_started() {
    let fixture = Fixture::new();
    let mut store = DomainStore::open(&fixture.path()).unwrap();
    store.record_release("0.3.7", 10).unwrap();
    store.record_release("0.3.7", 20).unwrap();
    let first: i64 = store.conn.query_row("SELECT first_seen_at FROM daemon_releases WHERE version='0.3.7'", [], |r| r.get(0)).unwrap();
    assert_eq!(first, 10);
}

/// task-1 folded with a merged PR on GitHub, whose size is not known yet.
fn merged_pr_store(fixture: &Fixture) -> DomainStore {
    let mut store = DomainStore::open(&fixture.path()).unwrap();
    seeded(&mut store);
    run(&mut store, &agent(), "start", Command::StartTask { task_id: "task-1".into() });
    run(&mut store, &agent(), "pr", Command::RegisterPr {
        task_id: "task-1".into(), pr_id: "pr-1".into(), host: "github.com".into(), repository: "owner/repository".into(),
        number: 7, head: "aaa".into(), base: "main".into(), allowed_methods: vec![MergeMethod::Squash],
    });
    run(&mut store, &user(), "cancel", Command::CancelTask { task_id: "task-1".into() });
    run(&mut store, &Actor::System, "end", Command::RecordNodeEnd { task_id: "task-1".into(), end: NodeEnd::Closed, agent_id: Some("agent-1".into()) });
    assert_eq!(store.load().unwrap().tasks[&TaskId::from("task-1")].state, TaskState::Cancelled);
    store.conn.execute("UPDATE pull_requests SET state='merged',merged_head=head", []).unwrap();
    store.fold_ended_tasks(64).unwrap();
    let project: Option<String> = store.conn.query_row("SELECT project FROM task_metrics WHERE task_id='task-1'", [], |r| r.get(0)).unwrap();
    assert_eq!(project.as_deref(), Some("repository"), "without a project chosen, the PR's repository names it");
    store
}

/// Runs `test` on a daemon whose store holds task-1 folded with a merged PR of unknown size.
pub(crate) fn with_a_merged_pr(test: impl FnOnce(&crate::domain_runtime::DomainRuntime)) {
    let fixture = Fixture::new();
    drop(merged_pr_store(&fixture));
    test(&crate::domain_runtime::DomainRuntime::open_at(&fixture.0, &fixture.path()).unwrap());
}

/// Runs `test` on a daemon whose store holds task-1 running on agent-1.
pub(crate) fn with_a_running_task(test: impl FnOnce(&crate::domain_runtime::DomainRuntime)) {
    let fixture = Fixture::new();
    let mut store = DomainStore::open(&fixture.path()).unwrap();
    seeded(&mut store);
    run(&mut store, &agent(), "start", Command::StartTask { task_id: "task-1".into() });
    drop(store);
    test(&crate::domain_runtime::DomainRuntime::open_at(&fixture.0, &fixture.path()).unwrap());
}

#[test]
fn codex_native_orchestrator_samples_keep_partial_coverage_and_unknown_cost_after_updates() {
    let fixture = Fixture::new(); let mut store = DomainStore::open(&fixture.path()).unwrap();
    for tokens in [100, 200] {
        let observed = [usage("native-model", None, tokens)];
        store.record_spend(&Spent { session_id: "codex-batch", task_id: None, role: Role::Orchestrator, provider: "codex", usage: &observed, steps: StepCounts::default(), at: tokens }).unwrap();
        assert_eq!(store.usage_row("codex-batch", "native-model"), Some((None, None, [tokens, tokens / 10, tokens * 10, tokens], false)));
    }
}
