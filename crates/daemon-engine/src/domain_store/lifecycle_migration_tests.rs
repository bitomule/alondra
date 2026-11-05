use super::*;
use crate::domain_store::tests::{Fixture, seeded};
use std::collections::BTreeSet;

fn node(id: &str) -> Actor {
    Actor::Agent { agent_id: id.into() }
}
fn run(store: &mut DomainStore, actor: &Actor, id: &str, command: Command) {
    let state = store.load().unwrap();
    let envelope = CommandEnvelope {
        protocol_version: 2,
        data_epoch: state.data_epoch.clone(),
        command_id: id.into(),
        expected_revision: state.revision,
        command,
    };
    store.execute_authenticated(actor, &envelope, None).unwrap_or_else(|error| panic!("{id}: {error:?}"));
}
fn create(store: &mut DomainStore, task: &str, node: Option<&str>, dependencies: &[&str]) {
    if let Some(node) = node {
        run(store, &Actor::System, &format!("register-{node}"), Command::RegisterAgent { agent_id: node.into(), name: node.into(), at: 0 });
    }
    run(store, &Actor::Orchestrator { generation: 0 }, &format!("create-{task}"), Command::CreateTask {
        plan_items: None, continues: None,
        task_id: task.into(),
        request_id: "request-1".into(),
        title: task.into(),
        agent_id: node.map(Into::into),
        review_prs: false,
        dependencies: dependencies.iter().map(|id| TaskId::from(*id)).collect::<BTreeSet<_>>(),
        stack: None,
        launch: Some(LaunchPlan::new("node", "/tmp")),
    });
}
fn present(store: &mut DomainStore, task: &str, node: &str) {
    run(store, &self::node(node), &format!("present-{task}"), Command::PresentResult {
        checklist: Vec::new(),
        task_id: task.into(),
        interaction_id: format!("delivery-{task}").as_str().into(),
        body: "Finished".into(),
        targets: vec![DeliveryTarget::Text { text: "The answer".into() }],
        is_final: true,
        nothing_to_review: false,
        attachments: vec![],
        card: CardSpec::delivery("Review the result", &[], true),
    });
}
fn end_node(store: &mut DomainStore, task: &str, node: &str) {
    run(store, &Actor::System, &format!("end-{task}"), Command::RecordNodeEnd { task_id: task.into(), end: NodeEnd::Closed, agent_id: Some(node.into()) });
}

/// One task of every shape a database of schema 4 holds today: not started with and without a
/// node, waiting, running, delivered, done, and cancelled with and without a node.
fn every_shape() -> (Fixture, DomainState) {
    let fixture = Fixture::new();
    let mut store = DomainStore::open(&fixture.path()).unwrap();
    seeded(&mut store);
    run(&mut store, &Actor::Orchestrator { generation: 0 }, "title", Command::TitleRequest { request_id: "request-1".into(), title: "Shapes".into() });
    create(&mut store, "new", None, &[]);
    create(&mut store, "assigned", Some("node-assigned"), &[]);
    create(&mut store, "waiting", None, &["new"]);
    create(&mut store, "running", Some("node-running"), &[]);
    run(&mut store, &node("node-running"), "start-running", Command::StartTask { task_id: "running".into() });
    create(&mut store, "review", Some("node-review"), &[]);
    run(&mut store, &node("node-review"), "start-review", Command::StartTask { task_id: "review".into() });
    present(&mut store, "review", "node-review");
    create(&mut store, "done", Some("node-done"), &[]);
    run(&mut store, &node("node-done"), "start-done", Command::StartTask { task_id: "done".into() });
    present(&mut store, "done", "node-done");
    run(&mut store, &Actor::User { user_id: "user-1".into() }, "accept-done", Command::AcceptDelivery {
        presentation_id: "delivery-done".into(),
        targets: vec![DeliveryTarget::Text { text: "The answer".into() }],
    });
    end_node(&mut store, "done", "node-done");
    create(&mut store, "cancelled", None, &[]);
    run(&mut store, &Actor::User { user_id: "user-1".into() }, "cancel-cancelled", Command::CancelTask { task_id: "cancelled".into() });
    run(&mut store, &Actor::System, "cleanup-cancelled", Command::RecordTaskCleanup { task_id: "cancelled".into() });
    create(&mut store, "cancelled-node", Some("node-cancelled"), &[]);
    run(&mut store, &node("node-cancelled"), "start-cancelled", Command::StartTask { task_id: "cancelled-node".into() });
    run(&mut store, &Actor::User { user_id: "user-1".into() }, "cancel-node", Command::CancelTask { task_id: "cancelled-node".into() });
    end_node(&mut store, "cancelled-node", "node-cancelled");
    let state = store.load().unwrap();
    let states: BTreeMap<&str, TaskState> = state.tasks.iter().map(|(id, t)| (id.0.as_str(), t.state)).collect();
    assert_eq!(states, BTreeMap::from([
        ("assigned", TaskState::NotStarted), ("cancelled", TaskState::Cancelled), ("cancelled-node", TaskState::Cancelled),
        ("done", TaskState::Done), ("new", TaskState::NotStarted), ("review", TaskState::ForYourReview),
        ("running", TaskState::Running), ("task-1", TaskState::NotStarted), ("waiting", TaskState::WaitingDependency),
    ]), "the fixture holds every shape");
    (fixture, DomainState::clone(&state))
}

