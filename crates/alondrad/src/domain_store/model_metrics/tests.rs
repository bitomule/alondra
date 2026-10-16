use super::*;
use crate::domain_store::tests::{Fixture, seeded};
use alondra_core::domain::*;

fn decision(task: &str, agent: &str, at: Millis, kind: DecisionKind, model: &str, effort: Option<&str>, approach: Approach) -> Decision {
    Decision {
        task_id: task.into(), request_id: "request-1".into(), agent_id: Some(agent.into()), at, kind,
        model: Some(model.into()), effort: effort.map(str::to_owned), approach: Some(approach),
        chosen_by: ModelChooser::Orchestrator, reason: Some(format!("why {task}")), title: format!("Title {task}"),
    }
}

fn row(d: Decision) -> DecisionRow {
    DecisionRow {
        task_id: d.task_id, agent_id: d.agent_id, at: d.at, kind: d.kind.as_str().into(), model: d.model, effort: d.effort,
        approach: d.approach.map(|a| approach_text(a).to_owned()), reason: d.reason, title: d.title,
    }
}

fn history(rows: &[(Millis, &str)]) -> Vec<(Millis, String)> {
    rows.iter().map(|(at, state)| (*at, (*state).to_owned())).collect()
}

/// t1 finishes after one question; t2 is reopened and had changes asked on its PR; t3 is
/// relaunched from sonnet to opus, which finishes it; t4 is cancelled; t5 gets a message on its
/// node, which continues the attempt, then is handed to another node that finishes it.
fn facts() -> Facts {
    use Approach::*;
    use DecisionKind::*;
    let mut f = Facts::default();
    let mut send_without_model = decision("t5", "a7", 300, Send, "sonnet", None, Workflow);
    send_without_model.reason = None;
    f.decisions = [
        decision("t1", "a1", 100, Launch, "sonnet", Some("medium"), Solo),
        decision("t2", "a2", 100, Launch, "sonnet", Some("medium"), Subagents),
        decision("t3", "a3", 100, Launch, "sonnet", Some("medium"), Solo),
        decision("t3", "a4", 500, Relaunch, "opus", Some("high"), Solo),
        decision("t4", "a5", 100, Launch, "haiku", None, Solo),
        decision("t5", "a6", 100, Launch, "sonnet", Some("medium"), Solo),
        decision("t5", "a6", 200, Send, "sonnet", None, Workflow),
        send_without_model,
    ].into_iter().map(row).collect();
    f.history = [
        ("t1", history(&[(110, "running"), (120, "needs_you"), (130, "running"), (200, "done")])),
        ("t2", history(&[(110, "running"), (150, "done"), (160, "running"), (300, "done")])),
        ("t3", history(&[(110, "running"), (400, "not_started"), (510, "running"), (700, "done")])),
        ("t4", history(&[(110, "running"), (150, "cancelled")])),
        ("t5", history(&[(110, "running"), (400, "done")])),
    ].into_iter().map(|(task, rows)| (task.to_owned(), rows)).collect();
    f.changes = [("t2".to_owned(), vec![ChangeRound { at: 155, by_reviewer: false, on_pull_request: true }])].into();
    f.costs = [("a1", "t1", 1.0), ("a4", "t3", 3.0), ("a5", "other-task", 9.0)].into_iter()
        .map(|(session, task, cost)| (session.to_owned(), (Some(task.to_owned()), Some(cost)))).collect();
    f
}

fn find<'a>(groups: &'a [Group], model: &str, effort: Option<&str>) -> &'a Group {
    groups.iter().find(|g| g.model.as_deref() == Some(model) && g.effort.as_deref() == effort).unwrap_or_else(|| panic!("no {model} {effort:?} in {groups:#?}"))
}

