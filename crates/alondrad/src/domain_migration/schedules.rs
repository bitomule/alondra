//! Read-only inventory of the legacy schedule store. The two source databases
//! have no cross-database transaction: callers use an offline locked backup.
//! This module prepares data for import; it does not enable v2 scheduling.
use super::canonical_leaf;
use alondra_core::domain_v2 as d;
use alondra_core::View;
use anyhow::{ensure, Context, Result};
use rusqlite::{Connection, OpenFlags};
use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, OpenOptions};
use std::io::Read;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::path::Path;

#[derive(Debug, PartialEq, Eq)]
pub(crate) struct LegacyScheduleReceiptRow {
    pub sender: String,
    pub request_id: String,
    pub digest: Vec<u8>,
    /// Original JSON text, not parsed and serialized again during import.
    pub result: String,
}

#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct LegacyScheduleInventory {
    pub state: d::ScheduleState,
    pub request_tools: BTreeMap<d::RequestId, Vec<String>>,
    pub legacy_schedule_receipts: Vec<LegacyScheduleReceiptRow>,
    pub failed_launches: BTreeSet<d::RequestId>,
}

#[cfg(test)]
impl LegacyScheduleInventory {
    pub fn counts(&self) -> (usize, usize, usize, usize, usize) {
        (
            self.state.schedules.len(),
            self.state.runs.len(),
            self.request_tools.len(),
            self.legacy_schedule_receipts.len(),
            self.failed_launches.len(),
        )
    }
}

fn frequency(value: crate::schedules::Frequency) -> d::ScheduleFrequency {
    match value {
        crate::schedules::Frequency::Daily => d::ScheduleFrequency::Daily,
        crate::schedules::Frequency::Weekly => d::ScheduleFrequency::Weekly,
        crate::schedules::Frequency::EveryHours => d::ScheduleFrequency::EveryHours,
        crate::schedules::Frequency::EveryDays => d::ScheduleFrequency::EveryDays,
        crate::schedules::Frequency::EveryMonths => d::ScheduleFrequency::EveryMonths,
        crate::schedules::Frequency::Once => d::ScheduleFrequency::Once,
    }
}

fn original_request_exists(view: &View, imported: &d::DomainState, request: &d::RequestId) -> bool {
    view.board
        .requests
        .get(&request.0)
        .is_some_and(|original| original.id == request.0)
        && imported.requests.contains_key(request)
}

/// The run links the legacy Task the schedule launched, under its original ID.
/// Tasks the importer derives for extra PRs share its Request but were never
/// launched by the schedule, so they do not make the link ambiguous.
fn original_task_for_request(
    view: &View,
    state: &d::DomainState,
    request: &d::RequestId,
) -> Result<d::TaskId> {
    ensure!(
        original_request_exists(view, state, request),
        "schedule links a missing original Request"
    );
    let mut originals = view
        .board
        .tasks
        .iter()
        .filter(|(_, task)| task.request.as_str() == request.0.as_str());
    let (original_key, original) = originals
        .next()
        .context("schedule Request has no original Task")?;
    ensure!(
        originals.next().is_none(),
        "schedule Request has ambiguous original Tasks"
    );
    ensure!(
        original_key == &original.id,
        "original schedule Task key and ID differ"
    );
    let expected_id = d::TaskId(original.id.clone());
    let task = state
        .tasks
        .get(&expected_id)
        .context("schedule Task was not imported under its original ID")?;
    ensure!(
        &task.request_id == request,
        "imported schedule Task belongs to another Request"
    );
    Ok(task.id.clone())
}

/// What the import of work in flight leaves out of the schedule store: a
/// sent run whose Task finished (v2 needs its Request and Task), and links,
/// tool policies and failed launches of finished Requests.
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct Dropped {
    pub runs: usize,
    pub links: usize,
}