/// The database as a build of schema 4 left it: the same tables and rows, without the store's
/// journal, and with the domain's own journal table when `kept` has one.
fn as_previous_schema(path: &Path, kept: &BTreeMap<TaskId, Vec<LifecycleRecord>>) {
    let conn = Connection::open(path).unwrap();
    conn.execute_batch("PRAGMA foreign_keys=OFF").unwrap();
    let objects: Vec<(String, String)> = conn
        .prepare("SELECT type,name FROM sqlite_schema WHERE (name GLOB 'lifecycle_*' OR name GLOB 'provider_*') AND type IN('trigger','index','table') ORDER BY type='table'")
        .unwrap()
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap();
    for (kind, name) in objects {
        if kind != "index" || !name.starts_with("sqlite_") {
            conn.execute_batch(&format!("DROP {} IF EXISTS \"{name}\"", kind.to_uppercase())).unwrap();
        }
    }
    if !kept.is_empty() {
        conn.execute_batch("CREATE TABLE task_lifecycle_journal(task_id TEXT NOT NULL REFERENCES tasks(id), seq INTEGER NOT NULL CHECK(seq>=0), record TEXT NOT NULL CHECK(json_valid(record)), PRIMARY KEY(task_id, seq));").unwrap();
        for (task, records) in kept {
            for (seq, record) in records.iter().enumerate() {
                conn.execute("INSERT INTO task_lifecycle_journal VALUES(?1,?2,?3)", params![task.0, seq as i64, serde_json::to_string(record).unwrap()]).unwrap();
            }
        }
    }
    conn.pragma_update(None, "user_version", PREVIOUS_SCHEMA_VERSION).unwrap();
}

fn tables(path: &Path) -> Vec<String> {
    Connection::open(path).unwrap()
        .prepare("SELECT name FROM sqlite_schema WHERE type='table' ORDER BY name").unwrap()
        .query_map([], |row| row.get(0)).unwrap()
        .collect::<rusqlite::Result<_>>().unwrap()
}

#[test]
fn a_previous_schema_moves_every_task_into_the_journal_and_every_task_reads_as_before() {
    let (fixture, before) = every_shape();
    as_previous_schema(&fixture.path(), &BTreeMap::new());
    let mut store = DomainStore::open(&fixture.path()).unwrap();
    let after = store.load().unwrap();
    assert_eq!(after.tasks, before.tasks, "every task keeps its state, node, start and delivery");
    assert_eq!(after.lifecycle_journal.keys().collect::<Vec<_>>(), after.tasks.keys().collect::<Vec<_>>(), "every task has a journal");
    assert_eq!(store.conn.pragma_query_value(None, "user_version", |r| r.get::<_, i64>(0)).unwrap(), SCHEMA_VERSION);
    assert!(!tables(&fixture.path()).contains(&"task_lifecycle_journal".to_owned()));
    let copy = lifecycle_migration::copy_path(&fixture.path().canonicalize().unwrap());
    let kept = Connection::open(&copy).unwrap();
    assert_eq!(kept.pragma_query_value(None, "user_version", |r| r.get::<_, i64>(0)).unwrap(), PREVIOUS_SCHEMA_VERSION, "the copy is the database as it was");
    use std::os::unix::fs::PermissionsExt;
    assert_eq!(std::fs::metadata(&copy).unwrap().permissions().mode() & 0o077, 0, "the copy is private");
    run(&mut store, &node("node-running"), "present-after", Command::PresentResult {
        checklist: Vec::new(), task_id: "running".into(), interaction_id: "delivery-after".into(), body: "Finished".into(),
        targets: vec![DeliveryTarget::Text { text: "The answer".into() }], is_final: true, nothing_to_review: false,
        attachments: vec![], card: CardSpec::delivery("Review the result", &[], true),
    });
    run(&mut store, &Actor::User { user_id: "user-1".into() }, "reopen-done", Command::ReopenTask { task_id: "done".into() });
    assert_eq!(store.load().unwrap().tasks[&TaskId::from("done")].state, TaskState::NotStarted, "a migrated task moves on through its journal");
}

