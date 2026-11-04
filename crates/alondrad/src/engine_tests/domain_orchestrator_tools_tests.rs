//! The orchestrator's tools over the daemon's own routes.
use crate::domain_store::model_metrics::Decision;
use crate::domain_store::model_metrics::DecisionKind;
use alondra_daemon_engine::domain_orchestrator_tools::tests::*;
use alondra_daemon_engine::domain_orchestrator_tools::*;
use crate::domain_runtime::DomainRuntime;
use crate::test_support::Directory;
use alondra_core::domain::*;
use serde_json::{Value, json};

/// What `alondra metrics --json` shows once a sonnet task launched by the orchestrator went
/// through a review that asked for changes twice and the user accepted it, every call made over
/// the daemon's own routes.
#[tokio::test]
async fn a_review_that_asks_for_changes_twice_shows_its_rounds_in_the_metrics() {
    let root = Directory::new();
    let home = root.join("home");
    std::fs::create_dir_all(&home).unwrap();
    drop(DomainRuntime::open(&home).unwrap());
    let daemon = crate::domain_bootstrap::DomainDaemon::open(&home).unwrap();
    let domain = daemon.domain();
    command(&domain, user(), Command::CreateRequest { request_id: "r".into(), body: "Make the editor readable".into(), auto_merge: false, maintenance: false, message: false });
    command(&domain, user(), Command::TitleRequest { request_id: "r".into(), title: "Readable editor".into() });
    command(&domain, Actor::Orchestrator { generation: 0 }, Command::CreateTask { plan_items: None, continues: None, task_id: "t".into(), request_id: "r".into(), title: "Editor colours".into(),
        agent_id: None, review_prs: false, dependencies: Default::default(), stack: None,
        launch: Some(alondra_core::domain::LaunchPlan::new("node", "/tmp")), });
    command(&domain, Actor::System, Command::RegisterAgent { agent_id: "reviewer".into(), name: "Reviewer".into(), at: 0 });
    command(&domain, Actor::Orchestrator { generation: 0 }, Command::CreateTask { plan_items: None, continues: None, task_id: "review".into(), request_id: "r".into(), title: "Review the editor colours".into(),
        agent_id: Some("reviewer".into()), review_prs: false, dependencies: Default::default(), stack: None,
        launch: Some(alondra_core::domain::LaunchPlan::new("node", "/tmp")), });
    command(&domain, user(), Command::SetRelations { task_id: "review".into(), delivered_dependencies: ["t".into()].into(), blocks: ["t".into()].into() });

    command(&domain, Actor::System, Command::RegisterAgent { agent_id: "worker".into(), name: "editor".into(), at: 0 });
    command(&domain, Actor::Orchestrator { generation: 0 }, Command::AssignTask { task_id: "t".into(), agent_id: "worker".into(), model: None });
    record(&domain, Decision { task_id: "t".into(), request_id: "r".into(), agent_id: Some("worker".into()), at: alondra_core::now_millis(),
        kind: DecisionKind::Launch, model: Some("sonnet".into()), effort: Some("medium".into()), approach: None,
        chosen_by: ModelChooser::Orchestrator, reason: Some("Colour tokens in one file: sonnet".into()), title: "Editor colours".into(),
        provider: None, tags: None });
    let worker = domain.bind_agent("worker").unwrap().expose_for_child_environment().to_owned();
    let reviewer = domain.bind_agent("reviewer").unwrap().expose_for_child_environment().to_owned();

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let app = crate::domain_bootstrap::router(daemon.clone()).layer(axum::middleware::from_fn_with_state(daemon.auth(), crate::auth::guard));
    let server = tokio::spawn(async move {
        axum::serve(listener, app.into_make_service_with_connect_info::<std::net::SocketAddr>()).await.unwrap();
    });
    let client = reqwest::Client::new();
    let epoch = domain.state().unwrap().data_epoch.0.clone();
    let mut sent = 0;
    let mut call = async |bearer: &str, path: String, mut body: Value| {
        sent += 1;
        body["data_epoch"] = json!(epoch);
        body["command_id"] = json!(format!("call-{sent}"));
        body["expected_revision"] = json!(0);
        let response = client.post(format!("{base}{path}")).bearer_auth(bearer).json(&body).send().await.unwrap();
        let status = response.status();
        assert!(status.is_success(), "{path}: {status} {}", response.text().await.unwrap());
    };
    let delivery = |task: &str, id: &str| json!({"task_id": task, "interaction_id": id, "body": "Done", "is_final": true, "attachments": [],
        "targets": [{"kind": "text", "text": "The result"}],
        "card": serde_json::to_value(alondra_core::domain::CardSpec::delivery("Review the result", &[], true)).unwrap()});

    call(&worker, "/tasks/t/start".into(), json!({"task_id": "t"})).await;
    let verdicts = [
        json!({"kind": "changes_requested", "text": "Empty list crashes\nContrast below 4.5:1", "serious_findings": 2}),
        json!({"kind": "changes_requested", "text": "Contrast still below 4.5:1", "serious_findings": 1}),
        json!({"kind": "approved", "summary": "Fixed"}),
    ];
    for (round, verdict) in verdicts.into_iter().enumerate() {
        call(&worker, "/tasks/t/results".into(), delivery("t", &format!("delivery-{round}"))).await;
        if round == 0 {
            call(&reviewer, "/tasks/review/start".into(), json!({"task_id": "review"})).await;
        }
        call(&reviewer, "/tasks/review/results".into(), delivery("review", &format!("review-{round}"))).await;
        call(&reviewer, "/tasks/review/verdict".into(), json!({"task_id": "review", "presentation_id": format!("review-{round}"), "verdict": verdict})).await;
    }

    command(&domain, user(), Command::AcceptDelivery { presentation_id: "delivery-2".into(),
        targets: vec![alondra_core::domain::DeliveryTarget::Text { text: "The result".into() }] });
    command(&domain, Actor::System, Command::RecordNodeEnd { task_id: "t".into(), end: alondra_core::domain::NodeEnd::Closed, agent_id: None });

    let report: Value = client.get(format!("{base}/metrics?period=all&compare=none")).send().await.unwrap().json().await.unwrap();
    println!("GET /metrics (what alondra metrics --json prints):\n{}", serde_json::to_string_pretty(&report).unwrap());
    let sonnet = &report["by_model"][0];
    assert_eq!((&sonnet["name"], &sonnet["effort"], &sonnet["figures"]["sums"]["tasks"]), (&json!("sonnet"), &json!("medium"), &json!(1)));
    let sums = &sonnet["figures"]["sums"];
    assert_eq!((&sums["change_rounds"], &sums["reviewer_rounds"], &sums["clean"]), (&json!(2), &json!(2), &json!(0)));
    let mut inputs = domain.metrics_inputs(0).unwrap();
    let task = inputs.tasks.iter().find(|task| task.model.as_deref() == Some("sonnet")).expect("the fold kept the completed sonnet task");
    assert_eq!((task.effort.as_deref(), task.change_rounds, task.reviewer_rounds, task.clean), (Some("medium"), 2, 2, false));
    inputs.now = task.finished_at;
    let at_end = alondra_core::metrics::report::report(&inputs, &alondra_core::metrics::report::Query::parse("period=all&compare=none").unwrap());
    let sonnet = at_end.by_model.iter().find(|group| group.name == "sonnet").expect("a report at the task's end includes its model");
    assert_eq!((sonnet.effort.as_deref(), sonnet.figures.sums.tasks, sonnet.figures.sums.change_rounds, sonnet.figures.sums.reviewer_rounds),
        (Some("medium"), 1, 2, 2));
    server.abort();
}
