use alondra_store::{AuthorityScope, Error, LifecycleTransaction};
use rusqlite::{Connection, TransactionBehavior};
#[path = "../../test_support.rs"]
mod test_support;

struct Database {
    connection: Connection,
    _directory: test_support::Directory,
}
impl Database {
    fn domain() -> Self {
        let directory = test_support::Directory::new();
        std::fs::create_dir_all(&*directory).unwrap();
        let connection = Connection::open(directory.join("domain.db")).unwrap();
        connection.execute_batch("PRAGMA foreign_keys=ON;").unwrap();
        connection
            .execute_batch(include_str!("../../alondrad/src/domain_store/schema.sql"))
            .unwrap();
        connection.execute_batch("INSERT INTO domain_meta VALUES(1,'epoch',0,0); INSERT INTO requests(id,body,closed) VALUES('request','Work',0); INSERT INTO agents(id,name) VALUES('node','Node'); INSERT INTO tasks(id,request_id,title,state,revision,started,review_prs,archived) VALUES('task','request','Task','not_started',0,0,0,0);").unwrap();
        Self {
            connection,
            _directory: directory,
        }
    }
    fn legacy(with_rows: bool) -> Self {
        let mut database = Self::domain();
        database
            .connection
            .execute_batch(include_str!("../fixtures/legacy-journal.sql"))
            .unwrap();
        if with_rows {
            let tx = database
                .connection
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .unwrap();
            tx.execute_batch("INSERT INTO lifecycle_tasks VALUES('task','epoch','initial',2); INSERT INTO lifecycle_inputs VALUES('task','initial','Retained original prompt'); INSERT INTO lifecycle_executions VALUES('task',1,'node','instance','conversation','host'); INSERT INTO lifecycle_current_execution VALUES('task',1,'node'); INSERT INTO lifecycle_dispatches VALUES(1,'task',1,'initial','launch','/owned/task','prepared',NULL);").unwrap();
            tx.execute("INSERT INTO lifecycle_events VALUES('task',1,'admit',?1)",[r#"{"kind":"admit","data":{"node":"node","conversation":"conversation","instance":"instance","host":"host"}}"#]).unwrap();
            tx.execute("INSERT INTO lifecycle_events VALUES('task',2,'initialized',?1)",[r#"{"kind":"runtime","data":{"stamp":{"task":"task","node":"node","conversation":"conversation","instance":"instance","host":"host","generation":1},"cursor":1,"fact":{"kind":"initialized"}}}"#]).unwrap();
            tx.commit().unwrap();
        }
        database
    }
    fn snapshot(&self) -> (Vec<(String, String)>, Vec<String>) {
        let schema = self
            .connection
            .prepare("SELECT name,coalesce(sql,'') FROM sqlite_master ORDER BY name")
            .unwrap()
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
            .unwrap()
            .collect::<rusqlite::Result<Vec<_>>>()
            .unwrap();
        let dispatch_columns = self
            .connection
            .prepare("SELECT name FROM pragma_table_info('lifecycle_dispatches') ORDER BY cid")
            .unwrap()
            .query_map([], |row| row.get::<_, String>(0))
            .unwrap()
            .collect::<rusqlite::Result<Vec<_>>>()
            .unwrap();
        let dispatch_sql = format!(
            "SELECT json_array({}) FROM lifecycle_dispatches ORDER BY id",
            dispatch_columns
                .iter()
                .map(|column| format!("\"{}\"", column.replace('"', "\"\"")))
                .collect::<Vec<_>>()
                .join(",")
        );
        let mut rows = vec![];
        for sql in [
            "SELECT json_array(task_id,epoch,initial_input,revision) FROM lifecycle_tasks ORDER BY task_id",
            "SELECT json_array(task_id,input_id,body) FROM lifecycle_inputs ORDER BY task_id,input_id",
            "SELECT json_array(task_id,revision,command_id,event) FROM lifecycle_events ORDER BY task_id,revision",
            "SELECT json_array(task_id,generation,node_id,instance_id,conversation_id,host_id) FROM lifecycle_executions ORDER BY task_id,generation",
            "SELECT json_array(task_id,generation,node_id) FROM lifecycle_current_execution ORDER BY task_id",
            dispatch_sql.as_str(),
        ] {
            rows.extend(
                self.connection
                    .prepare(sql)
                    .unwrap()
                    .query_map([], |row| row.get::<_, String>(0))
                    .unwrap()
                    .collect::<rusqlite::Result<Vec<_>>>()
                    .unwrap(),
            );
        }
        (schema, rows)
    }
}
#[test]
fn legacy_tables_are_never_blessed_even_when_empty_and_refusal_preserves_all_bytes() {
    for with_rows in [false, true] {
        let mut database = Database::legacy(with_rows);
        let before = database.snapshot();
        let mut tx = database
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .unwrap();
        assert!(matches!(
            LifecycleTransaction::install(&mut tx),
            Err(Error::UnsupportedJournalFormat)
        ));
        drop(tx);
        assert_eq!(database.snapshot(), before);
        let tx = database
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .unwrap();
        assert!(matches!(
            LifecycleTransaction::attach(tx, AuthorityScope::new("epoch".into()).unwrap()),
            Err(Error::UnsupportedJournalFormat)
        ));
        assert_eq!(database.snapshot(), before);
    }
}
#[test]
fn fresh_lifecycle_journal_is_valid_inside_an_existing_domain_database() {
    let mut database = Database::domain();
    let mut tx = database
        .connection
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .unwrap();
    LifecycleTransaction::install(&mut tx).unwrap();
    tx.commit().unwrap();
    let tx = database
        .connection
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .unwrap();
    let tx =
        LifecycleTransaction::attach(tx, AuthorityScope::new("epoch".into()).unwrap()).unwrap();
    tx.commit().unwrap();
    let tx = database
        .connection
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .unwrap();
    let tx =
        LifecycleTransaction::attach(tx, AuthorityScope::new("epoch".into()).unwrap()).unwrap();
    assert!(matches!(tx.load(&"task".into()), Err(Error::MissingTask)));
    assert!(matches!(tx.commit(), Err(Error::Poisoned)));
}
#[test]
fn unsupported_marker_is_refused_without_rewriting_its_value_or_schema() {
    let mut database = Database::domain();
    let mut tx = database
        .connection
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .unwrap();
    LifecycleTransaction::install(&mut tx).unwrap();
    tx.commit().unwrap();
    database
        .connection
        .execute(
            "UPDATE lifecycle_journal_format SET format='unknown-retained-format'",
            [],
        )
        .unwrap();
    let before = database.snapshot();
    let mut tx = database
        .connection
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .unwrap();
    assert!(matches!(
        LifecycleTransaction::install(&mut tx),
        Err(Error::UnsupportedJournalFormat)
    ));
    drop(tx);
    assert_eq!(database.snapshot(), before);
    let marker: String = database
        .connection
        .query_row("SELECT format FROM lifecycle_journal_format", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(marker, "unknown-retained-format");
}

fn schema_snapshot(connection: &Connection) -> Vec<(String, String)> {
    connection
        .prepare("SELECT name,coalesce(sql,'') FROM sqlite_master ORDER BY name")
        .unwrap()
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
        .unwrap()
        .collect::<rusqlite::Result<Vec<_>>>()
        .unwrap()
}
#[test]
fn partial_and_view_masquerades_cannot_be_reported_as_an_installed_journal() {
    for corruption in [
        "CREATE VIEW lifecycle_journal_format AS SELECT 1 AS singleton,'alondra-task-resources-v1' AS format",
        "CREATE TABLE lifecycle_journal_format(singleton INTEGER PRIMARY KEY CHECK(singleton=1),format TEXT NOT NULL); INSERT INTO lifecycle_journal_format VALUES(1,'alondra-task-resources-v1')",
        "CREATE TABLE lifecycle_journal_format(singleton INTEGER PRIMARY KEY CHECK(singleton=1),format TEXT NOT NULL); INSERT INTO lifecycle_journal_format VALUES(1,'alondra-task-resources-v1'); CREATE VIEW lifecycle_tasks AS SELECT 'task' AS task_id",
    ] {
        let mut database = Database::domain();
        database.connection.execute_batch(corruption).unwrap();
        let before = schema_snapshot(&database.connection);
        let mut tx = database
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .unwrap();
        assert!(matches!(
            LifecycleTransaction::install(&mut tx),
            Err(Error::UnsupportedJournalFormat)
        ));
        tx.commit().unwrap();
        assert_eq!(schema_snapshot(&database.connection), before);
    }
}
#[test]
fn caught_install_failure_cannot_commit_partial_ddl_or_a_format_marker() {
    let mut database = Database::domain();
    database.connection.execute_batch("CREATE TABLE sentinel(value TEXT); INSERT INTO sentinel VALUES('retained'); CREATE UNIQUE INDEX lifecycle_one_launch ON sentinel(value)").unwrap();
    let before = schema_snapshot(&database.connection);
    let mut tx = database
        .connection
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .unwrap();
    assert!(matches!(
        LifecycleTransaction::install(&mut tx),
        Err(Error::UnsupportedJournalFormat)
    ));
    // Deliberately commit after catching the error: only the install savepoint protects us.
    tx.commit().unwrap();
    assert_eq!(schema_snapshot(&database.connection), before);
    let value: String = database
        .connection
        .query_row("SELECT value FROM sentinel", [], |row| row.get(0))
        .unwrap();
    assert_eq!(value, "retained");
    assert!(
        !database
            .connection
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE name='lifecycle_journal_format')",
                [],
                |row| row.get::<_, bool>(0)
            )
            .unwrap()
    );
}

#[test]
fn missing_or_changed_owned_trigger_is_refused_before_attach_without_repairs() {
    for (trigger, replace) in [
        ("lifecycle_event_immutable", false),
        ("execution_owner_monotonic", false),
        ("lifecycle_required_card_stays_actionable", false),
        ("lifecycle_event_immutable", true),
    ] {
        let mut database = Database::domain();
        let mut tx = database.connection.transaction().unwrap();
        LifecycleTransaction::install(&mut tx).unwrap();
        tx.commit().unwrap();
        database
            .connection
            .execute_batch(&format!("DROP TRIGGER {trigger}"))
            .unwrap();
        if replace {
            database
                .connection
                .execute_batch(&format!(
                    "CREATE TRIGGER {trigger} BEFORE UPDATE ON lifecycle_events BEGIN SELECT 1; END"
                ))
                .unwrap();
        }
        let before = schema_snapshot(&database.connection);
        let raw = database.connection.transaction().unwrap();
        assert!(
            matches!(
                LifecycleTransaction::attach(raw, AuthorityScope::new("epoch".into()).unwrap()),
                Err(Error::UnsupportedJournalFormat)
            ),
            "untrusted owned trigger: {trigger}, replacement: {replace}"
        );
        assert_eq!(schema_snapshot(&database.connection), before);
    }
}
