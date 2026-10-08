//! Executable C/S slices of V54–V62. These use real domain commands and SQLite;
//! the catalog keeps the complete A/U and runner scenarios pending.
use super::*;
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};
use std::os::unix::fs::DirBuilderExt;

const CATALOG: &str = include_str!("../../../../fixtures/domain-v2/acceptance.json");

struct Fixture {
    directory: std::path::PathBuf,
    path: std::path::PathBuf,
}
impl Fixture {
    fn new() -> Self {
        let directory = std::env::temp_dir().join(format!(
            "alondra-schedule-cs-{}",
            crate::auth::random_hex(12)
        ));
        std::fs::DirBuilder::new()
            .mode(0o700)
            .create(&directory)
            .unwrap();
        let path = directory.join("domain.db");
        Self { directory, path }
    }
    fn open(&self) -> DomainStore {
        DomainStore::open(&self.path).unwrap()
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.directory).unwrap();
    }
}

fn catalog() -> Value {
    serde_json::from_str(CATALOG).unwrap()
}
fn scenario(id: &str) -> Value {
    let catalog = catalog();
    let case = catalog["scenarios"]
        .as_array()
        .unwrap()
        .iter()
        .find(|case| case["id"] == id)
        .unwrap_or_else(|| panic!("missing {id} scenario"));
    assert!(
        matches!(
            case["execution_status"].as_str(),
            Some("schedule_runner" | "test_evidence")
        ),
        "{id} is executed here or by the named tests; its runner/A/U layers stay listed as remaining"
    );
    assert!(
        case["layers"]
            .as_array()
            .unwrap()
            .iter()
            .any(|layer| layer == "S")
    );
    case.clone()
}
fn expected<'a>(case: &'a Value, key: &str) -> &'a Value {
    case["cs_expected"]
        .get(key)
        .unwrap_or_else(|| panic!("missing C/S expectation {key}"))
}
fn schedule() -> Schedule {
    serde_json::from_value(catalog()["schedule_base"].clone()).unwrap()
}
fn envelope(store: &mut DomainStore, id: &str, command: Command) -> CommandEnvelope {
    let state = store.load().unwrap();
    CommandEnvelope {
        protocol_version: PROTOCOL_VERSION,
        data_epoch: state.data_epoch.clone(),
        command_id: id.into(),
        expected_revision: state.revision,
        command,
    }
}
fn execute(store: &mut DomainStore, id: &str, command: Command) -> Result<Commit, StoreError> {
    let envelope = envelope(store, id, command);
    store.execute_authenticated(&Actor::System, &envelope, None)
}
fn rows(store: &DomainStore, table: &str) -> i64 {
    let query = match table {
        "schedules"
        | "schedule_runs"
        | "requests"
        | "tasks"
        | "schedule_request_tools"
        | "schedule_failed_launches"
        | "semantic_receipts"
        | "local_command_results" => format!("SELECT count(*) FROM {table}"),
        _ => panic!("unapproved acceptance table {table}"),
    };
    store.conn.query_row(&query, [], |row| row.get(0)).unwrap()
}
fn assert_domain_code(error: StoreError, code: &str) {
    match error {
        StoreError::Domain(error) => assert_eq!(error.code, code),
        other => panic!("expected domain error {code}, got {other}"),
    }
}
fn create(store: &mut DomainStore, definition: Schedule) {
    execute(
        store,
        "create",
        Command::CreateSchedule {
            schedule: definition,
        },
    )
    .unwrap();
}
fn claim(
    store: &mut DomainStore,
    id: &str,
    origin: ScheduleRunOrigin,
    now: i64,
    next_after_now: Option<i64>,
) -> Result<Commit, StoreError> {
    execute(
        store,
        &format!("claim-{id}"),
        Command::ClaimScheduleRun {
            schedule_id: "schedule-acceptance".into(),
            run_id: id.into(),
            origin,
            now,
            next_after_now,
            expected_schedule_revision: None,
        },
    )
}
fn sent(run: &str, suffix: &str, finished_at: i64) -> Command {
    let request_id = format!("request-{suffix}");
    Command::RecordScheduleRunSent {
        run_id: run.into(),
        request: Request::new(request_id.clone().into(), "Synthetic mission".into()),
        task: Task {
            id: format!("task-{suffix}").into(),
            request_id: request_id.into(),
            agent_id: None,
            title: "Synthetic schedule".into(),
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
            model: None,
            delivered_dependencies: Default::default(), blocks: Default::default(), verdict: None, review_rounds: 0, wrap_up: Default::default(), priority: Default::default(),
        },
        launch: ScheduleLaunchIntent {
            name: format!("scheduled/{run}"),
            cwd: "/synthetic/Projects".into(),
            prompt: "Synthetic mission".into(),
            options_json: serde_json::to_string(
                &alondra_core::options::OptionsFile::default()
                    .resolve(None, &BTreeMap::new())
                    .unwrap(),
            )
            .unwrap(),
        },
        finished_at,
    }
}

