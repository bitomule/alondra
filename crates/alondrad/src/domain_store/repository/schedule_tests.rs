use super::*;
use std::collections::BTreeMap;

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
    }
}

fn linked_request_and_task(c: &Connection) {
    c.execute_batch("INSERT INTO requests(id,body,closed) VALUES ('request-1','Review project',0),('request-2','Other project',0);
        INSERT INTO tasks(id,request_id,agent_id,title,state,revision,started,review_prs,final_presentation,pr_id,archived)
        VALUES ('task-1','request-1',NULL,'Review','not_started',1,0,0,NULL,NULL,0),
               ('task-2','request-2',NULL,'Earlier review','done',1,1,0,NULL,NULL,0);")
        .unwrap();
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
        },
    );
    save_schedule_state(&c, &state).unwrap();
    assert_eq!(load_schedule_state(&c).unwrap(), state);

    let claimed = state.runs.get_mut(&ScheduleRunId::from("run-1")).unwrap();
    claimed.state = ScheduleRunState::Sent;
    claimed.finished_at = Some(1_500);
    claimed.request_id = Some("request-1".into());
    claimed.task_id = Some("task-1".into());
    save_schedule_state(&c, &state).unwrap();
    assert_eq!(load_schedule_state(&c).unwrap(), state);

    let sent = state.runs.get_mut(&ScheduleRunId::from("run-1")).unwrap();
    sent.state = ScheduleRunState::Failed;
    sent.error = Some("Task launch failed later".into());
    save_schedule_state(&c, &state).unwrap();
    assert_eq!(load_schedule_state(&c).unwrap(), state);
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
    state.runs.insert(sent.id.clone(), sent);
    assert!(save_schedule_state(&c, &state).is_err());
    assert!(load_schedule_state(&c).unwrap().runs.is_empty());
}

#[test]
fn historical_policy_receipt_and_failed_launch_survive_restart() {
    let path = std::env::temp_dir().join(format!(
        "alondra-schedule-repository-{}.sqlite",
        crate::auth::random_hex(16)
    ));
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
    std::fs::remove_file(path).unwrap();
}
