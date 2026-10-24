use super::*;
use crate::domain_store::tests::{Fixture, seeded};
use crate::domain_store::DomainStore;
use alondra_core::domain::*;
use rusqlite::Connection;

const DAY: i64 = 24 * 60 * 60 * 1_000;
const RECEIPTS_BOUND_TO_AUDIT: &str = "CREATE TABLE semantic_receipts(actor TEXT NOT NULL,command_id TEXT NOT NULL,data_epoch TEXT NOT NULL,digest BLOB NOT NULL CHECK(length(digest)=32),result TEXT NOT NULL CHECK(json_valid(result)),PRIMARY KEY(actor,command_id,data_epoch))";

fn envelope(store: &mut DomainStore, id: &str, command: Command) -> CommandEnvelope {
    let state = store.load().unwrap();
    CommandEnvelope { protocol_version: 2, data_epoch: state.data_epoch.clone(), command_id: id.into(), expected_revision: state.revision, command }
}
fn agent() -> Actor {
    Actor::Agent { agent_id: "agent-1".into() }
}
fn present() -> Command {
    Command::PresentResult {
        checklist: Vec::new(),
        task_id: "task-1".into(),
        interaction_id: "delivery-1".into(),
        body: "Finished the answer".into(),
        targets: vec![DeliveryTarget::Text { text: "The answer".into() }],
        is_final: true,
        nothing_to_review: false,
        attachments: vec![],
        card: CardSpec::delivery("Review the result", &[], true),
    }
}
/// Four commits: the three of `seeded`, ten days old, and a delivery of today.
fn store_with_history(fixture: &Fixture) -> (DomainStore, CommandEnvelope, CommandEnvelope, i64) {
    let mut store = DomainStore::open(&fixture.path()).unwrap();
    seeded(&mut store);
    let old = store.conn.query_row("SELECT max(revision) FROM command_audit", [], |row| row.get::<_, i64>(0)).unwrap();
    let request = Command::CreateRequest { request_id: "request-1".into(), body: "Do useful work".into(), auto_merge: false, maintenance: false, message: false };
    let first = CommandEnvelope { expected_revision: 0, ..envelope(&mut store, "request", request) };
    let delivery = envelope(&mut store, "present", present());
    store.execute_authenticated(&agent(), &delivery, None).unwrap();
    let now = store.conn.query_row("SELECT max(at) FROM command_audit", [], |row| row.get::<_, i64>(0)).unwrap();
    store.conn.execute("UPDATE semantic_receipts SET at=at-10*?1 WHERE command_id IN(SELECT command_id FROM command_audit WHERE revision<=?2)", [DAY, old]).unwrap();
    store.conn.execute("UPDATE command_audit SET at=at-10*?1 WHERE revision<=?2", [DAY, old]).unwrap();
    (store, first, delivery, now)
}
/// Runs every policy until a pass changes fewer rows than `limit`, as the daemon does.
fn drain(store: &mut DomainStore, now: i64, limit: usize) -> Vec<usize> {
    POLICIES
        .iter()
        .map(|policy| {
            let mut total = 0;
            loop {
                let pass = store.retain(policy, now, limit).unwrap();
                assert!(pass <= limit, "{} changed {pass} rows in a pass of {limit}", policy.name);
                total += pass;
                if pass < limit {
                    break total;
                }
            }
        })
        .collect()
}
fn audit(conn: &Connection) -> Vec<(i64, String)> {
    conn.prepare("SELECT revision,command FROM command_audit ORDER BY revision").unwrap()
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?))).unwrap()
        .collect::<rusqlite::Result<_>>().unwrap()
}
fn bodies(conn: &Connection) -> Vec<(i64, String)> {
    conn.prepare("SELECT revision,command FROM command_bodies ORDER BY revision").unwrap()
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?))).unwrap()
        .collect::<rusqlite::Result<_>>().unwrap()
}
fn receipts(conn: &Connection) -> Vec<String> {
    conn.prepare("SELECT command_id FROM semantic_receipts ORDER BY command_id").unwrap()
        .query_map([], |row| row.get(0)).unwrap()
        .collect::<rusqlite::Result<_>>().unwrap()
}
fn kinds(rows: &[(i64, String)]) -> Vec<String> {
    rows.iter().map(|(_, command)| serde_json::from_str::<serde_json::Value>(command).unwrap()["kind"].as_str().unwrap().to_string()).collect()
}
fn pages(conn: &Connection, pragma: &str) -> i64 {
    conn.query_row(&format!("PRAGMA {pragma}"), [], |row| row.get(0)).unwrap()
}
/// Everything read from the audit besides the command: the revisions, their times and actors, and
/// what task history, inbox, messages and model metrics make of them.
fn clock(store: &mut DomainStore, now: i64) -> String {
    let rows: Vec<(i64, String, String, i64)> = store.conn.prepare("SELECT revision,actor,command_id,at FROM command_audit ORDER BY revision").unwrap()
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))).unwrap()
        .collect::<rusqlite::Result<_>>().unwrap();
    let revisions: Vec<Revision> = rows.iter().map(|row| row.0 as Revision).collect();
    let messages: Vec<i64> = store.conn.prepare("SELECT id FROM domain_messages ORDER BY id").unwrap()
        .query_map([], |row| row.get(0)).unwrap().collect::<rusqlite::Result<_>>().unwrap();
    assert!(!messages.is_empty());
    let history: Vec<(i64, String)> = store.task_history(&"task-1".into()).unwrap().into_iter().map(|step| (step.at, step.state)).collect();
    assert!(!history.is_empty());
    format!(
        "{rows:?} {history:?} {:?} {:?} {:?} {:?} {:?}",
        store.task_state_times().unwrap(),
        store.revision_times(&revisions).unwrap(),
        messages.iter().map(|id| store.message_origin(*id).unwrap()).collect::<Vec<_>>(),
        store.write_mark().unwrap().1,
        store.model_report(None, now).unwrap(),
    )
}

