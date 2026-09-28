use super::*;
use std::collections::{BTreeMap, BTreeSet};
use std::os::unix::fs::DirBuilderExt;

fn temporary_database(label: &str) -> (std::path::PathBuf, std::path::PathBuf) {
    let directory = std::env::temp_dir().join(format!(
        "alondra-schedule-{label}-{}",
        crate::auth::random_hex(16)
    ));
    std::fs::DirBuilder::new()
        .mode(0o700)
        .create(&directory)
        .unwrap();
    let path = directory.join("domain.sqlite");
    (directory, path)
}

fn database(c: &Connection) {
    c.pragma_update(None, "foreign_keys", "ON").unwrap();
    c.execute_batch(super::super::SCHEMA).unwrap();
}

fn schedule() -> Schedule {
    Schedule {
        id: "schedule-1".into(),
        title: "Review".into(),
        text: "Review project".into(),
        project: Some("Alondra".into()),
        options: BTreeMap::new(),
        disallowed_tools: vec!["Bash".into()],
        frequency: ScheduleFrequency::Weekly,
        hour: 9,
        minute: 30,
        weekday: Some(1),
        interval: None,
        start_at: None,
        enabled: true,
        next_run: Some(3_000),
        revision: 2,
        deleted_at: None,
    }
}

fn run() -> ScheduleRun {
    ScheduleRun {
        id: "run-1".into(),
        schedule_id: "schedule-1".into(),
        origin: ScheduleRunOrigin::Due,
        due_at: Some(1_000),
        claimed_at: Some(1_001),
        claim_revision: Some(2),
        finished_at: None,
        state: ScheduleRunState::Launching,
        request_id: None,
        task_id: None,
        error: None,
        definition_snapshot: Some(ScheduleDefinitionSnapshot {
            title: "Review".into(),
            text: "Review project".into(),
            project: Some("Alondra".into()),
            options: BTreeMap::new(),
            disallowed_tools: vec!["Bash".into()],
        }),
        launch: None,
    }
}

fn launch() -> ScheduleLaunch {
    ScheduleLaunch {
        intent: ScheduleLaunchIntent {
            name: "scheduled/run-1".into(),
            cwd: "/synthetic/Projects/Alondra".into(),
            prompt: "Review project".into(),
            options_json: serde_json::to_string(&alondra_core::options::OptionsFile::default()
                .resolve(Some("Alondra"), &BTreeMap::new()).unwrap()).unwrap(),
        },
        state: ScheduleLaunchState::Pending,
        claimed_at: None,
        finished_at: None,
        session_id: None,
        error: None,
    }
}

