use super::*;

const NOW: Millis = 1_000 * DAY_MS;

fn task(days_ago: i64, model: &str, cost: Option<i64>) -> TaskRow {
    let finished_at = NOW - days_ago * DAY_MS;
    TaskRow {
        project: Some("alondra".into()),
        model: Some(model.into()),
        effort: Some("medium".into()),
        approach: Some("solo".into()),
        provider: None,
        kind: None,
        outcome: Outcome::Done,
        started_at: Some(finished_at - 3_600_000),
        finished_at,
        clean: true,
        change_rounds: 0,
        reviewer_rounds: 0,
        interventions: 1,
        reopened: false,
        relaunched: false,
        work_ms: 1_800_000,
        waiting_user_ms: 600_000,
        queued_ms: 60_000,
        blocked_ms: 0,
        sessions: 2,
        steps: 100,
        subagent_steps: 20,
        looks: 40,
        polls: 5,
        pr_merged: true,
        pr_lines: Some(200),
        spend: Some(Spend { cost_micros: cost, tokens: Tokens { input: 10, output: 1_000, cache_read: 1_000_000, cache_write: 50_000 } }),
    }
}

fn week(days_ago: i64, tasks: i64, cost_micros: i64) -> WeekRow {
    WeekRow {
        week: NOW - days_ago * DAY_MS,
        project: "alondra".into(),
        model: "opus".into(),
        effort: String::new(),
        approach: String::new(),
        sums: Sums { tasks, done: tasks, clean: tasks / 2, measured: tasks, costed: tasks, cost_micros, sessions: tasks, ..Sums::default() },
    }
}

fn inputs(tasks: Vec<TaskRow>, weeks: Vec<WeekRow>) -> MetricsInputs {
    MetricsInputs { now: NOW, tasks, weeks, days: vec![], releases: vec![] }
}

fn query(text: &str) -> Query {
    Query::parse(text).expect("a valid query")
}

#[test]
fn an_empty_period_counts_no_task_and_knows_no_cost() {
    let report = report(&inputs(vec![task(20, "sonnet", Some(1))], vec![]), &query("period=7d"));
    assert_eq!(report.value(Metric::Tasks).unwrap().value, Some(0.0));
    for metric in [Metric::Clean, Metric::CostTotal, Metric::CostPerTask, Metric::CostPerPr, Metric::InputTokens, Metric::TaskTime, Metric::StepsPerTask] {
        assert_eq!(report.value(metric).unwrap().value, None, "{metric:?}");
    }
    assert!(report.by_model.is_empty() && report.by_project.is_empty());
}

#[test]
fn a_period_of_only_old_weeks_sums_them_without_medians() {
    let report = report(&inputs(vec![], vec![week(120, 10, 5_000_000), week(200, 4, 3_000_000)]), &query("period=all"));
    assert_eq!(report.figures.sums.tasks, 14);
    assert_eq!(report.value(Metric::Clean).unwrap().value, Some(50.0));
    assert_eq!(report.value(Metric::CostPerTask).unwrap().value, Some(8_000_000.0 / 14.0));
    assert_eq!(report.figures.medians, Medians::default(), "weeks keep no medians");
    assert_eq!(report.by_model.iter().map(|g| (g.name.as_str(), g.effort.as_deref(), g.figures.sums.tasks)).collect::<Vec<_>>(), [("opus", None, 14)]);
    assert_eq!(super::report(&inputs(vec![], vec![week(120, 10, 1)]), &query("period=90d")).figures.sums.tasks, 0);
}