#[test]
fn a_shape_the_migration_cannot_translate_changes_nothing_and_names_the_task() {
    let (fixture, _) = every_shape();
    as_previous_schema(&fixture.path(), &BTreeMap::new());
    Connection::open(fixture.path()).unwrap().execute("UPDATE tasks SET started=0 WHERE id='running'", []).unwrap();
    let rows = || -> Vec<(String, String, i64, Option<String>)> {
        Connection::open(fixture.path()).unwrap()
            .prepare("SELECT id,state,started,agent_id FROM tasks ORDER BY id").unwrap()
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))).unwrap()
            .collect::<rusqlite::Result<_>>().unwrap()
    };
    let (kept_rows, kept_tables) = (rows(), tables(&fixture.path()));
    let error = DomainStore::open(&fixture.path()).map(|_| ()).unwrap_err();
    let message = format!("{error:#}");
    assert!(message.contains("moving its tasks into the lifecycle journal failed") && message.contains("task running"), "{message}");
    assert_eq!(rows(), kept_rows, "no task changes");
    assert_eq!(tables(&fixture.path()), kept_tables, "the journal is not installed");
    let version: i64 = Connection::open(fixture.path()).unwrap().pragma_query_value(None, "user_version", |r| r.get(0)).unwrap();
    assert_eq!(version, PREVIOUS_SCHEMA_VERSION, "the database keeps its schema");
}

#[test]
fn a_journal_the_domain_kept_in_its_own_table_moves_as_it_is() {
    let (fixture, before) = every_shape();
    let mut kept = BTreeMap::new();
    for task in ["cancelled-node", "running", "review"] {
        let id = TaskId::from(task);
        let records: Vec<LifecycleRecord> = before.lifecycle_journal[&id].iter().map(|record| match record {
            LifecycleRecord::Drained { obligation: Drained::Execution { .. } } => LifecycleRecord::Drained {
                obligation: Drained::Execution { instance: before.tasks[&id].agent_id.clone().unwrap().0 },
            },
            other => other.clone(),
        }).collect();
        kept.insert(id, records);
    }
    as_previous_schema(&fixture.path(), &kept);
    let mut store = DomainStore::open(&fixture.path()).unwrap();
    let after = store.load().unwrap();
    for task in ["cancelled-node", "running", "review"] {
        let id = TaskId::from(task);
        assert_eq!(after.lifecycle_journal[&id], before.lifecycle_journal[&id], "{task}: its decisions come back, each execution with its own instance");
    }
    assert_eq!(after.tasks, before.tasks);
}

#[test]
fn a_current_schema_without_the_store_journal_is_refused_not_migrated() {
    let (fixture, _) = every_shape();
    as_previous_schema(&fixture.path(), &BTreeMap::new());
    Connection::open(fixture.path()).unwrap().pragma_update(None, "user_version", SCHEMA_VERSION).unwrap();
    let kept = tables(&fixture.path());
    let error = match DomainStore::open(&fixture.path()) {
        Ok(mut store) => store.load().map(|_| ()).unwrap_err(),
        Err(error) => error,
    };
    assert!(format!("{error:#}").contains("UnsupportedJournalFormat"), "{error:#}");
    assert_eq!(tables(&fixture.path()), kept, "nothing is installed in silence");
}

/// Opt-in rehearsal on a copy of a real database: `ALONDRA_REHEARSAL_DB` names a copy, which this
/// test only reads; it migrates its own copy of it and checks that every task reads as before.
#[test]
#[ignore = "opt-in: needs ALONDRA_REHEARSAL_DB, a copy of a real database"]
fn rehearse_the_migration_on_a_copy_of_a_real_database() {
    let source = PathBuf::from(std::env::var("ALONDRA_REHEARSAL_DB").expect("ALONDRA_REHEARSAL_DB names a copy"));
    let fixture = Fixture::new();
    {
        let from = Connection::open_with_flags(&source, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
        let mut to = Connection::open(fixture.path()).unwrap();
        rusqlite::backup::Backup::new(&from, &mut to).unwrap().run_to_completion(4096, std::time::Duration::ZERO, None).unwrap();
    }
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(fixture.path(), std::fs::Permissions::from_mode(0o600)).unwrap();
    let columns = || -> Vec<(String, String, i64, Option<String>, Option<String>)> {
        Connection::open(fixture.path()).unwrap()
            .prepare("SELECT id,state,started,agent_id,final_presentation FROM tasks ORDER BY id").unwrap()
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?))).unwrap()
            .collect::<rusqlite::Result<_>>().unwrap()
    };
    let before = columns();
    let mut store = DomainStore::open(&fixture.path()).unwrap_or_else(|error| panic!("{error:#}"));
    let state = store.load().unwrap();
    assert_eq!(columns(), before, "no task column changes");
    assert_eq!(state.lifecycle_journal.len(), state.tasks.len(), "every task has a journal");
    let mut shapes: BTreeMap<String, usize> = BTreeMap::new();
    for task in state.tasks.values() {
        *shapes.entry(task_shape(task)).or_default() += 1;
    }
    let events: i64 = store.conn.query_row("SELECT count(*) FROM lifecycle_events", [], |r| r.get(0)).unwrap();
    println!("rehearsal: {} tasks, {events} events, schema {}", state.tasks.len(), SCHEMA_VERSION);
    for (shape, count) in shapes {
        println!("rehearsal: {count:4} {shape}");
    }
}