#[test]
fn v54_cs_crud_revision_and_transport_receipts() {
    let case = scenario("V54");
    let fixture = Fixture::new();
    let mut store = fixture.open();
    let initial = schedule();
    let create = envelope(
        &mut store,
        "create",
        Command::CreateSchedule { schedule: initial },
    );
    let first = store
        .execute_authenticated(&Actor::System, &create, Some(("phone", "create", &[1; 32])))
        .unwrap();
    assert!(!first.replayed);
    let revision = store.load().unwrap().revision;
    let replay = store
        .execute_authenticated(&Actor::System, &create, Some(("phone", "create", &[1; 32])))
        .unwrap();
    assert!(replay.replayed);
    assert_eq!(store.load().unwrap().revision, revision);
    assert_domain_code(
        store
            .execute_authenticated(&Actor::System, &create, Some(("phone", "create", &[2; 32])))
            .unwrap_err(),
        "idempotency_conflict",
    );

    let mut update = schedule();
    update.title = "Edited mission".into();
    update.revision = 2;
    let update_command = Command::UpdateSchedule {
        schedule: update.clone(),
    };
    let update_envelope = envelope(&mut store, "update", update_command.clone());
    store
        .execute_authenticated(
            &Actor::System,
            &update_envelope,
            Some(("phone", "update", &[3; 32])),
        )
        .unwrap();
    let before_stale = store.load().unwrap();
    assert_domain_code(
        execute(&mut store, "stale-update", update_command).unwrap_err(),
        "stale_revision",
    );
    assert_eq!(store.load().unwrap(), before_stale);

    let delete = envelope(
        &mut store,
        "delete",
        Command::DeleteSchedule {
            schedule_id: update.id.clone(),
            deleted_at: 4_000,
            expected_schedule_revision: None,
        },
    );
    let deleted = store
        .execute_authenticated(&Actor::System, &delete, Some(("phone", "delete", &[4; 32])))
        .unwrap();
    assert!(!deleted.replayed);
    assert!(
        store
            .execute_authenticated(&Actor::System, &delete, Some(("phone", "delete", &[4; 32])))
            .unwrap()
            .replayed
    );
    let persisted = store.load().unwrap();
    let stored = &persisted.schedule.schedules[&update.id];
    assert_eq!(stored.deleted_at, Some(4_000));
    assert!(!stored.enabled && stored.next_run.is_none());
    assert_eq!(
        rows(&store, "schedules"),
        expected(&case, "schedule_rows").as_i64().unwrap()
    );
    assert_eq!(
        rows(&store, "local_command_results"),
        expected(&case, "transport_receipts").as_i64().unwrap()
    );
    store.integrity_check().unwrap();
}