#[test]
fn tasks_and_weeks_add_up_and_split_by_model_approach_and_project() {
    let mut other = task(3, "opus", Some(4_000_000));
    other.project = Some("quill".into());
    other.approach = None;
    let rows = vec![task(1, "sonnet", Some(1_000_000)), task(2, "sonnet", Some(3_000_000)), other];
    let inputs = inputs(rows, vec![week(150, 6, 6_000_000)]);
    let report = report(&inputs, &query("period=all&compare=none"));
    assert_eq!((report.figures.sums.tasks, report.figures.sums.cost_micros), (9, 14_000_000));
    assert_eq!(report.previous, None);
    let names = |groups: &[Group]| groups.iter().map(|g| (g.name.clone(), g.figures.sums.tasks)).collect::<Vec<_>>();
    assert_eq!(names(&report.by_model), [("opus".to_owned(), 6), ("sonnet".to_owned(), 2), ("opus".to_owned(), 1)]);
    assert_eq!(report.by_model[1].effort.as_deref(), Some("medium"));
    assert_eq!(names(&report.by_project), [("alondra".to_owned(), 8), ("quill".to_owned(), 1)]);
    assert_eq!(names(&report.by_approach), [(String::new(), 7), ("solo".to_owned(), 2)]);
    assert_eq!(report.figures.medians.cost_micros, Some(3_000_000), "medians come from the tasks' own rows");
    let quill = super::report(&inputs, &query("period=all&project=quill"));
    assert_eq!(quill.figures.sums.tasks, 1);
    let sonnet = super::report(&inputs, &query("period=all&model=sonnet&effort=medium"));
    assert_eq!(sonnet.figures.sums.tasks, 2);
}

#[test]
fn tasks_split_by_provider_and_by_kind_and_older_rows_still_count() {
    let mut docs = task(1, "codex/gpt-6.1-sol", Some(1_000_000));
    docs.kind = Some("docs".into());
    let mut fix = task(2, "gpt-6.1-sol", Some(2_000_000));
    fix.provider = Some("codex".into());
    fix.kind = Some("fix".into());
    let mut claude_docs = task(3, "sonnet", Some(3_000_000));
    claude_docs.provider = Some("claude".into());
    claude_docs.kind = Some("docs".into());
    let mut unknown = task(4, "opus", None);
    unknown.model = None;
    let rows = vec![docs, fix, claude_docs, task(5, "opus", Some(4_000_000)), unknown];
    let report = report(&inputs(rows, vec![week(150, 6, 6_000_000)]), &query("period=all&compare=none"));
    let names = |groups: &[Group]| groups.iter().map(|g| (g.name.clone(), g.figures.sums.tasks)).collect::<Vec<_>>();
    assert_eq!(names(&report.by_provider), [("claude".to_owned(), 8), ("codex".to_owned(), 2), (String::new(), 1)],
        "a bare model of an older row was Claude's; a row without a model is unknown");
    assert_eq!(names(&report.by_kind), [(String::new(), 8), ("docs".to_owned(), 2), ("fix".to_owned(), 1)]);
    let docs = report.by_kind.iter().find(|g| g.name == "docs").unwrap();
    assert_eq!(docs.figures.sums.cost_micros, 4_000_000);
    assert!(report.by_kind.iter().all(|g| g.effort.is_none()));
    let mut older = serde_json::to_value(&report).unwrap();
    older.as_object_mut().unwrap().remove("by_provider");
    older.as_object_mut().unwrap().remove("by_kind");
    let back: MetricsReport = serde_json::from_value(older).unwrap();
    assert!(back.by_provider.is_empty() && back.by_kind.is_empty(), "a report from an older daemon still reads");
}

#[test]
fn a_task_no_provider_priced_stays_out_of_the_cost_averages() {
    let report = report(&inputs(vec![task(1, "sonnet", Some(2_000_000)), task(2, "codex", None)], vec![]), &query("period=7d"));
    let sums = report.figures.sums;
    assert_eq!((sums.tasks, sums.measured, sums.costed), (2, 2, 1));
    assert_eq!(report.value(Metric::CostPerTask).unwrap().value, Some(2_000_000.0));
    assert_eq!(report.value(Metric::MedianCostPerTask).unwrap().value, Some(2_000_000.0));
    assert_eq!(report.value(Metric::CostTotal).unwrap().value, Some(2_000_000.0));
    let unpriced = super::report(&inputs(vec![task(2, "codex", None)], vec![]), &query("period=7d"));
    assert_eq!(unpriced.value(Metric::CostTotal).unwrap().value, None, "an unknown cost is not 0");
    assert_eq!(unpriced.value(Metric::InputTokens).unwrap().value, Some(10.0), "its tokens still count");
}