#[test]
fn the_report_counts_each_outcome_by_model_and_effort_and_by_approach() {
    let report = report(&facts(), None, 1_000);
    let sonnet = find(&report.by_model, "sonnet", Some("medium"));
    assert_eq!(
        (sonnet.tasks, sonnet.done_clean, sonnet.reopened, sonnet.relaunched, sonnet.cancelled, sonnet.questions, sonnet.changes_requested),
        (4, 1, 1, 1, 0, 1, 1), "t1, t2, t3's first launch and t5's first node"
    );
    assert_eq!(sonnet.relaunched_to, [("opus".to_owned(), 1)].into());
    assert_eq!(sonnet.median_ms_to_done, Some(75), "t1 took 100 and t2 50 to reach done");
    assert_eq!((sonnet.mean_cost_usd, sonnet.costed), (Some(1.0), 1));
    assert_eq!(sonnet.percent(sonnet.done_clean), 25);

    let opus = find(&report.by_model, "opus", Some("high"));
    assert_eq!((opus.tasks, opus.done_clean, opus.relaunched, opus.median_ms_to_done, opus.mean_cost_usd), (1, 1, 0, Some(200), Some(3.0)));
    let haiku = find(&report.by_model, "haiku", None);
    assert_eq!((haiku.tasks, haiku.cancelled, haiku.done_clean, haiku.mean_cost_usd), (1, 1, 0, None), "a cost recorded for another task is not this one's");
    let handed = find(&report.by_model, "sonnet", None);
    assert_eq!((handed.tasks, handed.done_clean, handed.median_ms_to_done), (1, 1, Some(100)), "a task handed to another node is a new attempt");
    assert_eq!(report.by_model[0].model.as_deref(), Some("sonnet"), "the most used first");

    let approach = |name: &str| report.by_approach.iter().find(|g| g.approach.as_deref() == Some(name)).unwrap().tasks;
    assert_eq!((approach("solo"), approach("subagents"), approach("workflow")), (5, 1, 1));

    let trouble: Vec<(&str, &[Outcome], Option<&str>, Option<&str>)> = report.sonnet_trouble.iter()
        .map(|a| (a.task_id.as_str(), a.outcomes.as_slice(), a.relaunched_to.as_deref(), a.reason.as_deref())).collect();
    assert_eq!(trouble, [
        ("t2", &[Outcome::Reopened, Outcome::ChangesRequested][..], None, Some("why t2")),
        ("t3", &[Outcome::Relaunched][..], Some("opus"), Some("why t3")),
    ]);
}

#[test]
fn since_keeps_the_attempts_that_started_in_the_period_with_their_whole_outcome() {
    let report = report(&facts(), Some(400), 1_000);
    assert_eq!(report.by_model.len(), 1);
    assert_eq!((report.by_model[0].model.as_deref(), report.by_model[0].done_clean), (Some("opus"), 1));
    assert!(report.sonnet_trouble.is_empty());
}

fn store() -> (Fixture, DomainStore) {
    let fixture = Fixture::new();
    let mut store = DomainStore::open(&fixture.path()).unwrap();
    seeded(&mut store);
    (fixture, store)
}

fn run_command(store: &mut DomainStore, actor: &Actor, id: &str, command: Command) {
    let state = store.load().unwrap();
    let envelope = CommandEnvelope { protocol_version: PROTOCOL_VERSION, data_epoch: state.data_epoch.clone(), command_id: id.into(), expected_revision: state.revision, command };
    store.execute_authenticated(actor, &envelope, None).unwrap();
}

fn sonnet_launch() -> Decision {
    decision("task-1", "agent-1", 0, DecisionKind::Launch, "sonnet", Some("medium"), Approach::Solo)
}