#[test]
fn an_audit_row_keeps_only_the_kind_of_its_command_and_the_body_lives_beside_it() {
    let fixture = Fixture::new();
    let (store, _, _, _) = store_with_history(&fixture);
    let rows = audit(&store.conn);
    assert_eq!(rows.iter().map(|row| row.1.as_str()).collect::<Vec<_>>(),
        [r#"{"kind":"create_request"}"#, r#"{"kind":"register_agent"}"#, r#"{"kind":"create_task"}"#, r#"{"kind":"present_result"}"#]);
    let full = bodies(&store.conn);
    assert_eq!(full.iter().map(|row| row.0).collect::<Vec<_>>(), rows.iter().map(|row| row.0).collect::<Vec<_>>());
    assert_eq!(kinds(&full), kinds(&rows));
    assert!(full[3].1.contains("Finished the answer"));
}

#[test]
fn the_summary_of_an_assign_task_is_all_of_it() {
    let conn = Connection::open_in_memory().unwrap();
    let summary = |json: &str| conn.query_row(&format!("SELECT {}", summary("?1")), [json], |row| row.get::<_, String>(0)).unwrap();
    let assign = r#"{"agent_id":"agent-1","kind":"assign_task","task_id":"task-1"}"#;
    assert_eq!(summary(assign), assign);
    assert_eq!(summary(r#"{"body":"long","kind":"present_result"}"#), r#"{"kind":"present_result"}"#);
}

#[test]
fn a_command_older_than_a_week_keeps_its_revision_and_time_and_loses_its_body_and_receipt() {
    let fixture = Fixture::new();
    let (mut store, _, _, now) = store_with_history(&fixture);
    let rows = audit(&store.conn);
    let today = bodies(&store.conn)[3].clone();
    let clock_before = clock(&mut store, now);
    assert_eq!(receipts(&store.conn), ["agent", "present", "request", "task"]);

    assert_eq!(drain(&mut store, now, 1), [0, 3, 0, 0, 0, 0, 0, 0], "three week-old commands, one row per pass");

    assert_eq!(bodies(&store.conn), [today], "only today's command keeps its body");
    assert_eq!(receipts(&store.conn), ["present"]);
    assert_eq!(audit(&store.conn), rows);
    assert_eq!(clock(&mut store, now), clock_before);
    assert_eq!(drain(&mut store, now, 1), [0, 0, 0, 0, 0, 0, 0, 0], "a second run finds nothing left");
    store.integrity_check().unwrap();
    drop(store);
    let mut reopened = DomainStore::open(&fixture.path()).unwrap();
    assert_eq!(clock(&mut reopened, now), clock_before);
}

#[test]
fn a_duplicate_inside_the_week_replays_and_an_older_one_is_refused() {
    let fixture = Fixture::new();
    let (mut store, first, delivery, now) = store_with_history(&fixture);
    let actor = Actor::Orchestrator { generation: 0 };
    assert!(store.execute_authenticated(&actor, &first, None).unwrap().replayed);
    drain(&mut store, now, 256);
    let state = store.load().unwrap();

    assert!(store.execute_authenticated(&agent(), &delivery, None).unwrap().replayed);
    assert!(store.execute_authenticated(&actor, &first, None).is_err(), "a week-old duplicate never runs twice");
    assert_eq!(store.load().unwrap(), state);
    drain(&mut store, now + WINDOW_MS + 1, 256);
    assert!(store.execute_authenticated(&agent(), &delivery, None).is_err());
    assert_eq!(store.load().unwrap(), state);
}

#[test]
fn an_assign_task_keeps_its_body_so_nodes_launched_before_decisions_stay_alondras() {
    let fixture = Fixture::new();
    let (mut store, _, _, now) = store_with_history(&fixture);
    let assign = r#"{"agent_id":"agent-legacy","kind":"assign_task","task_id":"task-1"}"#;
    store.conn.execute(
        "INSERT INTO command_audit(revision,actor,command_id,data_epoch,command,at) SELECT max(revision)+1,'{\"kind\":\"orchestrator\"}','orchestrator-tool-1',data_epoch,?1,?2 FROM command_audit",
        rusqlite::params![assign, now - 10 * DAY],
    ).unwrap();
    store.conn.execute("UPDATE command_audit SET at=?1", [now - 10 * DAY]).unwrap();
    store.conn.execute("INSERT INTO retention_marks(policy,revision) SELECT 'inline_bodies_end',max(revision) FROM command_audit", []).unwrap();
    assert!(store.launched_by_alondra("agent-legacy").unwrap());

    assert_eq!(drain(&mut store, now, 256)[0], 5, "written with its body, as before bodies had their own table");

    assert_eq!(audit(&store.conn).last().unwrap().1, assign);
    assert!(store.launched_by_alondra("agent-legacy").unwrap());
}

#[test]
fn a_receipt_written_before_receipts_carried_a_time_lasts_a_week_from_the_first_run() {
    let fixture = Fixture::new();
    let (mut store, _, _, now) = store_with_history(&fixture);
    store.conn.execute(
        "INSERT INTO semantic_receipts(actor,command_id,data_epoch,digest,result) SELECT actor,'unchanged',data_epoch,zeroblob(32),'{}' FROM semantic_receipts LIMIT 1",
        [],
    ).unwrap();

    assert_eq!(drain(&mut store, now, 256), [0, 3, 1, 0, 0, 0, 0, 0]);
    assert!(receipts(&store.conn).contains(&"unchanged".to_string()));
    assert_eq!(drain(&mut store, now + WINDOW_MS, 256), [0, 0, 0, 0, 0, 0, 0, 0]);
    assert!(receipts(&store.conn).contains(&"unchanged".to_string()));
    assert_eq!(drain(&mut store, now + WINDOW_MS + 1, 256), [0, 1, 0, 1, 0, 0, 0, 0], "today's command and the dated receipt");
    assert!(!receipts(&store.conn).contains(&"unchanged".to_string()));
}

#[test]
fn the_daemon_runs_every_policy_and_gives_the_disk_every_page_it_freed() {
    let fixture = Fixture::new();
    let (store, _, _, now) = store_with_history(&fixture);
    store.conn.execute_batch("PRAGMA auto_vacuum=INCREMENTAL; VACUUM;").unwrap();
    store.conn.execute("UPDATE command_bodies SET command=json_object('kind',json_extract(command,'$.kind'),'padding',?1) WHERE revision<4", [&"x".repeat(64 * 1024)]).unwrap();
    let before = pages(&store.conn, "page_count");
    drop(store);
    let runtime = crate::domain_runtime::DomainRuntime::open_at(&fixture.0, &fixture.path()).unwrap();

    let retained = runtime.retain(now).unwrap();

    let names: Vec<_> = POLICIES.iter().map(|policy| policy.name).collect();
    assert_eq!(retained.tables, names.into_iter().zip([0, 3, 0, 0, 0, 0, 0, 0]).collect::<Vec<_>>());
    let observer = Connection::open(fixture.path()).unwrap();
    assert_eq!(pages(&observer, "freelist_count"), 0, "every freed page goes back");
    assert!(pages(&observer, "page_count") < before - 40, "{} pages, {before} before", pages(&observer, "page_count"));
}

/// The store as the previous release left it: each audit row holding its command and bound to its
/// receipt by a foreign key, receipts without a time, no table for bodies nor retention marks.
fn previous_release(fixture: &Fixture, padding: usize) {
    let (store, _, _, _) = store_with_history(fixture);
    store.conn.execute(
        "UPDATE command_audit SET command=(SELECT json_set(b.command,'$.padding',?1) FROM command_bodies b WHERE b.revision=command_audit.revision)",
        [&"x".repeat(padding)],
    ).unwrap();
    let version: i64 = store.conn.pragma_query_value(None, "schema_version", |row| row.get(0)).unwrap();
    store.conn.execute_batch("PRAGMA writable_schema=ON").unwrap();
    store.conn.execute("UPDATE sqlite_schema SET sql=?1 WHERE type='table' AND name='command_audit'", [AUDIT_BOUND_TO_RECEIPTS]).unwrap();
    store.conn.pragma_update(None, "schema_version", version + 1).unwrap();
    store.conn.execute_batch("PRAGMA writable_schema=OFF; DROP TABLE command_bodies; ALTER TABLE semantic_receipts DROP COLUMN at; DROP TABLE retention_marks; PRAGMA auto_vacuum=INCREMENTAL; VACUUM;").unwrap();
    let receipts: String = store.conn.query_row("SELECT sql FROM sqlite_schema WHERE name='semantic_receipts'", [], |row| row.get(0)).unwrap();
    assert_eq!(receipts, RECEIPTS_BOUND_TO_AUDIT);
    let refused = store.conn.execute_batch("BEGIN; DELETE FROM semantic_receipts; COMMIT;");
    assert!(refused.is_err(), "the previous schema refuses removing a receipt its audit row names");
    store.conn.execute_batch("ROLLBACK").unwrap();
}
fn schema(conn: &Connection) -> Vec<(String, String, Option<String>)> {
    conn.prepare("SELECT type,name,sql FROM sqlite_schema ORDER BY type,name").unwrap()
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?))).unwrap()
        .collect::<rusqlite::Result<_>>().unwrap()
}

#[test]
fn a_store_of_the_previous_release_opens_into_the_schema_of_a_new_one_with_its_rows() {
    let fixture = Fixture::new();
    previous_release(&fixture, 0);
    let old = Connection::open(fixture.path()).unwrap();
    let rows = (audit(&old), receipts(&old));
    drop(old);
    let fresh = Fixture::new();
    drop(DomainStore::open(&fresh.path()).unwrap());

    let mut store = DomainStore::open(&fixture.path()).unwrap();

    assert_eq!(schema(&store.conn), schema(&Connection::open(fresh.path()).unwrap()));
    assert_eq!((audit(&store.conn), receipts(&store.conn)), rows);
    let now = store.conn.query_row("SELECT max(at) FROM command_audit", [], |row| row.get::<_, i64>(0)).unwrap();
    assert_eq!(drain(&mut store, now, 256), [4, 3, 1, 0, 0, 0, 0, 0], "every inline body, three week-old commands, today's undated receipt");
    assert_eq!(kinds(&audit(&store.conn)), kinds(&rows.0));
    assert!(audit(&store.conn).iter().all(|row| row.1.len() < 40), "{:?}", audit(&store.conn));
    assert_eq!(bodies(&store.conn), [rows.0[3].clone()], "today's body moved beside its audit row");
    assert_eq!(receipts(&store.conn), ["present"]);
    store.integrity_check().unwrap();
    drop(store);
    DomainStore::open(&fixture.path()).unwrap();
}

#[test]
fn a_store_whose_audit_rows_held_their_bodies_is_rebuilt_once_they_all_moved() {
    let fixture = Fixture::new();
    previous_release(&fixture, 3000);
    let runtime = crate::domain_runtime::DomainRuntime::open_at(&fixture.0, &fixture.path()).unwrap();
    let now = runtime.store.lock().unwrap().conn.query_row("SELECT max(at) FROM command_audit", [], |row| row.get::<_, i64>(0)).unwrap();

    runtime.retain(now).unwrap();

    let observer = Connection::open(fixture.path()).unwrap();
    let compact = fixture.0.join("compact.sqlite");
    observer.execute("VACUUM INTO ?1", [compact.to_str().unwrap()]).unwrap();
    let packed = pages(&Connection::open(&compact).unwrap(), "page_count");
    assert_eq!(pages(&observer, "page_count"), packed, "the shrunk audit rows leave no half-empty pages");
    let marks: Vec<String> = observer.prepare("SELECT policy FROM retention_marks ORDER BY policy").unwrap()
        .query_map([], |row| row.get(0)).unwrap().collect::<rusqlite::Result<_>>().unwrap();
    assert_eq!(marks, ["ended_tasks_compacted", "week_old_commands"], "the rebuild is owed no more");
}

#[test]
fn an_audit_table_of_unknown_shape_is_refused() {
    let fixture = Fixture::new();
    let store = DomainStore::open(&fixture.path()).unwrap();
    let version: i64 = store.conn.pragma_query_value(None, "schema_version", |row| row.get(0)).unwrap();
    store.conn.execute_batch("PRAGMA writable_schema=ON").unwrap();
    store.conn.execute("UPDATE sqlite_schema SET sql=replace(sql,'at INTEGER NOT NULL','at INTEGER') WHERE type='table' AND name='command_audit'", []).unwrap();
    store.conn.pragma_update(None, "schema_version", version + 1).unwrap();
    store.conn.execute_batch("PRAGMA writable_schema=OFF").unwrap();
    drop(store);
    let refused = DomainStore::open(&fixture.path()).err().map(|error| format!("{error:#}"));
    assert!(refused.as_deref().is_some_and(|error| error.contains("unexpected command_audit definition")), "{refused:?}");
}

fn long(head: &str) -> String {
    format!("{head}\n\n{}", "What the node measured, line after line of it. ".repeat(300))
}
/// A done task whose question, notices, final result and messages hold long texts.
fn ended_task(fixture: &Fixture) -> DomainStore {
    let mut store = DomainStore::open(&fixture.path()).unwrap();
    seeded(&mut store);
    let user = Actor::User { user_id: "user-1".into() };
    let ask = Command::AskQuestion {
        attachments: vec![], interaction_id: "ask-1".into(), scope: Scope::Task { task_id: "task-1".into() }, recipient: Recipient::User,
        body: long("Which table goes first?"), blocking: true,
        options: vec![
            QuestionOption { id: "cards".into(), label: "Cards".into(), description: Some(long("They weigh most")) },
            QuestionOption { id: "inputs".into(), label: "Inputs".into(), description: None },
        ],
        card: CardSpec::new(CardKind::Decide, "Choose the table to compact first").why("The cards weigh most").content(ContentKind::Options, long("Measured")),
    };
    let answer = Command::AnswerQuestion { attachments: vec![], interaction_id: "ask-1".into(), option_id: "cards".into(), inputs: Default::default() };
    let targets = vec![DeliveryTarget::Text { text: "The answer".into() }];
    let mut card = CardSpec::delivery("Review the result", &targets, true);
    card.content = Some(CardContent { kind: ContentKind::Report, summary: None, body: long("# Report") });
    let present = Command::PresentResult {
        checklist: vec![], task_id: "task-1".into(), interaction_id: "delivery-1".into(), body: long("Finished the answer"),
        targets: targets.clone(), is_final: true, nothing_to_review: false, attachments: vec![], card,
    };
    let accept = Command::AcceptDelivery { presentation_id: "delivery-1".into(), targets };
    let end = Command::RecordNodeEnd { task_id: "task-1".into(), end: NodeEnd::Closed, agent_id: None };
    let mut steps = vec![("ask".to_owned(), agent(), ask), ("answer".into(), user.clone(), answer)];
    for n in 0..60 {
        steps.push((format!("notice-{n}"), Actor::Orchestrator { generation: 0 }, Command::PublishInformation {
            attachments: vec![], interaction_id: format!("notice-{n}").into(), scope: Scope::Task { task_id: "task-1".into() }, recipient: Recipient::User,
            body: format!("Notice {n}\n\n{}", "A few lines the node measured. ".repeat(40)), card: CardSpec::new(CardKind::Know, format!("Read notice {n}")),
        }));
        steps.push((format!("read-{n}"), user.clone(), Command::MarkRead { interaction_id: format!("notice-{n}").into() }));
    }
    steps.extend([("present".into(), agent(), present), ("accept".into(), user, accept), ("end".into(), Actor::System, end)]);
    for (id, actor, command) in steps {
        let e = envelope(&mut store, &id, command);
        store.execute_authenticated(&actor, &e, None).unwrap();
    }
    assert_eq!(store.load().unwrap().tasks[&TaskId::from("task-1")].state, TaskState::Done);
    store
}
/// Runs `test` on a daemon whose store holds a done task with cards to compact.
pub(crate) fn with_an_ended_task(test: impl FnOnce(&crate::domain_runtime::DomainRuntime)) {
    let fixture = Fixture::new();
    drop(ended_task(&fixture));
    let runtime = crate::domain_runtime::DomainRuntime::open_at(&fixture.0, &fixture.path()).unwrap();
    test(&runtime);
}
/// What the apps show: the task's detail, its cards, the home and the task list, on every client.
fn shown(state: &DomainState) -> Vec<serde_json::Value> {
    use alondra_core::views::{Client, Lang, LiveSnapshot, RenderContext, Transport};
    let index = ReadIndex::build(state);
    let live = LiveSnapshot::default();
    let mut keys = vec!["task:task-1".to_owned(), "home".into(), "tasks".into()];
    keys.extend(state.interactions.keys().map(|id| format!("card:{}", id.0)));
    let mut shown = vec![];
    for key in &keys {
        for client in [Client::Ios, Client::Mac, Client::Tui] {
            let view = alondra_core::views::build(key, RenderContext {
                domain: state, index: &index, live: &live, client, lang: Lang::Es, transport: Transport::Loopback, cursor: "", rendered_at: "",
            });
            let Ok(view) = view else { continue };
            let mut view = serde_json::to_value(view).unwrap();
            for field in ["version", "status"] {
                view.as_object_mut().unwrap().remove(field);
            }
            shown.push(serde_json::json!({ "key": key, "view": view }));
        }
    }
    shown
}

#[test]
fn the_daemon_compacts_an_ended_tasks_cards_and_messages_and_the_apps_show_the_same() {
    let fixture = Fixture::new();
    let store = ended_task(&fixture);
    store.conn.execute("UPDATE domain_outbox SET state='handed'", []).unwrap();
    store.conn.execute_batch("PRAGMA auto_vacuum=INCREMENTAL; VACUUM;").unwrap();
    let messages: i64 = store.conn.query_row("SELECT count(*) FROM domain_messages WHERE task_id='task-1' AND message<>''", [], |row| row.get(0)).unwrap();
    assert!(messages > 0);
    drop(store);
    let runtime = crate::domain_runtime::DomainRuntime::open_at(&fixture.0, &fixture.path()).unwrap();
    let before = runtime.state().unwrap();
    let now = alondra_core::now_millis();

    let retained = runtime.retain(now).unwrap();

    assert_eq!(retained.compacted, 1);
    assert!(retained.tables.contains(&("messages of ended tasks", messages as usize)), "{:?}", retained.tables);
    let after = runtime.state().unwrap();
    assert_eq!(after.interactions[&InteractionId::from("ask-1")].body, "Which table goes first?");
    assert_eq!(result_excerpt(&after.interactions[&InteractionId::from("delivery-1")].body), result_excerpt(&before.interactions[&InteractionId::from("delivery-1")].body));
    let renumbered = serde_json::to_string(&shown(&before)).unwrap().replace(&format!("@{}", before.revision), &format!("@{}", after.revision));
    assert_eq!(serde_json::to_string(&shown(&after)).unwrap(), renumbered, "the apps show the same, buttons aside, which carry the revision");
    let observer = Connection::open(fixture.path()).unwrap();
    let left: i64 = observer.query_row("SELECT count(*) FROM domain_messages WHERE message<>''", [], |row| row.get(0)).unwrap();
    assert_eq!(left, 0, "a handed message of an ended task keeps no text");
    let marks: Vec<String> = observer.prepare("SELECT policy FROM retention_marks ORDER BY policy").unwrap()
        .query_map([], |row| row.get(0)).unwrap().collect::<rusqlite::Result<_>>().unwrap();
    assert_eq!(marks, ["ended_tasks_compacted"], "the first full compaction rebuilt the store once");
    let compact = fixture.0.join("compact.sqlite");
    observer.execute("VACUUM INTO ?1", [compact.to_str().unwrap()]).unwrap();
    assert_eq!(pages(&observer, "page_count"), pages(&Connection::open(&compact).unwrap(), "page_count"), "the shrunk cards leave no half-empty pages");
    drop(observer);
    let again = runtime.retain(now).unwrap();
    assert_eq!(again.compacted, 0);
    drop(runtime);
    let mut reopened = DomainStore::open(&fixture.path()).unwrap();
    assert_eq!(*reopened.load().unwrap(), *after, "what the daemon holds is what the store keeps");
    let reopen = CommandEnvelope { expected_revision: before.revision, ..envelope(&mut reopened, "reopen", Command::ReopenTask { task_id: "task-1".into() }) };
    reopened.execute_authenticated(&Actor::User { user_id: "user-1".into() }, &reopen, None).expect("a button shown before the compaction still works");
}

/// Runs the whole retention on a copy of a real domain database at `ALONDRA_RETENTION_DB`, as of
/// `ALONDRA_RETENTION_DAYS` days from now, and prints how long it took.
#[test]
#[ignore = "measures a copy of a real HOME: ALONDRA_RETENTION_DB=… bazelisk test … --test_arg=--ignored"]
fn measure_retention() {
    let path = std::path::PathBuf::from(std::env::var("ALONDRA_RETENTION_DB").expect("ALONDRA_RETENTION_DB"));
    let days: i64 = std::env::var("ALONDRA_RETENTION_DAYS").map_or(0, |days| days.parse().unwrap());
    let started = std::time::Instant::now();
    let runtime = crate::domain_runtime::DomainRuntime::open_at(path.parent().unwrap(), &path).unwrap();
    eprintln!("open: {:.0} ms", started.elapsed().as_secs_f64() * 1000.0);
    let state = runtime.state().unwrap();
    let batch = state.compactable_tasks(32);
    if !batch.is_empty() {
        let envelope = CommandEnvelope {
            protocol_version: PROTOCOL_VERSION, data_epoch: state.data_epoch.clone(), command_id: "measure-compaction".into(),
            expected_revision: state.revision, command: Command::CompactEndedTasks { task_ids: batch.clone() },
        };
        let started = std::time::Instant::now();
        runtime.execute_as_system(envelope).unwrap();
        eprintln!("one batch of {} ended tasks under the writer lock: {:.0} ms", batch.len(), started.elapsed().as_secs_f64() * 1000.0);
    }
    let started = std::time::Instant::now();
    let retained = runtime.retain(alondra_core::now_millis() + days * DAY).unwrap();
    eprintln!("retention: {:.0} ms; {} ended tasks compacted; {:?}; {:?}", started.elapsed().as_secs_f64() * 1000.0, retained.compacted, retained.evidence, retained.tables);
}