fn linked_request_and_task(c: &Connection) {
    c.execute_batch("INSERT INTO requests(id,body,closed) VALUES ('request-1','Review project',0),('request-2','Other project',0);
        INSERT INTO tasks(id,request_id,agent_id,title,state,revision,started,review_prs,final_presentation,pr_id,archived)
        VALUES ('task-1','request-1',NULL,'Review','not_started',1,0,0,NULL,NULL,0),
               ('task-2','request-2',NULL,'Earlier review','done',1,1,0,NULL,NULL,0);")
        .unwrap();
}

fn execute(
    store: &mut super::super::DomainStore,
    id: &str,
    command: Command,
) -> std::result::Result<super::super::Commit, super::super::StoreError> {
    let state = store.load().unwrap();
    store.execute_authenticated(
        &Actor::System,
        &CommandEnvelope {
            protocol_version: PROTOCOL_VERSION,
            data_epoch: state.data_epoch,
            command_id: id.into(),
            expected_revision: state.revision,
            command,
        },
        None,
    )
}

#[test]
fn schedule_state_roundtrips_claim_result_and_legacy_observation() {
    let c = Connection::open_in_memory().unwrap();
    database(&c);
    linked_request_and_task(&c);
    let mut state = ScheduleState::default();
    state.schedules.insert("schedule-1".into(), schedule());
    state.runs.insert("run-1".into(), run());
    state.runs.insert(
        "legacy-run".into(),
        ScheduleRun {
            id: "legacy-run".into(),
            schedule_id: "schedule-1".into(),
            origin: ScheduleRunOrigin::LegacyLastObserved,
            due_at: None,
            claimed_at: None,
            claim_revision: None,
            finished_at: Some(900),
            state: ScheduleRunState::Sent,
            request_id: Some("request-2".into()),
            task_id: Some("task-2".into()),
            error: None,
            definition_snapshot: None,
            launch: None,
        },
    );
    save_schedule_state(&c, &state).unwrap();
    assert_eq!(load_schedule_state(&c).unwrap(), state);

    let claimed = state.runs.get_mut(&ScheduleRunId::from("run-1")).unwrap();
    claimed.state = ScheduleRunState::Sent;
    claimed.finished_at = Some(1_500);
    claimed.request_id = Some("request-1".into());
    claimed.task_id = Some("task-1".into());
    claimed.launch = Some(launch());
    save_schedule_state(&c, &state).unwrap();
    assert_eq!(load_schedule_state(&c).unwrap(), state);
    assert_eq!(
        load_schedule_request_tools(&c, &RequestId::from("request-1")).unwrap(),
        Some(vec!["Bash".into()])
    );

    let sent = state.runs.get_mut(&ScheduleRunId::from("run-1")).unwrap();
    sent.state = ScheduleRunState::Failed;
    sent.error = Some("Task launch failed later".into());
    sent.launch.as_mut().unwrap().state = ScheduleLaunchState::Failed;
    sent.launch.as_mut().unwrap().finished_at = Some(1_600);
    sent.launch.as_mut().unwrap().error = sent.error.clone();
    save_schedule_state(&c, &state).unwrap();
    assert_eq!(load_schedule_state(&c).unwrap(), state);
    assert!(!was_schedule_launch_failed(&c, &RequestId::from("request-1")).unwrap());

    let legacy = state
        .runs
        .get_mut(&ScheduleRunId::from("legacy-run"))
        .unwrap();
    legacy.state = ScheduleRunState::Failed;
    legacy.error = Some("Legacy launch failed later".into());
    save_schedule_state(&c, &state).unwrap();
    assert!(!was_schedule_launch_failed(&c, &RequestId::from("request-2")).unwrap());
}

#[test]
fn schedule_state_rejects_wrong_request_task_relation() {
    let c = Connection::open_in_memory().unwrap();
    database(&c);
    linked_request_and_task(&c);
    let mut state = ScheduleState::default();
    state.schedules.insert("schedule-1".into(), schedule());
    save_schedule_state(&c, &state).unwrap();
    let mut sent = run();
    sent.state = ScheduleRunState::Sent;
    sent.finished_at = Some(1_500);
    sent.request_id = Some("request-2".into());
    sent.task_id = Some("task-1".into());
    sent.launch = Some(launch());
    state.runs.insert(sent.id.clone(), sent);
    assert!(save_schedule_state(&c, &state).is_err());
    assert!(load_schedule_state(&c).unwrap().runs.is_empty());
}

#[test]
fn historical_policy_receipt_and_failed_launch_survive_restart() {
    let (directory, path) = temporary_database("historical");
    let request_id = RequestId::from("request-1");
    let patterns = vec!["Bash".into(), "mcp__danger__*".into()];
    let receipt = LegacyScheduleReceipt {
        sender: "phone".into(),
        request_id: "legacy-command-1".into(),
        digest: vec![7; 32],
        result: r#"{ "status": 201, "body": {"id":"schedule-1"}}"#.into(),
    };
    {
        let c = Connection::open(&path).unwrap();
        database(&c);
        let mut state = ScheduleState::default();
        state.schedules.insert("schedule-1".into(), schedule());
        save_schedule_state(&c, &state).unwrap();
        assert!(
            remember_schedule_request_tools(&c, &RequestId::from("missing-request"), &patterns,)
                .is_err()
        );
        c.execute(
            "INSERT INTO requests(id,body,closed) VALUES (?1,'Review project',0)",
            [&request_id.0],
        )
        .unwrap();
        remember_schedule_request_tools(&c, &request_id, &patterns).unwrap();
        remember_schedule_request_tools(&c, &request_id, &patterns).unwrap();
        assert!(remember_schedule_request_tools(&c, &request_id, &["Other".into()]).is_err());
        save_legacy_schedule_receipt(&c, &receipt).unwrap();
        save_legacy_schedule_receipt(&c, &receipt).unwrap();
        let mut changed = receipt.clone();
        changed.result = r#"{"status":500}"#.into();
        assert!(save_legacy_schedule_receipt(&c, &changed).is_err());
        remember_schedule_failed_launch(&c, &request_id).unwrap();
    }
    {
        let c = Connection::open(&path).unwrap();
        c.pragma_update(None, "foreign_keys", "ON").unwrap();
        assert_eq!(
            load_schedule_state(&c).unwrap().schedules[&ScheduleId::from("schedule-1")],
            schedule()
        );
        assert_eq!(
            load_schedule_request_tools(&c, &request_id).unwrap(),
            Some(patterns)
        );
        assert_eq!(
            load_legacy_schedule_receipt(&c, "phone", "legacy-command-1").unwrap(),
            Some(receipt)
        );
        assert!(was_schedule_launch_failed(&c, &request_id).unwrap());
        assert!(!was_schedule_launch_failed(&c, &RequestId::from("other")).unwrap());
    }
    std::fs::remove_dir_all(directory).unwrap();
}

#[test]
fn sent_run_request_task_policy_and_receipt_commit_together() {
    let (directory, path) = temporary_database("atomic");
    let mut store = super::super::DomainStore::open(&path).unwrap();
    let mut definition = schedule();
    definition.revision = 1;
    definition.next_run = Some(1_000);
    execute(
        &mut store,
        "create-schedule",
        Command::CreateSchedule {
            schedule: definition,
        },
    )
    .unwrap();
    execute(
        &mut store,
        "claim-run",
        Command::ClaimScheduleRun {
            schedule_id: "schedule-1".into(),
            run_id: "run-1".into(),
            origin: ScheduleRunOrigin::Due,
            now: 1_000,
            next_after_now: Some(3_000),
        },
    )
    .unwrap();
    let before = store.load().unwrap();
    store
        .conn
        .execute_batch("CREATE TRIGGER reject_tools BEFORE INSERT ON schedule_request_tools BEGIN SELECT RAISE(ABORT,'synthetic policy failure'); END;")
        .unwrap();
    let sent = Command::RecordScheduleRunSent {
        run_id: "run-1".into(),
        request: Request {
            id: "request-1".into(),
            body: "Review project".into(),
            closed: false,
        },
        task: Task {
            id: "task-1".into(),
            request_id: "request-1".into(),
            agent_id: None,
            title: "Review".into(),
            state: TaskState::NotStarted,
            revision: 1,
            started: false,
            review_prs: false,
            final_presentation: None,
            pr_id: None,
            dependencies: BTreeSet::new(),
            stack: None,
            documents: BTreeMap::new(),
            archived: false,
        },
        launch: ScheduleLaunchIntent {
            name: "scheduled/run-1".into(),
            cwd: "/synthetic/Projects/Alondra".into(),
            prompt: "Review project".into(),
            options_json: serde_json::to_string(
                &alondra_core::options::OptionsFile::default()
                    .resolve(Some("Alondra"), &BTreeMap::new())
                    .unwrap(),
            )
            .unwrap(),
        },
        finished_at: 1_500,
    };
    assert!(execute(&mut store, "sent-run", sent.clone()).is_err());
    assert_eq!(store.load().unwrap(), before);
    for table in ["requests", "tasks", "schedule_request_tools"] {
        let count: i64 = store
            .conn
            .query_row(&format!("SELECT count(*) FROM {table}"), [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 0, "{table} should roll back");
    }
    let receipt_count: i64 = store
        .conn
        .query_row(
            "SELECT count(*) FROM semantic_receipts WHERE command_id='sent-run'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(receipt_count, 0);
    store
        .conn
        .execute_batch("DROP TRIGGER reject_tools")
        .unwrap();

    let commit = execute(&mut store, "sent-run", sent).unwrap();
    assert_eq!(store.load().unwrap(), commit.state);
    assert_eq!(
        commit.state.schedule.runs[&ScheduleRunId::from("run-1")].state,
        ScheduleRunState::Sent
    );
    assert_eq!(
        load_schedule_request_tools(&store.conn, &RequestId::from("request-1")).unwrap(),
        Some(vec!["Bash".into()])
    );
    let receipt_count: i64 = store
        .conn
        .query_row(
            "SELECT count(*) FROM semantic_receipts WHERE command_id='sent-run'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(receipt_count, 1);
    drop(store);
    let mut reopened = super::super::DomainStore::open(&path).unwrap();
    assert_eq!(reopened.load().unwrap(), commit.state);
    execute(
        &mut reopened,
        "late-failure",
        Command::RecordScheduleRunFailed {
            run_id: "run-1".into(),
            reason: "Task launch failed later".into(),
            finished_at: 2_000,
        },
    )
    .unwrap();
    assert!(was_schedule_launch_failed(&reopened.conn, &RequestId::from("request-1")).unwrap());
    drop(reopened);
    std::fs::remove_dir_all(directory).unwrap();
}