#[test]
fn the_report_reads_the_tasks_history_and_the_changes_asked_on_its_pr_from_the_store() {
    let (_fixture, mut store) = store();
    store.record_model_decision(&sonnet_launch()).unwrap();
    let (user, agent) = (Actor::User { user_id: "user-1".into() }, Actor::Agent { agent_id: "agent-1".into() });
    run_command(&mut store, &agent, "start", Command::StartTask { task_id: "task-1".into() });
    run_command(&mut store, &agent, "pr", Command::RegisterPr {
        task_id: "task-1".into(), pr_id: "pr-1".into(), host: "github.com".into(), repository: "owner/repository".into(),
        number: 1, head: "aaa".into(), base: "main".into(), allowed_methods: vec![MergeMethod::Squash],
    });
    let targets = vec![DeliveryTarget::PullRequest { pr_id: "pr-1".into(), head: "aaa".into() }];
    run_command(&mut store, &agent, "present", Command::PresentResult {
        checklist: Vec::new(),
        task_id: "task-1".into(), interaction_id: "delivery-1".into(), body: "The PR".into(), targets: targets.clone(),
        is_final: true, nothing_to_review: false, attachments: vec![], card: CardSpec::delivery("Review the PR", &[], true),
    });
    run_command(&mut store, &user, "changes", Command::SubmitFeedback {
        attachments: vec![], presentation_id: "delivery-1".into(), targets, body: "Rename it".into(), instruction: Instruction::Changes,
    });
    run_command(&mut store, &user, "cancel", Command::CancelTask { task_id: "task-1".into() });
    run_command(&mut store, &Actor::System, "end", Command::RecordNodeEnd { task_id: "task-1".into(), end: NodeEnd::Closed, agent_id: Some("agent-1".into()) });

    let report = store.model_report(None, alondra_core::now_millis()).unwrap();
    let sonnet = find(&report.by_model, "sonnet", Some("medium"));
    assert_eq!((sonnet.tasks, sonnet.changes_requested, sonnet.cancelled, sonnet.done_clean), (1, 1, 1, 0));
    assert_eq!(report.sonnet_trouble[0].outcomes, [Outcome::Cancelled, Outcome::ChangesRequested]);
    assert_eq!(report.sonnet_trouble[0].title, "Title task-1");
}

#[test]
fn decisions_can_be_neither_changed_nor_removed() {
    let (_fixture, mut store) = store();
    store.record_model_decision(&sonnet_launch()).unwrap();
    let update = store.conn.execute("UPDATE model_decisions SET model='opus'", []).unwrap_err().to_string();
    assert!(update.contains("model decisions are immutable"), "{update}");
    let delete = store.conn.execute("DELETE FROM model_decisions", []).unwrap_err().to_string();
    assert!(delete.contains("model decisions are retained"), "{delete}");
    assert_eq!(store.model_decision_rows().unwrap().len(), 1);
}

#[test]
fn a_session_that_ends_twice_keeps_its_earliest_start_its_highest_readings_and_its_last_end() {
    let (_fixture, mut store) = store();
    store.record_model_decision(&sonnet_launch()).unwrap();
    let first = NodeCost { session_id: "agent-1".into(), task_id: None, model: Some("claude-sonnet-5-5".into()), cost_usd: Some(2.0),
        context_peak_percent: Some(70.0), started_at: Some(10), ended_at: 100 };
    store.record_node_cost(&first).unwrap();
    store.record_node_cost(&NodeCost { model: None, cost_usd: Some(1.5), context_peak_percent: None, started_at: Some(50), ended_at: 200, ..first.clone() }).unwrap();
    assert_eq!(store.node_cost("agent-1").unwrap(), Some(NodeCost { task_id: Some("task-1".into()), ended_at: 200, ..first }),
        "the task comes from the session's model decision");
}

#[test]
fn a_store_made_before_model_metrics_opens_in_place_and_keeps_its_rows() {
    let (fixture, mut store) = store();
    store.record_model_decision(&sonnet_launch()).unwrap();
    let before = store.load().unwrap();
    store.conn.execute_batch("DROP TABLE model_decisions; DROP TABLE node_costs;").unwrap();
    drop(store);

    let mut reopened = DomainStore::open(&fixture.path()).unwrap();
    assert_eq!(reopened.load().unwrap(), before, "the domain is as it was");
    assert_eq!(reopened.conn.pragma_query_value(None, "user_version", |r| r.get::<_, i64>(0)).unwrap(), crate::domain_store::SCHEMA_VERSION);
    assert!(!fixture.0.read_dir().unwrap().any(|e| e.unwrap().file_name().to_string_lossy().contains(".old-")), "nothing was moved aside");
    assert!(reopened.model_decision_rows().unwrap().is_empty());
    reopened.record_model_decision(&sonnet_launch()).unwrap();
    assert!(reopened.conn.execute("DELETE FROM model_decisions", []).is_err(), "the recreated table is guarded too");
    drop(reopened);

    let again = DomainStore::open(&fixture.path()).unwrap();
    assert_eq!(again.model_decision_rows().unwrap().len(), 1, "a reopen keeps recorded decisions");
}