/// The imported original Task a schedule run launched, or None when its
/// Request or Task finished before the cutover and was not imported.
fn imported_original_task(view: &View, imported: &d::DomainState, request: &d::RequestId) -> Result<Option<d::TaskId>> {
    if !original_request_exists(view, imported, request) {
        return Ok(None);
    }
    let originals: Vec<_> = view.board.tasks.values().filter(|task| task.request.as_str() == request.0.as_str()).collect();
    match originals.as_slice() {
        [] => Ok(None),
        [task] if !imported.tasks.contains_key(&d::TaskId(task.id.clone())) => Ok(None),
        _ => original_task_for_request(view, imported, request).map(Some),
    }
}

fn continue_run(result: &mut LegacyScheduleInventory, id: &d::ScheduleId, schedule: d::Schedule) -> Result<()> {
    ensure!(result.state.schedules.insert(id.clone(), schedule).is_none(), "duplicate legacy Schedule");
    Ok(())
}

fn ensure_no_sqlite_sidecars(home: &Path) -> Result<()> {
    for suffix in [
        "schedules.db-wal",
        "schedules.db-shm",
        "schedules.db-journal",
    ] {
        match fs::symlink_metadata(home.join(suffix)) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => (),
            Ok(_) => {
                anyhow::bail!("schedules.db inventory requires a backup without SQLite sidecars")
            }
            Err(error) => return Err(error.into()),
        }
    }
    Ok(())
}

/// Read one coherent SQLite snapshot without calling Schedules::open, which
/// rewrites launching/waiting state. No mission, ID, pattern or receipt is logged.
#[cfg(test)]
pub(crate) fn read_legacy_schedules_readonly(
    home: &Path,
    view: &View,
    imported: &d::DomainState,
) -> Result<LegacyScheduleInventory> {
    read_legacy_schedules_counted(home, view, imported).map(|(inventory, _)| inventory)
}