#[test]
fn v55_cs_overdue_claim_consumes_one_observed_slot() {
    let case = scenario("V55");
    let fixture = Fixture::new();
    let mut store = fixture.open();
    create(&mut store, schedule());
    claim(
        &mut store,
        "run-due",
        ScheduleRunOrigin::Due,
        10_000,
        Some(11_000),
    )
    .unwrap();
    let state = store.load().unwrap();
    let run = &state.schedule.runs[&ScheduleRunId::from("run-due")];
    assert_eq!(
        run.due_at,
        Some(expected(&case, "due_at").as_i64().unwrap())
    );
    assert_eq!(
        state.schedule.schedules[&ScheduleId::from("schedule-acceptance")].next_run,
        Some(expected(&case, "next_run").as_i64().unwrap())
    );
    execute(
        &mut store,
        "failed-launch",
        Command::RecordScheduleRunFailed {
            run_id: "run-due".into(),
            reason: "synthetic launch failure".into(),
            finished_at: 10_001,
        },
    )
    .unwrap();
    match claim(
        &mut store,
        "duplicate-due",
        ScheduleRunOrigin::Due,
        10_000,
        Some(11_000),
    )
    .unwrap_err()
    {
        StoreError::Domain(error) => {
            assert_eq!(error.code, "conflict");
            assert_eq!(error.message, "schedule is not due");
        }
        other => panic!("expected not-due conflict, got {other}"),
    }
    assert_eq!(
        rows(&store, "schedule_runs"),
        expected(&case, "run_rows").as_i64().unwrap()
    );
    store.integrity_check().unwrap();
}

#[test]
fn v56_cs_manual_disabled_runs_have_distinct_links() {
    let case = scenario("V56");
    let fixture = Fixture::new();
    let mut store = fixture.open();
    let mut definition = schedule();
    definition.enabled = false;
    definition.next_run = None;
    create(&mut store, definition);
    claim(&mut store, "run-1", ScheduleRunOrigin::Manual, 1_000, None).unwrap();
    execute(&mut store, "sent-1", sent("run-1", "1", 1_001)).unwrap();
    claim(&mut store, "run-2", ScheduleRunOrigin::Manual, 2_000, None).unwrap();
    execute(&mut store, "sent-2", sent("run-2", "2", 2_001)).unwrap();
    let state = store.load().unwrap();
    for suffix in ["1", "2"] {
        let run = &state.schedule.runs[&ScheduleRunId::from(format!("run-{suffix}"))];
        assert_eq!(run.state, ScheduleRunState::Sent);
        assert_eq!(
            run.request_id,
            Some(RequestId::from(format!("request-{suffix}")))
        );
        assert_eq!(run.task_id, Some(TaskId::from(format!("task-{suffix}"))));
        assert_eq!(
            state.tasks[&TaskId::from(format!("task-{suffix}"))].state,
            TaskState::NotStarted
        );
    }
    assert_eq!(
        rows(&store, "schedule_runs"),
        expected(&case, "run_rows").as_i64().unwrap()
    );
    assert_eq!(
        rows(&store, "requests"),
        expected(&case, "request_rows").as_i64().unwrap()
    );
    assert_eq!(
        rows(&store, "tasks"),
        expected(&case, "task_rows").as_i64().unwrap()
    );
    assert!(
        state.schedule.schedules[&ScheduleId::from("schedule-acceptance")]
            .next_run
            .is_none()
    );
    store.integrity_check().unwrap();
}