#[test]
fn a_figure_that_was_zero_before_moves_without_a_percentage() {
    let mut reworked = task(2, "sonnet", Some(1_000_000));
    reworked.change_rounds = 2;
    reworked.clean = false;
    let report = report(&inputs(vec![reworked, task(9, "sonnet", Some(1_000_000))], vec![]), &query("period=7d&compare=previous"));
    let rounds = report.value(Metric::ChangeRounds).unwrap();
    assert_eq!((rounds.value, rounds.previous, rounds.change, rounds.better), (Some(2.0), Some(0.0), None, Some(false)));
    let clean = report.value(Metric::Clean).unwrap();
    assert_eq!((clean.value, clean.previous, clean.change, clean.better), (Some(0.0), Some(100.0), Some(-100.0), Some(false)));
    let tasks = report.value(Metric::Tasks).unwrap();
    assert_eq!((tasks.change, tasks.better), (Some(0.0), None), "the same count moved neither way");
}

#[test]
fn a_release_splits_the_time_since_it_started_from_as_long_before() {
    let mut inputs = inputs(vec![task(1, "sonnet", Some(1)), task(3, "sonnet", Some(1)), task(5, "sonnet", Some(1))], vec![]);
    inputs.releases = vec![Release { version: "0.3.4".into(), first_seen_at: NOW - 10 * DAY_MS }, Release { version: "0.3.5".into(), first_seen_at: NOW - 2 * DAY_MS }];
    let report = report(&inputs, &query("period=30d&compare=release:0.3.5"));
    assert_eq!(report.window, Window { from: Some(NOW - 2 * DAY_MS), until: NOW });
    assert_eq!(report.previous_window, Some(Window { from: Some(NOW - 4 * DAY_MS), until: NOW - 2 * DAY_MS }));
    assert_eq!((report.figures.sums.tasks, report.previous.unwrap().sums.tasks), (1, 1));
    assert_eq!(report.releases.iter().map(|r| r.version.as_str()).collect::<Vec<_>>(), ["0.3.5", "0.3.4"]);
}

#[test]
fn spend_without_a_task_counts_in_the_totals_but_not_per_task_or_per_project() {
    let mut inputs = inputs(vec![task(1, "sonnet", Some(1_000_000))], vec![]);
    inputs.days = vec![DayRow { day: NOW - DAY_MS, model: "opus".into(), cost_micros: 500_000, costed: 1, sessions: 3,
        tokens: Tokens { input: 1, output: 2, cache_read: 3, cache_write: 4 } }];
    let all = report(&inputs, &query("period=7d"));
    assert_eq!(all.value(Metric::CostTotal).unwrap().value, Some(1_500_000.0));
    assert_eq!(all.value(Metric::CostPerTask).unwrap().value, Some(1_000_000.0));
    assert_eq!(all.value(Metric::CostPerSession).unwrap().value, Some(300_000.0));
    let project = super::report(&inputs, &query("period=7d&project=alondra"));
    assert_eq!(project.value(Metric::CostTotal).unwrap().value, Some(1_000_000.0));
}

#[test]
fn a_query_reads_back_from_its_key_and_refuses_what_it_does_not_know() {
    let query = query("period=30d&compare=release:0.3.5&project=My%20App&model=sonnet&effort=high");
    assert_eq!(query.key(), "metrics?period=30d&compare=release:0.3.5&project=My%20App&model=sonnet&effort=high");
    assert_eq!(Query::of_key(&query.key()), Some(query));
    assert_eq!(Query::of_key("metrics"), Some(Query::default()));
    assert_eq!(Query::of_key("metrics?compare=date:2026-10-19").unwrap().compare, Compare::Date("2026-10-19".into()));
    for refused in ["metrics?period=1y", "metrics?compare=later", "metrics?colour=red", "metrics?compare=date:2026-13-01", "metricsx"] {
        assert_eq!(Query::of_key(refused), None, "{refused}");
    }
}

#[test]
fn a_day_is_its_midnight_utc() {
    assert_eq!(day_millis("1970-01-01"), Some(0));
    assert_eq!(day_millis("2026-10-26"), Some(20_752 * DAY_MS));
    assert_eq!(day_millis("2024-03-01").unwrap() - day_millis("2024-02-28").unwrap(), 2 * DAY_MS, "2024 is a leap year");
}