#[test]
fn tasks_are_grouped_by_the_role_of_their_launch_plan() {
    let mut f = facts();
    f.roles = [("t1", "design"), ("t2", "implementation"), ("t3", "implementation"), ("t4", "review")]
        .into_iter().map(|(task, role)| (task.to_owned(), role.to_owned())).collect();
    let report = report(&f, None, Millis::MAX);
    let roles: Vec<(&str, u32)> = report.by_role.iter().map(|g| (g.role.as_deref().unwrap(), g.tasks)).collect();
    assert_eq!(roles, [("design", 1), ("implementation", 3), ("review", 1)], "in the order design, implementation, review; t5 has no role");
    assert_eq!(report.by_role[0].done_clean, 1);
    assert_eq!(report.by_role[2].cancelled, 1);
    let without = super::report(&facts(), None, Millis::MAX);
    assert!(without.by_role.is_empty(), "no roles, no section");
}

#[test]
fn the_roles_come_from_the_launch_plans_in_the_store() {
    let (_fixture, mut store) = store();
    store.record_model_decision(&sonnet_launch()).unwrap();
    let plan = crate::launch_plans::LaunchPlan {
        name: "app/review".into(), cwd: "/synthetic".into(), mission: None, model: None, effort: None, approach: None, reason: None,
        role: Some(alondra_core::domain::TaskRole::Review),
    };
    store.set_launch_plan(&"task-1".into(), &plan, 1).unwrap();
    let report = store.model_report(None, alondra_core::now_millis()).unwrap();
    assert_eq!(report.by_role.iter().map(|g| (g.role.clone().unwrap(), g.tasks)).collect::<Vec<_>>(), [("review".to_owned(), 1)]);
}

fn round(at: Millis, by_reviewer: bool) -> ChangeRound {
    ChangeRound { at, by_reviewer, on_pull_request: true }
}

#[test]
fn each_attempt_counts_its_rounds_of_changes_and_the_serious_findings_of_its_reviews() {
    let mut f = facts();
    f.changes.insert("t1".into(), vec![round(140, true), round(150, false), round(160, true)]);
    f.changes.insert("t3".into(), vec![round(300, true), round(600, true)]);
    f.findings = [("t1".to_owned(), vec![(140, 2), (160, 1)]), ("t3".to_owned(), vec![(300, 4), (600, 1)])].into();
    let report = report(&f, None, 1_000);

    let attempt = |task: &str, model: &str| report.attempts.iter().find(|a| a.task_id == task && a.model.as_deref() == Some(model)).unwrap();
    let counts = |a: &Attempt| (a.change_rounds, a.reviewer_rounds, a.serious_findings);
    assert_eq!(counts(attempt("t1", "sonnet")), (3, 2, 3), "two reviewer rounds and one from the user");
    assert_eq!(counts(attempt("t3", "sonnet")), (1, 1, 4), "the round after the relaunch is not the first attempt's");
    assert_eq!(counts(attempt("t3", "opus")), (1, 1, 1));
    assert_eq!(counts(attempt("t5", "sonnet")), (0, 0, 0));

    let sonnet = find(&report.by_model, "sonnet", Some("medium"));
    assert_eq!((sonnet.change_rounds, sonnet.reviewer_rounds, sonnet.serious_findings), (5, 3, 7), "t1 3, t2 1 and t3 1");
    assert_eq!(sonnet.mean_change_rounds(), 1.25);
    let opus = find(&report.by_model, "opus", Some("high"));
    assert_eq!((opus.change_rounds, opus.serious_findings), (1, 1));
    assert_eq!(report.attempts.len(), 7, "every attempt, not only the troubled ones");
}