#[test]
fn v57_cs_restart_interrupts_claim_without_replaying_due_slot() {
    let case = scenario("V57");
    let fixture = Fixture::new();
    {
        let mut store = fixture.open();
        create(&mut store, schedule());
        claim(
            &mut store,
            "run-1",
            ScheduleRunOrigin::Due,
            1_000,
            Some(2_000),
        )
        .unwrap();
    }
    let mut restarted = fixture.open();
    let state = restarted.load().unwrap();
    assert_eq!(
        state.schedule.runs[&ScheduleRunId::from("run-1")].state,
        ScheduleRunState::Interrupted
    );
    assert_eq!(
        state.schedule.schedules[&ScheduleId::from("schedule-acceptance")].next_run,
        Some(expected(&case, "next_run").as_i64().unwrap())
    );
    assert_eq!(
        rows(&restarted, "requests"),
        expected(&case, "request_rows").as_i64().unwrap()
    );
    assert_eq!(
        rows(&restarted, "tasks"),
        expected(&case, "task_rows").as_i64().unwrap()
    );
    assert_domain_code(
        claim(
            &mut restarted,
            "replay",
            ScheduleRunOrigin::Due,
            1_000,
            Some(2_000),
        )
        .unwrap_err(),
        "conflict",
    );
    restarted.integrity_check().unwrap();
}