/// Every schedule and its last run are imported. A run link, tool policy or
/// failed launch whose Request finished before the cutover is not imported
/// and counted instead; a link to imported work must still be unambiguous.
pub(crate) fn read_legacy_schedules_counted(
    home: &Path,
    view: &View,
    imported: &d::DomainState,
) -> Result<(LegacyScheduleInventory, Dropped)> {
    let mut dropped = Dropped::default();
    let path = home.join("schedules.db");
    let metadata = match fs::symlink_metadata(&path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            ensure_no_sqlite_sidecars(home)?;
            return Ok((LegacyScheduleInventory::default(), Dropped::default()));
        }
        Err(error) => return Err(error.into()),
    };
    ensure!(
        metadata.is_file() && metadata.nlink() == 1 && metadata.uid() == unsafe { libc::geteuid() },
        "unsafe schedules.db path"
    );
    let path = canonical_leaf(&path)?;
    // A read-only SQLite connection to a WAL database can still create -wal or
    // -shm files. The offline backup uses rollback/DELETE mode; require that
    // format before SQLite sees the path.
    let mut source = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(&path)?;
    let opened = source.metadata()?;
    ensure!(
        opened.dev() == metadata.dev() && opened.ino() == metadata.ino(),
        "schedules.db changed before inventory"
    );
    let mut header = [0u8; 20];
    source.read_exact(&mut header)?;
    ensure!(
        &header[..16] == b"SQLite format 3\0",
        "invalid schedules.db header"
    );
    ensure!(
        header[18] == 1 && header[19] == 1,
        "schedules.db inventory requires an offline rollback-journal backup"
    );
    ensure_no_sqlite_sidecars(home)?;
    drop(source);
    let database = Connection::open_with_flags(
        &path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NOFOLLOW,
    )?;
    database.execute_batch("PRAGMA query_only=ON; BEGIN")?;
    let integrity: String = database.query_row("PRAGMA integrity_check", [], |row| row.get(0))?;
    ensure!(integrity == "ok", "schedules.db integrity check failed");
    let mut table_query = database.prepare(
        "SELECT name FROM sqlite_schema WHERE type='table' AND name NOT LIKE 'sqlite_%' ORDER BY name"
    )?;
    let tables = table_query
        .query_map([], |row| row.get::<_, String>(0))?
        .collect::<rusqlite::Result<BTreeSet<_>>>()?;
    drop(table_query);
    for table in &tables {
        ensure!(
            [
                "schedules",
                "local_command_results",
                "schedule_failed_launches",
                "schedule_launch_tools"
            ]
            .contains(&table.as_str()),
            "schedules.db contains an unknown table"
        );
    }
    ensure!(
        tables.contains("schedules"),
        "schedules.db has no schedules table"
    );

    let mut result = LegacyScheduleInventory::default();
    if tables.contains("schedules") {
        let mut query = database.prepare("SELECT id,body FROM schedules ORDER BY id")?;
        let mut rows = query.query([])?;
        while let Some(row) = rows.next()? {
            let row_id: String = row.get(0)?;
            let body: String = row.get(1)?;
            let old: crate::schedules::Schedule =
                serde_json::from_str(&body).context("invalid legacy Schedule JSON")?;
            ensure!(row_id == old.id, "legacy Schedule key and body ID differ");
            let schedule_id = d::ScheduleId(old.id.clone());
            let schedule = d::Schedule {
                id: schedule_id.clone(),
                title: old.title,
                text: old.text,
                project: old.project,
                options: old.options,
                disallowed_tools: old.disallowed_tools,
                frequency: frequency(old.frequency),
                hour: old.hour,
                minute: old.minute,
                weekday: old.weekday,
                interval: old.interval,
                start_at: old.start_at,
                enabled: old.enabled,
                next_run: old.next_run,
                revision: old.revision,
                deleted_at: None,
            };
            schedule.validate().map_err(anyhow::Error::new)?;
            let observed = match old.status.as_str() {
                "never_run" => {
                    ensure!(
                        old.last_run.is_none()
                            && old.last_request.is_none()
                            && old.last_error.is_none(),
                        "legacy Schedule claims never_run with prior result"
                    );
                    None
                }
                "sent" | "failed" => {
                    let finished_at = old
                        .last_run
                        .context("legacy Schedule result lacks last_run")?;
                    let (request_id, task_id) = match old.last_request.map(d::RequestId) {
                        Some(request_id) => match imported_original_task(view, imported, &request_id)? {
                            Some(task_id) => (Some(request_id), Some(task_id)),
                            None if old.status == "sent" => {
                                dropped.runs += 1;
                                continue_run(&mut result, &schedule_id, schedule)?;
                                continue;
                            }
                            None => {
                                dropped.links += 1;
                                (None, None)
                            }
                        },
                        None => (None, None),
                    };
                    let (state, error) = if old.status == "sent" {
                        ensure!(old.last_error.is_none(), "sent Schedule retains an error");
                        (d::ScheduleRunState::Sent, None)
                    } else {
                        (d::ScheduleRunState::Failed, old.last_error)
                    };
                    let run = d::ScheduleRun {
                        id: d::ScheduleRunId(format!("legacy-last-{}", schedule_id.0)),
                        schedule_id: schedule_id.clone(),
                        origin: d::ScheduleRunOrigin::LegacyLastObserved,
                        due_at: None,
                        claimed_at: None,
                        claim_revision: None,
                        finished_at: Some(finished_at),
                        state,
                        request_id,
                        task_id,
                        error,
                        definition_snapshot: None,
                        launch: None,
                    };
                    run.validate().map_err(anyhow::Error::new)?;
                    Some(run)
                }
                "launching" | "interrupted" | "waiting" => {
                    anyhow::bail!("legacy Schedule has an unreconciled run state")
                }
                _ => anyhow::bail!("legacy Schedule has an unknown run state"),
            };
            ensure!(
                result
                    .state
                    .schedules
                    .insert(schedule_id, schedule)
                    .is_none(),
                "duplicate legacy Schedule"
            );
            if let Some(run) = observed {
                ensure!(
                    result.state.runs.insert(run.id.clone(), run).is_none(),
                    "duplicate legacy ScheduleRun"
                );
            }
        }
    }

    if tables.contains("schedule_launch_tools") {
        let mut query = database
            .prepare("SELECT request,patterns FROM schedule_launch_tools ORDER BY request")?;
        let mut rows = query.query([])?;
        while let Some(row) = rows.next()? {
            let request = d::RequestId(row.get::<_, String>(0)?);
            if !original_request_exists(view, imported, &request) {
                dropped.links += 1;
                continue;
            }
            let patterns: Vec<String> = serde_json::from_str(&row.get::<_, String>(1)?)
                .context("invalid schedule tool policy")?;
            ensure!(
                result.request_tools.insert(request, patterns).is_none(),
                "duplicate schedule tool policy"
            );
        }
    }

    if tables.contains("schedule_failed_launches") {
        let mut query =
            database.prepare("SELECT request FROM schedule_failed_launches ORDER BY request")?;
        let mut rows = query.query([])?;
        while let Some(row) = rows.next()? {
            let request = d::RequestId(row.get::<_, String>(0)?);
            if !original_request_exists(view, imported, &request) {
                dropped.links += 1;
                continue;
            }
            ensure!(
                result.failed_launches.insert(request),
                "duplicate failed schedule launch"
            );
        }
    }

    if tables.contains("local_command_results") {
        let mut query = database.prepare(
            "SELECT sender,request_id,digest,result FROM local_command_results ORDER BY sender,request_id"
        )?;
        let mut rows = query.query([])?;
        while let Some(row) = rows.next()? {
            let receipt = LegacyScheduleReceiptRow {
                sender: row.get(0)?,
                request_id: row.get(1)?,
                digest: row.get(2)?,
                result: row.get(3)?,
            };
            ensure!(
                receipt.digest.len() == 32,
                "legacy schedule receipt digest has invalid length"
            );
            serde_json::from_str::<serde_json::Value>(&receipt.result)
                .context("invalid legacy schedule receipt result")?;
            result.legacy_schedule_receipts.push(receipt);
        }
    }
    database.execute_batch("COMMIT")?;
    result.state.validate().map_err(anyhow::Error::new)?;
    Ok((result, dropped))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn entries(path: &Path) -> BTreeSet<String> {
        fs::read_dir(path)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().into_string().unwrap())
            .collect()
    }

    struct Fixture {
        root: std::path::PathBuf,
        home: std::path::PathBuf,
        database: Option<Connection>,
        view: View,
        imported: d::DomainState,
    }

    impl Fixture {
        fn db(&self) -> &Connection {
            self.database.as_ref().unwrap()
        }

        fn new() -> Self {
            let root = std::env::temp_dir().join(format!(
                "alondra-schedule-import-{}",
                crate::auth::random_hex(8)
            ));
            let home = root.join("source");
            fs::create_dir(&root).unwrap();
            fs::create_dir(&home).unwrap();
            let database = Connection::open(home.join("schedules.db")).unwrap();
            database.execute_batch(
                "CREATE TABLE schedules(id TEXT PRIMARY KEY,body TEXT NOT NULL);
                 CREATE TABLE local_command_results(sender TEXT NOT NULL,request_id TEXT NOT NULL,
                   digest BLOB NOT NULL,result TEXT NOT NULL,PRIMARY KEY(sender,request_id));
                 CREATE TABLE schedule_failed_launches(request TEXT PRIMARY KEY);
                 CREATE TABLE schedule_launch_tools(request TEXT PRIMARY KEY,patterns TEXT NOT NULL);"
            ).unwrap();
            let mut view = View::default();
            let mut imported = d::DomainState::new(d::DataEpoch("synthetic-epoch".into()));
            // request-5 represents a historical launch with no current Schedule link.
            for index in 0..6 {
                let request = format!("request-{index}");
                let task = format!("task-{index}");
                view.board.requests.insert(
                    request.clone(),
                    serde_json::from_value(json!({
                        "id":request,"text":"Synthetic mission","created_at":1
                    }))
                    .unwrap(),
                );
                view.board.tasks.insert(
                    task.clone(),
                    serde_json::from_value(json!({
                        "id":task,"request":request,"title":"Synthetic scheduled task",
                        "state":"not_started","state_since":1,"created_at":1,"recorded":true
                    }))
                    .unwrap(),
                );
                imported.requests.insert(
                    request.clone().into(),
                    d::Request::new(request.clone().into(), "Synthetic mission".into()),
                );
                imported.tasks.insert(
                    task.clone().into(),
                    d::Task {
                        id: task.into(),
                        request_id: request.into(),
                        agent_id: None,
                        title: "Synthetic scheduled task".into(),
                        state: d::TaskState::NotStarted,
                        revision: 1,
                        started: false,
                        review_prs: false,
                        final_presentation: None,
                        pr_id: None,
                        dependencies: Default::default(),
                        stack: None,
                        documents: Default::default(),
                        archived: false,
                    },
                );
            }
            for index in 0..11 {
                let sent = index < 5;
                let enabled = index != 10;
                let id = format!("schedule-{index}");
                let request = sent.then(|| format!("request-{index}"));
                let body = json!({
                    "id":id,"title":"Synthetic schedule","text":"Synthetic mission",
                    "project":null,"options":{},"disallowed_tools":["Bash"],
                    "frequency":"daily","hour":9,"minute":30,"weekday":null,
                    "interval":null,"start_at":null,"enabled":enabled,
                    "next_run":if enabled { Some(2000 + index) } else { None },
                    "last_run":if sent { Some(1000 + index) } else { None },
                    "last_request":request,"last_error":null,
                    "status":if sent { "sent" } else { "never_run" },
                    "revision":7 + index
                });
                database
                    .execute(
                        "INSERT INTO schedules(id,body) VALUES(?1,?2)",
                        rusqlite::params![id, body.to_string()],
                    )
                    .unwrap();
                if sent {
                    database.execute("INSERT INTO schedule_launch_tools(request,patterns) VALUES(?1,'[\"Bash\"]')",
                        [format!("request-{index}")]).unwrap();
                }
            }
            Self {
                root,
                home,
                database: Some(database),
                view,
                imported,
            }
        }

        fn read(&self) -> Result<LegacyScheduleInventory> {
            read_legacy_schedules_readonly(&self.home, &self.view, &self.imported)
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            drop(self.database.take());
            fs::remove_dir_all(&self.root).unwrap();
        }
    }

    #[test]
    fn eleven_definitions_and_five_observed_runs_preserve_ids_revisions_and_policy() {
        let fixture = Fixture::new();
        let original_receipt = r#"{"status": 201, "body": {"id": "schedule-0"}}"#;
        fixture.db().execute(
            "INSERT INTO local_command_results(sender,request_id,digest,result) VALUES('phone','create',?1,?2)",
            rusqlite::params![vec![7u8;32], original_receipt],
        ).unwrap();
        let before_read = fs::read(fixture.home.join("schedules.db")).unwrap();
        let before_entries = entries(&fixture.home);
        let inventory = fixture.read().unwrap();
        assert_eq!(inventory.counts(), (11, 5, 5, 1, 0));
        for index in 0..11 {
            let schedule = &inventory.state.schedules[&d::ScheduleId(format!("schedule-{index}"))];
            assert_eq!(schedule.revision, 7 + index);
            assert_eq!(schedule.next_run, (index != 10).then_some(2000 + index));
            assert_eq!(schedule.enabled, index != 10);
            if index < 5 {
                let run = &inventory.state.runs
                    [&d::ScheduleRunId(format!("legacy-last-schedule-{index}"))];
                assert_eq!(
                    run.request_id,
                    Some(d::RequestId(format!("request-{index}")))
                );
                assert_eq!(run.task_id, Some(d::TaskId(format!("task-{index}"))));
                assert_eq!(run.finished_at, Some(1000 + index));
                assert_eq!(run.origin, d::ScheduleRunOrigin::LegacyLastObserved);
                assert!(run.claimed_at.is_none() && run.definition_snapshot.is_none());
                assert_eq!(
                    inventory.request_tools[&d::RequestId(format!("request-{index}"))],
                    vec!["Bash"]
                );
            }
        }
        assert_eq!(
            inventory.legacy_schedule_receipts[0].result,
            original_receipt
        );
        assert_eq!(inventory.legacy_schedule_receipts[0].digest, vec![7u8; 32]);
        assert_eq!(
            fs::read(fixture.home.join("schedules.db")).unwrap(),
            before_read,
            "read-only inventory must not rewrite SQLite or interrupted state"
        );
        assert_eq!(
            entries(&fixture.home),
            before_entries,
            "inventory created a SQLite sidecar"
        );
    }

    #[test]
    fn finished_links_are_dropped_but_ambiguous_and_uncertain_ones_fail_without_rewriting_source() {
        let mut fixture = Fixture::new();
        let run = d::ScheduleRunId("legacy-last-schedule-0".into());
        fixture.db().execute("UPDATE schedules SET body=json_set(body,'$.last_request','missing') WHERE id='schedule-0'", []).unwrap();
        let (inventory, dropped) =
            read_legacy_schedules_counted(&fixture.home, &fixture.view, &fixture.imported).unwrap();
        assert_eq!(dropped, Dropped { runs: 1, links: 0 }, "a sent run needs its Task, so it stays behind");
        assert!(!inventory.state.runs.contains_key(&run));
        assert!(inventory.state.schedules.contains_key(&d::ScheduleId("schedule-0".into())), "the schedule itself is kept");
        fixture.db().execute("UPDATE schedules SET body=json_set(body,'$.last_request','request-0') WHERE id='schedule-0'", []).unwrap();

        let original = fixture.view.board.requests.remove("request-0").unwrap();
        assert!(
            !fixture.read().unwrap().state.runs.contains_key(&run),
            "a synthesized imported Request cannot authorize the schedule link"
        );
        fixture
            .view
            .board
            .requests
            .insert("request-0".into(), original);

        fixture.view.board.tasks.insert(
            "extra-task".into(),
            serde_json::from_value(json!({
                "id":"extra-task","request":"request-0","title":"Another task",
                "state":"not_started","state_since":1,"created_at":1,"recorded":true
            }))
            .unwrap(),
        );
        assert!(fixture
            .read()
            .unwrap_err()
            .to_string()
            .contains("ambiguous original Tasks"));
        fixture.view.board.tasks.remove("extra-task");

        fixture.db().execute("UPDATE schedules SET body=json_set(body,'$.status','launching') WHERE id='schedule-0'", []).unwrap();
        let before = fs::read(fixture.home.join("schedules.db")).unwrap();
        assert!(fixture
            .read()
            .unwrap_err()
            .to_string()
            .contains("unreconciled run state"));
        assert_eq!(fs::read(fixture.home.join("schedules.db")).unwrap(), before);
        fixture
            .db()
            .execute(
                "UPDATE schedules SET body=json_set(body,'$.status','sent') WHERE id='schedule-0'",
                [],
            )
            .unwrap();

        fixture
            .db()
            .execute(
                "INSERT INTO schedule_launch_tools(request,patterns) VALUES('orphan','[]')",
                [],
            )
            .unwrap();
        let (inventory, dropped) =
            read_legacy_schedules_counted(&fixture.home, &fixture.view, &fixture.imported).unwrap();
        assert_eq!(dropped, Dropped { runs: 0, links: 1 });
        assert!(!inventory.request_tools.contains_key(&d::RequestId("orphan".into())));
    }

    #[test]
    fn failed_without_request_is_observed_but_historical_markers_do_not_invent_runs() {
        let fixture = Fixture::new();
        fixture.db().execute("UPDATE schedules SET body=json_set(body,
            '$.status','failed','$.last_run',1500,'$.last_error','project missing') WHERE id='schedule-5'", []).unwrap();
        fixture
            .db()
            .execute(
                "INSERT INTO schedule_failed_launches(request) VALUES('request-5')",
                [],
            )
            .unwrap();
        let inventory = fixture.read().unwrap();
        assert_eq!(inventory.counts(), (11, 6, 5, 0, 1));
        let run = &inventory.state.runs[&d::ScheduleRunId("legacy-last-schedule-5".into())];
        assert_eq!(run.state, d::ScheduleRunState::Failed);
        assert!(run.request_id.is_none() && run.task_id.is_none());
        assert_eq!(run.error.as_deref(), Some("project missing"));
        assert!(inventory
            .failed_launches
            .contains(&d::RequestId("request-5".into())));
        assert_eq!(
            inventory.state.runs.len(),
            6,
            "a marker is not another observed run"
        );
    }

    #[test]
    fn missing_database_is_empty_but_invalid_schema_is_rejected() {
        let mut fixture = Fixture::new();
        drop(fixture.database.take());
        fs::remove_file(fixture.home.join("schedules.db")).unwrap();
        assert_eq!(fixture.read().unwrap().counts(), (0, 0, 0, 0, 0));
        fs::write(fixture.home.join("schedules.db-wal"), b"orphan").unwrap();
        assert!(fixture
            .read()
            .unwrap_err()
            .to_string()
            .contains("without SQLite sidecars"));
        fs::remove_file(fixture.home.join("schedules.db-wal")).unwrap();
        let invalid = Connection::open(fixture.home.join("schedules.db")).unwrap();
        invalid.pragma_update(None, "user_version", 1).unwrap();
        assert!(fixture
            .read()
            .unwrap_err()
            .to_string()
            .contains("no schedules table"));
        invalid
            .execute_batch("CREATE TABLE unrelated(value TEXT)")
            .unwrap();
        assert!(fixture
            .read()
            .unwrap_err()
            .to_string()
            .contains("unknown table"));
    }

    #[test]
    fn wal_source_is_rejected_before_sqlite_can_create_sidecars() {
        let mut fixture = Fixture::new();
        fixture
            .db()
            .pragma_update(None, "journal_mode", "WAL")
            .unwrap();
        drop(fixture.database.take());
        let before = entries(&fixture.home);
        assert!(fixture
            .read()
            .unwrap_err()
            .to_string()
            .contains("rollback-journal backup"));
        assert_eq!(entries(&fixture.home), before);
    }

    #[test]
    fn offline_import_roundtrips_schedule_rows_and_exact_side_tables() {
        let fixture = Fixture::new();
        let original_result = r#"{"status": 409, "body": {"error": "same reply"}}"#;
        fixture.db().execute(
            "INSERT INTO local_command_results(sender,request_id,digest,result) VALUES('phone','old-command',?1,?2)",
            rusqlite::params![vec![9u8; 32], original_result],
        ).unwrap();
        fixture
            .db()
            .execute(
                "INSERT INTO schedule_failed_launches(request) VALUES('request-5')",
                [],
            )
            .unwrap();
        fixture.db().execute(
            "INSERT INTO schedule_launch_tools(request,patterns) VALUES('request-5','[\"Read\"]')", [],
        ).unwrap();
        let source_before = entries(&fixture.home);
        let stage = fixture.root.join("stage");
        fs::create_dir(&stage).unwrap();
        let path = stage.join("domain-v2.db");
        let mut store = crate::domain_store::DomainStore::open(&path).unwrap();
        let epoch = store.load().unwrap().data_epoch;
        let (state, report, schedules) =
            super::super::materialize_with_schedules(&fixture.view, &super::super::import::Source { home: &fixture.home, recorded_home: &fixture.home }, &stage, epoch)
                .unwrap();
        assert_eq!(schedules.counts(), (11, 5, 6, 1, 1));
        store
            .import_initial(&state, &report.links, fixture.view.seq, &schedules)
            .unwrap();
        store.integrity_check().unwrap();
        assert_eq!(store.load().unwrap(), state);
        assert_eq!(entries(&fixture.home), source_before);

        let target = Connection::open(&path).unwrap();
        let counts: (i64, i64, i64, i64, i64) = target
            .query_row(
                "SELECT (SELECT count(*) FROM schedules), (SELECT count(*) FROM schedule_runs),
                (SELECT count(*) FROM schedule_request_tools),
                (SELECT count(*) FROM legacy_schedule_receipts),
                (SELECT count(*) FROM schedule_failed_launches)",
                [],
                |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                    ))
                },
            )
            .unwrap();
        assert_eq!(counts, (11, 5, 6, 1, 1));
        let (digest, result): (Vec<u8>, String) = target.query_row(
            "SELECT digest,result FROM legacy_schedule_receipts WHERE sender='phone' AND request_id='old-command'",
            [], |row| Ok((row.get(0)?, row.get(1)?)),
        ).unwrap();
        assert_eq!(digest, vec![9u8; 32]);
        assert_eq!(result, original_result);
        let patterns: String = target
            .query_row(
                "SELECT patterns FROM schedule_request_tools WHERE request_id='request-0'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(
            serde_json::from_str::<Vec<String>>(&patterns).unwrap(),
            vec!["Bash"]
        );
        let historical_patterns: String = target
            .query_row(
                "SELECT patterns FROM schedule_request_tools WHERE request_id='request-5'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(
            serde_json::from_str::<Vec<String>>(&historical_patterns).unwrap(),
            vec!["Read"]
        );
        let invented_run: i64 = target
            .query_row(
                "SELECT count(*) FROM schedule_runs WHERE request_id='request-5'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(invented_run, 0);
        let historical_marker: String = target
            .query_row(
                "SELECT request_id FROM schedule_failed_launches",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(historical_marker, "request-5");
        let foreign_keys: i64 = target
            .query_row("SELECT count(*) FROM pragma_foreign_key_check", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(foreign_keys, 0);
    }

    #[test]
    fn rejected_historical_marker_rolls_back_state_and_side_tables() {
        let fixture = Fixture::new();
        fixture.db().execute(
            "INSERT INTO local_command_results(sender,request_id,digest,result) VALUES('phone','old-command',?1,'{}')",
            rusqlite::params![vec![4u8; 32]],
        ).unwrap();
        let stage = fixture.root.join("stage");
        fs::create_dir(&stage).unwrap();
        let path = stage.join("domain-v2.db");
        let mut store = crate::domain_store::DomainStore::open(&path).unwrap();
        let epoch = store.load().unwrap().data_epoch;
        let (state, report, mut schedules) =
            super::super::materialize_with_schedules(&fixture.view, &super::super::import::Source { home: &fixture.home, recorded_home: &fixture.home }, &stage, epoch)
                .unwrap();
        schedules
            .failed_launches
            .insert(d::RequestId("orphan-marker".into()));
        assert!(store
            .import_initial(&state, &report.links, fixture.view.seq, &schedules)
            .is_err());
        store.integrity_check().unwrap();
        let target = Connection::open(&path).unwrap();
        let counts: (i64, i64, i64, i64, i64) = target
            .query_row(
                "SELECT (SELECT count(*) FROM requests), (SELECT count(*) FROM schedules),
                (SELECT count(*) FROM schedule_request_tools),
                (SELECT count(*) FROM legacy_schedule_receipts),
                (SELECT count(*) FROM schedule_failed_launches)",
                [],
                |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                    ))
                },
            )
            .unwrap();
        assert_eq!(counts, (0, 0, 0, 0, 0));
    }
}