#[test]
fn a_change_asked_on_a_delivery_without_a_pr_is_a_round_but_not_a_changed_pr() {
    let mut f = facts();
    f.changes.insert("t1".into(), vec![ChangeRound { at: 150, by_reviewer: false, on_pull_request: false }]);
    let report = report(&f, None, 1_000);
    let t1 = report.attempts.iter().find(|a| a.task_id == "t1").unwrap();
    assert_eq!((t1.change_rounds, t1.outcomes.as_slice()), (1, &[][..]));
}

fn text_delivery(task: &str, id: &str) -> Command {
    Command::PresentResult {
        checklist: Vec::new(),
        task_id: task.into(), interaction_id: id.into(), body: "Done".into(), targets: vec![DeliveryTarget::Text { text: "The answer".into() }],
        is_final: true, nothing_to_review: false, attachments: vec![], card: CardSpec::delivery("Review the result", &[], true),
    }
}

#[test]
fn the_rounds_and_serious_findings_of_a_review_come_from_the_store() {
    let (_fixture, mut store) = store();
    store.record_model_decision(&sonnet_launch()).unwrap();
    let (user, agent, reviewer) = (Actor::User { user_id: "user-1".into() }, Actor::Agent { agent_id: "agent-1".into() }, Actor::Agent { agent_id: "agent-2".into() });
    run_command(&mut store, &Actor::System, "agent-2", Command::RegisterAgent { agent_id: "agent-2".into(), name: "Reviewer".into(), at: 0 });
    run_command(&mut store, &user, "request-title", Command::TitleRequest { request_id: "request-1".into(), title: "Useful work".into() });
    run_command(&mut store, &Actor::Orchestrator { generation: 0 }, "task-2", Command::CreateTask { plan_items: None,
        task_id: "task-2".into(), request_id: "request-1".into(), title: "Review".into(), agent_id: Some("agent-2".into()),
        review_prs: false, dependencies: Default::default(), stack: None,
        launch: Some(alondra_core::domain::LaunchPlan::new("node", "/tmp")),
    });
    run_command(&mut store, &user, "relations", Command::SetRelations {
        task_id: "task-2".into(), delivered_dependencies: ["task-1".into()].into(), blocks: ["task-1".into()].into(),
    });
    run_command(&mut store, &agent, "start", Command::StartTask { task_id: "task-1".into() });
    let verdicts = [
        Verdict::ChangesRequested { text: "Empty list crashes\nTotal is wrong".into(), serious_findings: Some(2) },
        Verdict::ChangesRequested { text: "Total is still wrong".into(), serious_findings: Some(1) },
        Verdict::Approved { summary: "Fine".into(), open_issues: false, serious_findings: 0 },
    ];
    for (n, verdict) in verdicts.into_iter().enumerate() {
        run_command(&mut store, &agent, &format!("present-{n}"), text_delivery("task-1", &format!("delivery-{n}")));
        if n == 0 {
            run_command(&mut store, &reviewer, "start-review", Command::StartTask { task_id: "task-2".into() });
        }
        run_command(&mut store, &reviewer, &format!("review-{n}"), text_delivery("task-2", &format!("review-{n}")));
        run_command(&mut store, &reviewer, &format!("verdict-{n}"), Command::RecordVerdict {
            task_id: "task-2".into(), presentation_id: format!("review-{n}").into(), verdict,
        });
    }

    let report = store.model_report(None, alondra_core::now_millis()).unwrap();
    let attempt = &report.attempts[0];
    assert_eq!((attempt.task_id.as_str(), attempt.change_rounds, attempt.reviewer_rounds, attempt.serious_findings), ("task-1", 2, 2, 3));
    let rows: Vec<(String, String, u32)> = store.conn.prepare("SELECT reviewer_task_id,kind,serious_findings FROM review_findings WHERE task_id='task-1' ORDER BY at_revision").unwrap()
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?))).unwrap().collect::<Result<_, _>>().unwrap();
    assert_eq!(rows, [
        ("task-2".to_owned(), "changes_requested".to_owned(), 2),
        ("task-2".to_owned(), "changes_requested".to_owned(), 1),
        ("task-2".to_owned(), "approved".to_owned(), 0),
    ], "one row per round, though the reviewer's verdict is replaced each time");
    assert!(store.conn.execute("DELETE FROM review_findings", []).is_err(), "rounds are retained");
}