#[test]
fn v58_cs_sent_links_policy_and_receipt_commit_together() {
    let case = scenario("V58");
    let fixture = Fixture::new();
    let mut store = fixture.open();
    create(&mut store, schedule());
    claim(
        &mut store,
        "run-1",
        ScheduleRunOrigin::Due,
        1_000,
        Some(2_000),
    )
    .unwrap();
    let sent = envelope(&mut store, "sent-1", sent("run-1", "1", 1_001));
    let before = store.load().unwrap();
    store
        .conn
        .execute_batch(
            "CREATE TRIGGER reject_policy BEFORE INSERT ON schedule_request_tools
        BEGIN SELECT RAISE(ABORT,'synthetic policy failure'); END;",
        )
        .unwrap();
    assert!(
        store
            .execute_authenticated(&Actor::System, &sent, None)
            .is_err()
    );
    assert_eq!(store.load().unwrap(), before);
    for table in ["requests", "tasks", "schedule_request_tools"] {
        assert_eq!(rows(&store, table), 0, "{table} must roll back");
    }
    let receipt: i64 = store
        .conn
        .query_row(
            "SELECT count(*) FROM semantic_receipts WHERE command_id='sent-1'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(receipt, 0);
    store
        .conn
        .execute_batch("DROP TRIGGER reject_policy")
        .unwrap();
    let committed = store
        .execute_authenticated(&Actor::System, &sent, None)
        .unwrap();
    assert!(!committed.replayed);
    assert!(
        store
            .execute_authenticated(&Actor::System, &sent, None)
            .unwrap()
            .replayed
    );
    assert_eq!(
        rows(&store, "requests"),
        expected(&case, "request_rows").as_i64().unwrap()
    );
    assert_eq!(
        rows(&store, "tasks"),
        expected(&case, "task_rows").as_i64().unwrap()
    );
    assert_eq!(
        rows(&store, "schedule_request_tools"),
        expected(&case, "policy_rows").as_i64().unwrap()
    );
    let persisted = store.load().unwrap();
    let run = &persisted.schedule.runs[&ScheduleRunId::from("run-1")];
    assert_eq!(run.state, ScheduleRunState::Sent);
    assert_eq!(run.request_id, Some("request-1".into()));
    assert_eq!(run.task_id, Some("task-1".into()));
    store.integrity_check().unwrap();
}

#[test]
fn v62_cs_older_launch_failure_does_not_replace_latest_run() {
    let case = scenario("V62");
    let fixture = Fixture::new();
    let mut store = fixture.open();
    let mut definition = schedule();
    definition.enabled = false;
    definition.next_run = None;
    create(&mut store, definition);
    claim(&mut store, "first", ScheduleRunOrigin::Manual, 1_000, None).unwrap();
    execute(&mut store, "sent-first", sent("first", "1", 1_001)).unwrap();
    claim(&mut store, "second", ScheduleRunOrigin::Manual, 2_000, None).unwrap();
    execute(&mut store, "sent-second", sent("second", "2", 2_001)).unwrap();
    let before = store.load().unwrap();
    let revision = before.schedule.schedules[&ScheduleId::from("schedule-acceptance")].revision;
    execute(
        &mut store,
        "fail-old",
        Command::RecordScheduleRunFailed {
            run_id: "first".into(),
            reason: "old launcher failed".into(),
            finished_at: 3_000,
        },
    )
    .unwrap();
    let after = store.load().unwrap();
    assert_eq!(
        after.schedule.schedules[&ScheduleId::from("schedule-acceptance")].revision,
        revision
    );
    let first = &after.schedule.runs[&ScheduleRunId::from("first")];
    let second = &after.schedule.runs[&ScheduleRunId::from("second")];
    assert!(first.claim_revision < second.claim_revision);
    assert_eq!(first.state, ScheduleRunState::Failed);
    assert_eq!(second.state, ScheduleRunState::Sent);
    assert_eq!(first.finished_at, Some(1_001));
    assert_eq!(
        rows(&store, "schedule_failed_launches"),
        expected(&case, "failure_markers").as_i64().unwrap()
    );
    assert_eq!(
        rows(&store, "semantic_receipts"),
        expected(&case, "semantic_receipts").as_i64().unwrap()
    );
    store.integrity_check().unwrap();
}

#[test]
fn schedule_catalog_has_explicit_partial_cs_evidence_and_pending_layers() {
    assert!(
        catalog()["schedule_acceptance_status_note"]
            .as_str()
            .unwrap()
            .contains("complete scenario pending")
    );
    let cases = ["V54", "V55", "V56", "V57", "V58", "V62"];
    for id in cases {
        let case = scenario(id);
        assert!(
            !case["cs_evidence"].as_array().unwrap().is_empty(),
            "{id} evidence mapping"
        );
        assert_eq!(case["cs_evidence_status"], "passed", "{id} C/S slice");
        assert!(
            !case["remaining"].as_array().unwrap().is_empty(),
            "{id} pending gate"
        );
        assert_ne!(case["remaining"], json!([]));
    }
}

#[test]
fn schedule_mutations_are_judged_by_their_schedule_not_by_unrelated_writes() {
    let fixture = Fixture::new();
    let mut store = fixture.open();
    create(&mut store, schedule());
    let mut edited = schedule();
    edited.title = "Edited mission".into();
    edited.revision = 2;
    let update = envelope(
        &mut store,
        "update-from-old-snapshot",
        Command::UpdateSchedule { schedule: edited },
    );
    let run = envelope(
        &mut store,
        "run-from-old-snapshot",
        Command::ClaimScheduleRun {
            schedule_id: "schedule-acceptance".into(),
            run_id: "manual-run".into(),
            origin: ScheduleRunOrigin::Manual,
            now: 500,
            next_after_now: Some(1000),
            expected_schedule_revision: Some(2),
        },
    );
    let unrelated = envelope(
        &mut store,
        "unrelated",
        Command::CreateRequest {
            request_id: "unrelated-request".into(),
            body: "Unrelated work".into(),
            auto_merge: false, maintenance: false, message: false,
        },
    );
    store
        .execute_authenticated(
            &Actor::User {
                user_id: "user-1".into(),
            },
            &unrelated,
            None,
        )
        .unwrap();
    store
        .execute_authenticated(&Actor::System, &update, None)
        .unwrap();
    store
        .execute_authenticated(&Actor::System, &run, None)
        .unwrap();
    let stale_delete = envelope(
        &mut store,
        "delete-from-before-the-edit",
        Command::DeleteSchedule {
            schedule_id: "schedule-acceptance".into(),
            deleted_at: 4_000,
            expected_schedule_revision: Some(1),
        },
    );
    let before = store.load().unwrap();
    assert_domain_code(
        store
            .execute_authenticated(&Actor::System, &stale_delete, None)
            .unwrap_err(),
        "stale_revision",
    );
    assert_eq!(store.load().unwrap(), before);
}