/// A database from before a task could own several PRs: the column's UNIQUE and no columns for them.
fn with_one_pr_per_task(path: &Path) {
    let conn = Connection::open(path).unwrap();
    conn.execute_batch("PRAGMA foreign_keys=OFF").unwrap();
    let table: String = conn.query_row("SELECT sql FROM sqlite_schema WHERE type='table' AND name='pull_requests'", [], |row| row.get(0)).unwrap();
    let attached: Vec<String> = conn.prepare("SELECT sql FROM sqlite_schema WHERE tbl_name='pull_requests' AND type='trigger'").unwrap()
        .query_map([], |row| row.get(0)).unwrap().collect::<rusqlite::Result<_>>().unwrap();
    assert!(!attached.is_empty());
    conn.execute_batch(&format!("CREATE TABLE copied AS SELECT * FROM pull_requests; DROP TABLE pull_requests; {};
        INSERT INTO pull_requests SELECT * FROM copied; DROP TABLE copied;
        ALTER TABLE task_relations DROP COLUMN later_prs; ALTER TABLE task_relations DROP COLUMN opened_prs;",
        table.replace("task_id TEXT NOT NULL REFERENCES tasks(id)", "task_id TEXT NOT NULL UNIQUE REFERENCES tasks(id)"))).unwrap();
    for trigger in attached {
        conn.execute_batch(&trigger).unwrap();
    }
}

#[test]
fn a_database_with_one_pr_per_task_keeps_its_prs_and_takes_more() {
    let (fixture, _) = every_shape();
    let mut store = DomainStore::open(&fixture.path()).unwrap();
    run(&mut store, &node("node-running"), "first-pr", Command::RegisterPr {
        task_id: "running".into(), pr_id: "github.com/me/app#1".into(), host: "github.com".into(), repository: "me/app".into(), number: 1,
        head: "0123456789abcdef".into(), base: "main".into(), allowed_methods: vec![MergeMethod::Squash],
    });
    let before = store.load().unwrap();
    drop(store);
    with_one_pr_per_task(&fixture.path());

    let mut store = DomainStore::open(&fixture.path()).unwrap();
    let after = store.load().unwrap();
    assert_eq!(after.pull_requests, before.pull_requests);
    assert_eq!(after.tasks, before.tasks);
    let table: String = store.conn.query_row("SELECT sql FROM sqlite_schema WHERE name='pull_requests'", [], |row| row.get(0)).unwrap();
    assert!(!table.contains("task_id TEXT NOT NULL UNIQUE"), "{table}");
    let triggers: i64 = store.conn.query_row("SELECT count(*) FROM sqlite_schema WHERE type='trigger' AND tbl_name='pull_requests'", [], |row| row.get(0)).unwrap();
    assert_eq!(triggers, 2, "its triggers came back with it");
    let dangling: i64 = store.conn.query_row("SELECT count(*) FROM pragma_foreign_key_check", [], |row| row.get(0)).unwrap();
    assert_eq!(dangling, 0);

    run(&mut store, &node("node-running"), "second-pr", Command::RegisterPr {
        task_id: "running".into(), pr_id: "github.com/me/site#2".into(), host: "github.com".into(), repository: "me/site".into(), number: 2,
        head: "fedcba9876543210".into(), base: "main".into(), allowed_methods: vec![MergeMethod::Squash],
    });
    drop(store);
    let mut store = DomainStore::open(&fixture.path()).unwrap();
    let state = store.load().unwrap();
    assert_eq!(state.task_prs(&state.tasks[&"running".into()]).count(), 2);
}
