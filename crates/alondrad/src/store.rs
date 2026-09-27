use std::path::{Path, PathBuf};
use std::fs::{self, File, OpenOptions};
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt};

use alondra_core::{Event, Millis, View};
use anyhow::{Context, Result};
use rusqlite::{Connection, OpenFlags, OptionalExtension, Transaction, params};

pub struct Store {
    conn: Connection,
}

impl Store {
    pub fn open(path: &Path) -> Result<Self> {
        // Resolve the containing directory once; SQLite must not follow a leaf symlink.
        let parent = fs::canonicalize(path.parent().filter(|p| !p.as_os_str().is_empty()).unwrap_or(Path::new(".")))?;
        let path = parent.join(path.file_name().context("database filename required")?);
        let original = match fs::symlink_metadata(&path) {
            Ok(value) => Some(value),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
            Err(error) => return Err(error.into()),
        };
        let mut conn = Connection::open_with_flags(&path,
            OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_CREATE | OpenFlags::SQLITE_OPEN_NOFOLLOW)
            .with_context(|| format!("abriendo {}", path.display()))?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "synchronous", "FULL")?;
        // An old writer must fail before replay, even after snapshots compact away new events.
        let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let kind: Option<String> = tx.query_row("SELECT type FROM sqlite_schema WHERE name='events'", [], |r|r.get(0)).optional()?;
        let new_kind: Option<String> = tx.query_row("SELECT type FROM sqlite_schema WHERE name='event_log'", [], |r|r.get(0)).optional()?;
        match (kind.as_deref(), new_kind.as_deref()) {
            (Some("table"), None) => {
                // Keep this write reservation until migration commits. A separate
                // read connection copies committed WAL pages; backing up `tx`
                // itself would be SQLITE_LOCKED because it is a writer.
                backup_before_delivery_migration(&path, &tx, original.as_ref().context("legacy database appeared while opening")?)?;
                tx.execute_batch("DROP INDEX IF EXISTS events_session; ALTER TABLE events RENAME TO event_log;")?;
            },
            (None, None) => tx.execute_batch("CREATE TABLE event_log (seq INTEGER PRIMARY KEY, at INTEGER NOT NULL, kind TEXT NOT NULL, session TEXT, body TEXT NOT NULL);")?,
            (Some("view"), Some("table")) => (),
            _ => anyhow::bail!("unsupported event storage layout; refusing delivery migration"),
        }
        tx.execute_batch("CREATE VIEW IF NOT EXISTS events AS SELECT seq,at,kind,session,body FROM event_log;
            CREATE INDEX IF NOT EXISTS event_log_session ON event_log(session);
            CREATE TABLE IF NOT EXISTS local_command_results (
                sender TEXT NOT NULL, request_id TEXT NOT NULL, digest BLOB NOT NULL,
                result TEXT NOT NULL, PRIMARY KEY(sender, request_id)
            );
            CREATE TABLE IF NOT EXISTS snapshots (
                seq INTEGER PRIMARY KEY,
                at INTEGER NOT NULL,
                body TEXT NOT NULL
            );",
        )?;
        tx.commit()?;
        Ok(Store { conn })
    }

    pub(crate) fn command_result(tx: &Transaction<'_>, sender: &str, id: &str, digest: &[u8; 32]) -> Result<Option<serde_json::Value>> {
        let stored: Option<(Vec<u8>, String)> = tx.query_row(
            "SELECT digest,result FROM local_command_results WHERE sender=?1 AND request_id=?2",
            params![sender, id], |r| Ok((r.get(0)?, r.get(1)?))).optional()?;
        stored.map(|(previous, result)| {
            anyhow::ensure!(previous == digest, "request ID reused with different encrypted bytes");
            Ok(serde_json::from_str(&result)?)
        }).transpose()
    }

    pub(crate) fn save_command_result(tx: &Transaction<'_>, sender: &str, id: &str, digest: &[u8; 32], result: &serde_json::Value) -> Result<()> {
        tx.execute("INSERT INTO local_command_results(sender,request_id,digest,result) VALUES(?1,?2,?3,?4)",
            params![sender, id, digest.as_slice(), serde_json::to_string(result)?])?;
        Ok(())
    }

    pub fn append(&mut self, at: Millis, event: &Event) -> Result<u64> {
        Self::insert(&self.conn, at, event)
    }

    fn insert(conn: &Connection, at: Millis, event: &Event) -> Result<u64> {
        let body = serde_json::to_string(event)?;
        // Compaction can delete every event while keeping a snapshot. SQLite's implicit
        // rowid would then restart at 1, below that snapshot, and replay would miss writes.
        conn.execute(
            "INSERT INTO event_log (seq, at, kind, session, body)
             VALUES ((SELECT MAX(seq) + 1 FROM (
                 SELECT COALESCE(MAX(seq), 0) AS seq FROM event_log
                 UNION ALL SELECT COALESCE(MAX(seq), 0) FROM snapshots
             )), ?1, ?2, ?3, ?4)",
            params![at, event.kind(), event.session_id(), body],
        )?;
        Ok(conn.last_insert_rowid() as u64)
    }

    /// A source event and every task event it causes are one durable fact. A failed closure or
    /// commit rolls the whole batch back, including any earlier events in the same API call.
    pub fn transaction<T>(&mut self, f: impl FnOnce(&Transaction<'_>) -> Result<T>) -> Result<T> {
        let tx = self.conn.transaction()?;
        let result = f(&tx)?;
        tx.commit()?;
        Ok(result)
    }

    pub fn append_in(tx: &Transaction<'_>, at: Millis, event: &Event) -> Result<u64> {
        Self::insert(tx, at, event)
    }

    pub fn latest_snapshot(&self) -> Result<Option<(u64, View)>> {
        let row = self.conn.query_row("SELECT seq, body FROM snapshots ORDER BY seq DESC LIMIT 1", [], |r| {
            Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?))
        });
        match row {
            Ok((seq, body)) => Ok(Some((seq as u64, serde_json::from_str(&body).context("unreadable snapshot")?))),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    pub fn compact(&mut self, view: &View, at: Millis, before: Millis) -> Result<usize> {
        let body = serde_json::to_string(view)?;
        let tx = self.conn.transaction()?;
        tx.execute("INSERT OR REPLACE INTO snapshots (seq, at, body) VALUES (?1, ?2, ?3)", params![view.seq as i64, at, body])?;
        tx.execute("DELETE FROM snapshots WHERE seq < ?1", params![view.seq as i64])?;
        let deleted = tx.execute("DELETE FROM event_log WHERE seq <= ?1 AND at < ?2", params![view.seq as i64, before])?;
        tx.commit()?;
        self.conn.execute_batch("VACUUM; PRAGMA wal_checkpoint(TRUNCATE);")?;
        Ok(deleted)
    }

    pub fn replay(&self, after: u64, mut f: impl FnMut(u64, Millis, Event)) -> Result<u64> {
        let mut stmt = self.conn.prepare("SELECT seq, at, body FROM event_log WHERE seq > ?1 ORDER BY seq")?;
        let mut rows = stmt.query(params![after as i64])?;
        let mut count = 0;
        while let Some(row) = rows.next()? {
            let seq: i64 = row.get(0)?;
            let at: i64 = row.get(1)?;
            let body: String = row.get(2)?;
            let ev = serde_json::from_str::<Event>(&body)
                .with_context(|| format!("event {seq} is unreadable; replay stopped before state can diverge"))?;
            f(seq as u64, at, ev);
            count += 1;
        }
        Ok(count)
    }

    /// The session's newest `limit` events before `before` (all of them when it is `None`), newest first.
    pub fn session_timeline(&self, session: &str, limit: usize, before: Option<u64>) -> Result<Vec<(u64, Millis, String)>> {
        let mut stmt = self.conn.prepare(
            "SELECT seq, at, body FROM event_log WHERE session = ?1 AND seq < ?3 ORDER BY seq DESC LIMIT ?2",
        )?;
        let before = before.map_or(i64::MAX, |b| b as i64);
        let rows = stmt.query_map(params![session, limit as i64, before], |r| {
            Ok((r.get::<_, i64>(0)? as u64, r.get::<_, i64>(1)?, r.get::<_, String>(2)?))
        })?;
        Ok(rows.filter_map(Result::ok).collect())
    }
}

/// A completed generation is a standalone SQLite file in an owner-only directory.
/// Never replace previous generations: a failed migration may be retried after new writes.
fn backup_before_delivery_migration(path: &Path, _writer: &Transaction<'_>, before: &fs::Metadata) -> Result<PathBuf> {
    let current = fs::symlink_metadata(path)?;
    anyhow::ensure!((before.dev(), before.ino()) == (current.dev(), current.ino()), "migration source replaced after opening");
    anyhow::ensure!(before.is_file() && before.nlink() == 1 && before.uid() == unsafe { libc::geteuid() }, "unsafe migration source");
    let parent = path.parent().context("database parent required")?;
    let mut name = path.file_name().context("database filename required")?.to_os_string();
    name.push(".migration-backups");
    let directory = parent.join(name);
    match fs::DirBuilder::new().mode(0o700).create(&directory) {
        Ok(()) => (),
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => (),
        Err(error) => return Err(error.into()),
    }
    let directory_meta = fs::symlink_metadata(&directory)?;
    anyhow::ensure!(directory_meta.is_dir() && directory_meta.uid() == unsafe { libc::geteuid() }
        && directory_meta.mode() & 0o077 == 0, "migration backup directory must be private and owned");
    let generation = directory.join(format!("pre-delivery-{}.sqlite", crate::auth::random_hex(16)));
    let output = OpenOptions::new().write(true).read(true).create_new(true).mode(0o600)
        .custom_flags(libc::O_NOFOLLOW).open(&generation)?;
    let source = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NOFOLLOW)?;
    let mut destination = Connection::open_with_flags(&generation, OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NOFOLLOW)?;
    destination.pragma_update(None, "synchronous", "FULL")?;
    let result = (|| -> Result<()> {
        let actual = fs::symlink_metadata(path)?;
        anyhow::ensure!((before.dev(), before.ino()) == (actual.dev(), actual.ino()), "migration source replaced");
        let legacy: String = source.query_row("SELECT type FROM sqlite_schema WHERE name='events'", [], |r| r.get(0))?;
        anyhow::ensure!(legacy == "table", "backup source is not the legacy schema");
        {
            let backup = rusqlite::backup::Backup::new(&source, &mut destination)?;
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
            loop {
                match backup.step(512)? {
                    rusqlite::backup::StepResult::Done => break,
                    rusqlite::backup::StepResult::More if std::time::Instant::now() < deadline => (),
                    _ => anyhow::bail!("migration backup could not complete under write reservation"),
                }
            }
        }
        destination.pragma_update(None, "journal_mode", "DELETE")?;
        let integrity: String = destination.query_row("PRAGMA integrity_check", [], |r| r.get(0))?;
        anyhow::ensure!(integrity == "ok", "migration backup integrity check failed");
        let legacy: String = destination.query_row("SELECT type FROM sqlite_schema WHERE name='events'", [], |r| r.get(0))?;
        anyhow::ensure!(legacy == "table", "migration backup lost legacy layout");
        let actual = fs::symlink_metadata(path)?;
        let dir_after = fs::symlink_metadata(&directory)?;
        let file_after = fs::symlink_metadata(&generation)?;
        anyhow::ensure!((before.dev(), before.ino()) == (actual.dev(), actual.ino())
            && (directory_meta.dev(), directory_meta.ino()) == (dir_after.dev(), dir_after.ino())
            && (output.metadata()?.dev(), output.metadata()?.ino()) == (file_after.dev(), file_after.ino()),
            "migration paths changed during backup");
        Ok(())
    })();
    drop(destination);
    result?;
    // Completion marker is published only after DB bytes and directory entries are durable.
    output.sync_all()?;
    File::open(&directory)?.sync_all()?;
    File::open(parent)?.sync_all()?;
    let marker = generation.with_extension("complete");
    let complete = OpenOptions::new().write(true).create_new(true).mode(0o600)
        .custom_flags(libc::O_NOFOLLOW).open(marker)?;
    complete.sync_all()?;
    File::open(&directory)?.sync_all()?;
    log!("legacy database backup completed before delivery migration: {}", generation.display());
    Ok(generation)
}

#[cfg(test)]
mod tests {
    use super::*;
    use alondra_core::{HookEvent, states, upkeep};
    use std::collections::BTreeSet;

    fn legacy_backup_fixture() -> (PathBuf, Connection) {
        let home = std::env::temp_dir().join(format!("alondra-migration-backup-{}", crate::auth::random_hex(8)));
        fs::DirBuilder::new().mode(0o700).create(&home).unwrap();
        let db = Connection::open(home.join("alondra.db")).unwrap();
        db.execute_batch("PRAGMA journal_mode=WAL; PRAGMA wal_autocheckpoint=0;
            CREATE TABLE events(seq INTEGER PRIMARY KEY,at INTEGER NOT NULL,kind TEXT NOT NULL,session TEXT,body TEXT NOT NULL);
            CREATE INDEX events_session ON events(session);
            CREATE TABLE snapshots(seq INTEGER PRIMARY KEY,at INTEGER NOT NULL,body TEXT NOT NULL);
            PRAGMA wal_checkpoint(TRUNCATE);").unwrap();
        db.execute("INSERT INTO events VALUES(7,42,'clock',NULL,?1)", [serde_json::to_string(&Event::Clock {machine_strained:false}).unwrap()]).unwrap();
        db.execute("INSERT INTO snapshots VALUES(0,1,?1)", [serde_json::to_string(&View::default()).unwrap()]).unwrap();
        (home, db)
    }

    #[test]
    fn migration_backup_includes_wal_is_private_restorable_and_not_repeated() {
        let (home, old) = legacy_backup_fixture();
        let path = home.join("alondra.db");
        assert!(fs::metadata(home.join("alondra.db-wal")).unwrap().len() > 0);
        let store = Store::open(&path).unwrap();
        let directory = home.join("alondra.db.migration-backups");
        let backups: Vec<_> = fs::read_dir(&directory).unwrap().map(|e| e.unwrap().path())
            .filter(|p| p.extension().is_some_and(|e| e == "sqlite")).collect();
        assert_eq!(backups.len(), 1);
        let backup = &backups[0];
        assert!(backup.with_extension("complete").is_file());
        assert_eq!(fs::metadata(&directory).unwrap().mode() & 0o777, 0o700);
        assert_eq!(fs::metadata(backup).unwrap().mode() & 0o777, 0o600);
        let restored = home.join("restored.sqlite");
        fs::copy(backup, &restored).unwrap();
        let restored = Connection::open(&restored).unwrap();
        assert_eq!(restored.query_row("PRAGMA integrity_check", [], |r| r.get::<_,String>(0)).unwrap(), "ok");
        assert_eq!(restored.query_row("SELECT count(*) FROM events WHERE seq=7", [], |r| r.get::<_,i64>(0)).unwrap(), 1);
        assert_eq!(restored.query_row("SELECT count(*) FROM snapshots", [], |r| r.get::<_,i64>(0)).unwrap(), 1);
        assert_eq!(restored.query_row("SELECT type FROM sqlite_schema WHERE name='events'", [], |r| r.get::<_,String>(0)).unwrap(), "table");
        // The exact old layout accepts 0.1.4's index statement and remains writable.
        restored.execute_batch("CREATE INDEX IF NOT EXISTS events_session ON events(session); INSERT INTO events VALUES(8,43,'clock',NULL,'restored-only');").unwrap();
        drop(store);
        let current = Store::open(&path).unwrap();
        assert_eq!(fs::read_dir(&directory).unwrap().count(), 2, "current schema must not create another generation");
        assert_eq!(current.conn.query_row("SELECT count(*) FROM event_log WHERE seq=8", [], |r| r.get::<_,i64>(0)).unwrap(), 0);
        drop((current, restored, old));
        fs::remove_dir_all(home).unwrap();
    }

    #[test]
    fn migration_backup_failure_or_foreign_path_never_alters_legacy_schema() {
        use std::os::unix::fs::symlink;
        for link in [false, true] {
            let (home, db) = legacy_backup_fixture();
            let directory = home.join("alondra.db.migration-backups");
            if link { symlink(&home, &directory).unwrap(); }
            else { File::create(&directory).unwrap(); }
            assert!(Store::open(&home.join("alondra.db")).is_err());
            assert_eq!(db.query_row("SELECT type FROM sqlite_schema WHERE name='events'", [], |r| r.get::<_,String>(0)).unwrap(), "table");
            assert_eq!(db.query_row("SELECT count(*) FROM events", [], |r| r.get::<_,i64>(0)).unwrap(), 1);
            assert_eq!(db.query_row("SELECT count(*) FROM sqlite_schema WHERE name='event_log'", [], |r| r.get::<_,i64>(0)).unwrap(), 0);
            drop(db); fs::remove_dir_all(home).unwrap();
        }
    }

    #[test]
    fn migration_backup_reserves_writer_across_snapshot_and_ddl() {
        let (home, competitor) = legacy_backup_fixture();
        // Match Store::open's canonical-parent boundary before the NOFOLLOW
        // helper. macOS temp_dir uses /var, a symlink to /private/var.
        let path = fs::canonicalize(&home).unwrap().join("alondra.db");
        assert_eq!(fs::metadata(&path).unwrap().ino(), fs::metadata(home.join("alondra.db")).unwrap().ino());
        let before = fs::symlink_metadata(&path).unwrap();
        let mut migration = Connection::open(&path).unwrap();
        let tx = migration.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate).unwrap();
        competitor.busy_timeout(std::time::Duration::ZERO).unwrap();
        assert!(competitor.execute("INSERT INTO events VALUES(8,43,'clock',NULL,'too-late')", []).is_err());
        let backup = backup_before_delivery_migration(&path, &tx, &before).unwrap();
        assert!(competitor.execute("INSERT INTO events VALUES(8,43,'clock',NULL,'too-late')", []).is_err());
        tx.execute_batch("DROP INDEX events_session; ALTER TABLE events RENAME TO event_log; CREATE VIEW events AS SELECT * FROM event_log;").unwrap();
        tx.commit().unwrap();
        let saved = Connection::open(backup).unwrap();
        assert_eq!(saved.query_row("SELECT count(*) FROM events", [], |r| r.get::<_,i64>(0)).unwrap(), 1);
        assert!(competitor.execute("INSERT INTO events VALUES(8,43,'clock',NULL,'old-writer')", []).is_err());
        drop((saved, competitor, migration)); fs::remove_dir_all(home).unwrap();
    }

    #[test]
    fn migration_backup_rejects_replaced_source_identity() {
        let (home, original) = legacy_backup_fixture();
        let path = home.join("alondra.db");
        let before = fs::symlink_metadata(&path).unwrap();
        // Passing metadata from another file models replacement between open and backup.
        let replacement = home.join("replacement.sqlite");
        let mut other = Connection::open(&replacement).unwrap();
        other.execute_batch("CREATE TABLE events(x);").unwrap();
        let tx = other.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate).unwrap();
        assert!(backup_before_delivery_migration(&replacement, &tx, &before).is_err());
        assert!(!home.join("replacement.sqlite.migration-backups").exists());
        drop(tx); drop((other, original)); fs::remove_dir_all(home).unwrap();
    }

    #[test]
    #[ignore = "requires ALONDRA_LEGACY_DAEMON_TEST pointing to released 0.1.4 daemon"]
    fn delivery_released_014_daemon_refuses_migrated_database_before_event_mutation() {
        let binary=std::env::var_os("ALONDRA_LEGACY_DAEMON_TEST").expect("provide released daemon path");
        let version=std::process::Command::new(&binary).arg("--version").output().unwrap();
        assert_eq!(String::from_utf8_lossy(&version.stdout).trim(),"alondrad 0.1.4");
        let home=std::env::temp_dir().join(format!("delivery-old-writer-{}",crate::auth::random_hex(6)));
        std::fs::create_dir(&home).unwrap();
        let mut store=Store::open(&home.join("alondra.db")).unwrap();
        store.append(1,&Event::Clock {machine_strained:false}).unwrap();
        let before:Vec<(i64,String)>=store.conn.prepare("SELECT seq,body FROM events ORDER BY seq").unwrap().query_map([],|r|Ok((r.get(0)?,r.get(1)?))).unwrap().collect::<rusqlite::Result<_>>().unwrap();
        let stderr=std::fs::File::create(home.join("old-stderr.log")).unwrap();
        let mut child=std::process::Command::new(&binary).env("ALONDRA_HOME",&home).env("ALONDRA_PORT","0").env("ALONDRA_WAKE","0").env("ALONDRA_SANDBOX","1")
            .stdout(std::process::Stdio::null()).stderr(stderr).spawn().unwrap();
        let deadline=std::time::Instant::now()+std::time::Duration::from_secs(5);
        let status=loop {
            if let Some(status)=child.try_wait().unwrap() {break status;}
            if std::time::Instant::now()>deadline {child.kill().unwrap();child.wait().unwrap();panic!("legacy daemon failed to reject migration promptly");}
            std::thread::sleep(std::time::Duration::from_millis(10));
        };
        assert!(!status.success());
        let error=std::fs::read_to_string(home.join("old-stderr.log")).unwrap();
        assert!(error.contains("views may not be indexed"),"{error}");
        let after:Vec<(i64,String)>=store.conn.prepare("SELECT seq,body FROM events ORDER BY seq").unwrap().query_map([],|r|Ok((r.get(0)?,r.get(1)?))).unwrap().collect::<rusqlite::Result<_>>().unwrap();
        assert_eq!(before,after);assert!(!home.join("hooks.sock").exists());
        drop(store);std::fs::remove_dir_all(home).unwrap();
    }

    #[test]
    #[ignore = "requires ALONDRA_MENU_LEGACY_DAEMON_TEST pointing to released 0.1.5 daemon"]
    fn menu_released_015_daemon_refuses_events_and_snapshots_before_processing_hooks() {
        use std::os::unix::fs::PermissionsExt;
        let binary=std::env::var_os("ALONDRA_MENU_LEGACY_DAEMON_TEST").expect("provide released 0.1.5 daemon path");
        let version=std::process::Command::new(&binary).arg("--version").output().unwrap();
        assert!(version.status.success());
        assert_eq!(String::from_utf8_lossy(&version.stdout).trim(),"alondrad 0.1.5");
        for snapshot in [false,true] {
            let home=std::env::temp_dir().join(format!("menu-old-reader-{}",crate::auth::random_hex(6)));
            std::fs::create_dir(&home).unwrap();
            struct Cleanup(PathBuf);
            impl Drop for Cleanup {fn drop(&mut self){let _=std::fs::remove_dir_all(&self.0);}}
            let _cleanup=Cleanup(home.clone());
            let mut store=Store::open(&home.join("alondra.db")).unwrap();
            let event=Event::DeliveryAttemptRecorded(alondra_core::delivery::Attempt {
                id:1,session_id:"fixture".into(),channel:alondra_core::delivery::Channel::MenuDecision,
                message_ids:vec![],question_id:Some(9),state:alondra_core::delivery::State::Uncertain,
                version:2,reason:Some("fixture_write_unknown".into()),created_at:1,updated_at:1,resolved_from_version:None,
            });
            let sequence=store.append(1,&event).unwrap();
            if snapshot {
                let mut view=View::default();view.apply(sequence,1,&event);
                store.compact(&view,2,2).unwrap();
                assert!(store.latest_snapshot().unwrap().is_some(),"new reader understands snapshot");
            } else {
                assert_eq!(store.replay(0,|_,_,_|{}).unwrap(),1,"new reader understands event");
            }
            let rows=|store:&Store,table:&str|->Vec<(i64,String)>{
                store.conn.prepare(&format!("SELECT seq,body FROM {table} ORDER BY seq")).unwrap()
                    .query_map([],|r|Ok((r.get(0)?,r.get(1)?))).unwrap().collect::<rusqlite::Result<_>>().unwrap()
            };
            let before=(rows(&store,"event_log"),rows(&store,"snapshots"));
            let bin=home.join("bin");std::fs::create_dir(&bin).unwrap();
            for command in ["claude","tmux","gh"] {
                let path=bin.join(command);
                std::fs::write(&path,b"#!/bin/sh\nprintf unexpected > \"$HOME/external-effect\"\nexit 99\n").unwrap();
                std::fs::set_permissions(path,std::fs::Permissions::from_mode(0o700)).unwrap();
            }
            let stderr=std::fs::File::create(home.join("old-stderr.log")).unwrap();
            let mut child=std::process::Command::new(&binary).env_clear().env("HOME",&home)
                .env("PATH",format!("{}:/usr/bin:/bin",bin.display())).env("USER","menu-fixture").env("LOGNAME","menu-fixture")
                .env("ALONDRA_HOME",&home).env("CLAUDE_CONFIG_DIR",home.join("claude"))
                .env("ALONDRA_LISTEN","127.0.0.1").env("ALONDRA_PORT","0").env("ALONDRA_WAKE","1").env("ALONDRA_SANDBOX","1")
                .stdin(std::process::Stdio::null()).stdout(std::process::Stdio::null()).stderr(stderr).spawn().unwrap();
            let deadline=std::time::Instant::now()+std::time::Duration::from_secs(5);
            let status=loop {
                if let Some(status)=child.try_wait().unwrap(){break status;}
                if std::time::Instant::now()>deadline{child.kill().unwrap();child.wait().unwrap();panic!("old menu reader did not reject promptly");}
                std::thread::sleep(std::time::Duration::from_millis(10));
            };
            assert!(!status.success());
            let error=std::fs::read_to_string(home.join("old-stderr.log")).unwrap();
            assert!(error.contains("unknown variant `menu_decision`"),"snapshot={snapshot}: {error}");
            assert!(error.contains(if snapshot {"unreadable snapshot"} else {"unreadable; replay stopped"}),"{error}");
            assert_eq!(before,(rows(&store,"event_log"),rows(&store,"snapshots")),"old binary cannot append recovery or alter snapshot");
            assert!(!home.join("external-effect").exists(),"no terminal/provider command may run");
            // 0.1.5 reserves its socket before replay, but must never start serving.
            assert!(std::os::unix::net::UnixStream::connect(home.join("hooks.sock")).is_err());
            drop(store);
        }
    }

    #[test]
    fn delivery_schema_migration_preserves_rows_snapshots_and_blocks_previous_writer() {
        let path=std::env::temp_dir().join(format!("delivery-migration-{}.db",crate::auth::random_hex(6)));
        let old=Connection::open(&path).unwrap();
        old.execute_batch("CREATE TABLE events(seq INTEGER PRIMARY KEY,at INTEGER NOT NULL,kind TEXT NOT NULL,session TEXT,body TEXT NOT NULL); CREATE INDEX events_session ON events(session); CREATE TABLE snapshots(seq INTEGER PRIMARY KEY,at INTEGER NOT NULL,body TEXT NOT NULL);").unwrap();
        let event=serde_json::to_string(&Event::Clock {machine_strained:false}).unwrap();
        let snapshot=serde_json::to_string(&View::default()).unwrap();
        old.execute("INSERT INTO events VALUES(9,123,'clock',NULL,?1)",[&event]).unwrap();
        old.execute("INSERT INTO snapshots VALUES(3,99,?1)",[&snapshot]).unwrap();drop(old);
        let mut store=Store::open(&path).unwrap();
        assert_eq!(store.conn.query_row("SELECT body FROM events WHERE seq=9",[],|r|r.get::<_,String>(0)).unwrap(),event);
        assert_eq!(store.conn.query_row("SELECT body FROM snapshots WHERE seq=3",[],|r|r.get::<_,String>(0)).unwrap(),snapshot);
        // This is the unmodified 0.1.4 Store::open index statement. The compatibility view must reject it.
        assert!(store.conn.execute_batch("CREATE TABLE IF NOT EXISTS events(seq INTEGER PRIMARY KEY,at INTEGER NOT NULL,kind TEXT NOT NULL,session TEXT,body TEXT NOT NULL); CREATE INDEX IF NOT EXISTS events_session ON events(session);").unwrap_err().to_string().contains("views may not be indexed"));
        let mut view=View::default();view.seq=9;
        store.compact(&view,200,199).unwrap();
        assert!(store.conn.execute_batch("CREATE INDEX IF NOT EXISTS events_session ON events(session);").is_err(),"snapshot/compaction must not remove downgrade barrier");
        drop(store);std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn delivery_schema_failure_rolls_back_rename_index_and_rows() {
        let path=std::env::temp_dir().join(format!("delivery-migration-failure-{}.db",crate::auth::random_hex(6)));
        let db=Connection::open(&path).unwrap();
        db.execute_batch("CREATE TABLE events(seq INTEGER PRIMARY KEY,at INTEGER NOT NULL,kind TEXT NOT NULL,session TEXT,body TEXT NOT NULL); CREATE INDEX events_session ON events(session); INSERT INTO events VALUES(7,42,'clock',NULL,'original bytes'); CREATE TABLE event_log_session(x);").unwrap();
        assert!(Store::open(&path).is_err(),"index namespace collision injects failure after rename");
        assert_eq!(db.query_row("SELECT type FROM sqlite_schema WHERE name='events'",[],|r|r.get::<_,String>(0)).unwrap(),"table");
        assert_eq!(db.query_row("SELECT body FROM events WHERE seq=7",[],|r|r.get::<_,String>(0)).unwrap(),"original bytes");
        assert!(db.query_row("SELECT 1 FROM sqlite_schema WHERE name='events_session'",[],|r|r.get::<_,u8>(0)).is_ok());
        assert!(db.query_row("SELECT 1 FROM sqlite_schema WHERE name='event_log'",[],|r|r.get::<_,u8>(0)).optional().unwrap().is_none());
        drop(db);std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn append_then_replay_rebuilds_same_events() {
        let dir = std::env::temp_dir().join(format!("alondra-store-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("t.db");
        let _ = std::fs::remove_file(&path);
        let mut s = Store::open(&path).unwrap();
        let ev = Event::Hook(HookEvent { session_id: "s".into(), event: "Stop".into(), ..Default::default() });
        let seq = s.append(5, &ev).unwrap();
        let mut seen = vec![];
        s.replay(0, |q, at, e| seen.push((q, at, e))).unwrap();
        assert_eq!(seen, vec![(seq, 5, ev)]);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn sequence_stays_above_snapshot_after_all_events_are_compacted() {
        let dir = std::env::temp_dir().join(format!("alondra-sequence-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("t.db");
        let _ = std::fs::remove_file(&path);
        let mut s = Store::open(&path).unwrap();
        let ev = Event::Hook(HookEvent { session_id: "s".into(), event: "Stop".into(), ..Default::default() });
        let mut view = View::default();
        for at in 1..=3 {
            let seq = s.append(at, &ev).unwrap();
            view.apply(seq, at, &ev);
        }
        let snapshot = view.seq;
        assert_eq!(s.compact(&view, 100, 100).unwrap(), 3);
        drop(s);
        let mut s = Store::open(&path).unwrap();
        let seq = s.append(101, &ev).unwrap();
        assert!(seq > snapshot);
        let mut seen = vec![];
        s.replay(snapshot, |seq, _, _| seen.push(seq)).unwrap();
        assert_eq!(seen, vec![seq]);
        drop(s);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn failed_batch_does_not_leave_a_source_event_without_its_task_move() {
        let dir = std::env::temp_dir().join(format!("alondra-rollback-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("t.db");
        let _ = std::fs::remove_file(&path);
        let mut s = Store::open(&path).unwrap();
        let ev = Event::Hook(HookEvent { session_id: "s".into(), event: "Stop".into(), ..Default::default() });
        let result: Result<()> = s.transaction(|tx| {
            Store::append_in(tx, 1, &ev)?;
            anyhow::bail!("simulated derived event failure")
        });
        assert!(result.is_err());
        assert_eq!(s.replay(0, |_, _, _| {}).unwrap(), 0);
        drop(s);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_session_timeline_pages_back_from_a_sequence_number() {
        let dir = std::env::temp_dir().join(format!("alondra-timeline-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("t.db");
        let _ = std::fs::remove_file(&path);
        let mut s = Store::open(&path).unwrap();
        let hook = |session: &str| Event::Hook(HookEvent { session_id: session.into(), event: "Stop".into(), ..Default::default() });
        let mut mine = vec![];
        for i in 0..5 {
            mine.push(s.append(i, &hook("s")).unwrap());
            s.append(i, &hook("other")).unwrap();
        }
        let seqs = |limit, before| s.session_timeline("s", limit, before).unwrap().into_iter().map(|(seq, _, _)| seq).collect::<Vec<_>>();
        assert_eq!(seqs(2, None), vec![mine[4], mine[3]], "newest first");
        assert_eq!(seqs(2, Some(mine[3])), vec![mine[2], mine[1]]);
        assert_eq!(seqs(10, Some(mine[1])), vec![mine[0]]);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn compaction_leaves_the_write_ahead_log_empty() {
        let dir = std::env::temp_dir().join(format!("alondra-wal-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("t.db");
        let _ = std::fs::remove_file(&path);
        let mut s = Store::open(&path).unwrap();
        let mut view = View::default();
        for i in 0..2_000 {
            let ev = Event::Hook(HookEvent { session_id: format!("s{i}"), event: "Stop".into(), last_message: Some("x".repeat(500)), ..Default::default() });
            let seq = s.append(i, &ev).unwrap();
            view.apply(seq, i, &ev);
        }
        s.compact(&view, 10_000, 5_000).unwrap();
        let wal = std::fs::metadata(dir.join("t.db-wal")).map_or(0, |m| m.len());
        assert_eq!(wal, 0, "WAL still holds {wal} bytes after compaction");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    fn summary(view: &View) -> String {
        let mut out = String::new();
        let mut states: std::collections::BTreeMap<&str, usize> = std::collections::BTreeMap::new();
        for t in view.board.tasks.values() {
            *states.entry(t.state.name()).or_default() += 1;
        }
        let requests = view.board.requests.keys().map(|id| format!("{:?}", view.board.request_state(id)));
        let mut by_state: std::collections::BTreeMap<String, usize> = std::collections::BTreeMap::new();
        for r in requests {
            *by_state.entry(r).or_default() += 1;
        }
        out += &format!("tasks: {} {states:?}\nrequests: {} {by_state:?}\n", view.board.tasks.len(), view.board.requests.len());
        out += &format!("anomalies: {} {:?}\n", view.board.anomalies.values().sum::<u64>(), view.board.anomalies);
        let mut first: Vec<(&str, &alondra_core::tasks::Anomaly)> = view.board.tasks.values().flat_map(|t| t.anomalies.iter().map(move |a| (t.title.as_str(), a))).collect();
        first.sort_by_key(|(_, a)| a.at);
        for (title, a) in first.iter().take(10) {
            out += &format!("  {} {title}: {} + {} — {}\n", a.at, a.state.name(), serde_json::to_string(&a.event).unwrap_or_default(), a.reason);
        }
        let mut moves: std::collections::BTreeMap<String, usize> = std::collections::BTreeMap::new();
        for h in view.board.tasks.values().flat_map(|t| &t.history) {
            *moves.entry(format!("{}->{} {}", h.from.name(), h.to.name(), h.event.name())).or_default() += 1;
        }
        out += &format!("transitions: {moves:#?}\n");
        let history: usize = view.board.tasks.values().map(|t| serde_json::to_string(&t.history).map_or(0, |s| s.len())).sum();
        let briefs: usize = view.board.tasks.values().map(|t| t.brief.as_ref().map_or(0, String::len)).sum();
        out += &format!("board JSON: {} bytes ({history} of history, {briefs} of briefs)\n", serde_json::to_string(&view.board).map_or(0, |s| s.len()));
        out
    }

    /// A node shows done while a task of it is open, waits for a review nobody has to give, is
    /// stalled with nothing to move, or shows a delivered result over a task in review or finished.
    fn contradicts(v: &View, n: &alondra_core::Node, now: Millis) -> bool {
        use alondra_core::NodeState;
        use alondra_core::tasks::TaskState;
        let tasks: Vec<_> = v.tasks_of(&n.session_id).collect();
        let any = |states: &[TaskState]| tasks.iter().any(|t| states.contains(&t.state));
        let finished = !tasks.is_empty() && tasks.iter().all(|t| t.state.finished());
        match n.state {
            NodeState::Done => !tasks.is_empty() && !finished,
            NodeState::WaitingMerge => {
                let due = v.merge_notice_due(&n.session_id).is_some() || v.review_reply_due(&n.session_id).is_some();
                let waits = tasks.iter().any(|t| v.auto_merge_pending(t, now)) || states::awaits_merge(v, n, now).is_some();
                !due && !any(&[TaskState::InReview, TaskState::WaitingDependency]) && !waits
            }
            NodeState::Stalled => n.evidence.as_ref().is_none_or(|e| e.signal != alondra_core::Signal::CallStuck) && !any(&[TaskState::Running, TaskState::Closing]),
            NodeState::Delivered => any(&[TaskState::InReview]) || finished,
            _ => false,
        }
    }

    /// Replays a copy of the real log and prints the tasks it gives. Never point it at the live
    /// file: `cp ~/.alondra/alondra.db* /tmp/` and
    /// `ALONDRA_REPLAY_DB=/tmp/alondra.db cargo test --release -p alondrad replay_a_copy_of_the_real_log -- --ignored --nocapture`.
    /// In release, as the daemon runs: the log of 22-24 Sep trips the debug-only limbo check of
    /// `View::apply` on two nodes, with or without tasks.
    #[test]
    #[ignore]
    fn replay_a_copy_of_the_real_log() {
        let path = std::env::var("ALONDRA_REPLAY_DB").expect("ALONDRA_REPLAY_DB names a copy of alondra.db");
        let s = Store::open(Path::new(&path)).unwrap();
        let first: i64 = s.conn.query_row("SELECT COALESCE(MIN(seq), 0) FROM events", [], |r| r.get(0)).unwrap();
        let mut sessions = std::collections::BTreeSet::new();
        let mut full = View::default();
        let (mut idle, mut resumes, mut closable, mut threads) = (BTreeSet::new(), BTreeSet::new(), BTreeSet::new(), 0usize);
        let (mut shown, mut entered) = (std::collections::HashMap::new(), std::collections::BTreeMap::<&str, usize>::new());
        let mut contradicted = std::collections::BTreeMap::<&str, usize>::new();
        let two_hours = 2 * 3600 * 1000;
        let last_at: i64 = s.conn.query_row("SELECT COALESCE(MAX(at), 0) FROM events", [], |r| r.get(0)).unwrap();
        let hour_from = last_at - 3600 * 1000;
        let (mut hour_events, mut hour_hooks, mut batches, mut bumps) = (0usize, 0usize, 0usize, 0usize);
        let (mut batch_at, mut digest): (Option<i64>, Option<u64>) = (None, None);
        let n = s.replay(0, |seq, at, e| {
            if let Some(sid) = e.session_id() {
                sessions.insert(sid.to_string());
            }
            if at >= hour_from {
                match batch_at {
                    None => digest = Some(crate::http::overview_digest(&full, at)),
                    Some(b) if b != at => {
                        batches += 1;
                        let now = crate::http::overview_digest(&full, b);
                        bumps += usize::from(digest != Some(now));
                        digest = Some(now);
                    }
                    Some(_) => {}
                }
                batch_at = Some(at);
                hour_events += 1;
                hour_hooks += usize::from(matches!(e, Event::Hook(_)));
            }
            full.apply(seq, at, &e);
            let touched: Vec<String> = e.session_id().map_or_else(|| full.nodes.keys().cloned().collect(), |s| vec![s.to_string()]);
            for n in touched.iter().filter_map(|sid| full.nodes.get(sid)) {
                if shown.insert(n.session_id.clone(), n.state) != Some(n.state) {
                    *entered.entry(n.state.label()).or_default() += 1;
                }
                if contradicts(&full, n, at) {
                    *contradicted.entry(n.state.label()).or_default() += 1;
                }
                if states::idle_move(&full, n, at).is_some() {
                    idle.insert((n.session_id.clone(), n.episode_since));
                }
                if upkeep::resume_due(&full, n, at).is_some() {
                    resumes.insert((n.session_id.clone(), n.exited_at));
                }
                if upkeep::free_to_close(&full, n, at, two_hours) {
                    closable.insert(n.session_id.clone());
                }
                threads = threads.max(n.ledger.threads.len());
            }
        }).unwrap();
        if let Some(b) = batch_at {
            batches += 1;
            bumps += usize::from(digest != Some(crate::http::overview_digest(&full, b)));
        }
        println!(
            "last hour: {hour_events} events ({hour_hooks} hooks) in {batches} appends; every append moved the clients' tick before, {bumps} move it now (machine samples: 360 before, only 5 % steps now)"
        );
        println!("whole log: {n} events from seq {first}, {} sessions, {} nodes, {} removed", sessions.len(), full.nodes.len(), full.removed.len());
        println!("{}", summary(&full));
        let mut last = std::collections::BTreeMap::<&str, usize>::new();
        for n in full.nodes.values() {
            *last.entry(n.state.label()).or_default() += 1;
        }
        let statuses: Vec<String> = full.nodes.values().map(|n| format!("{:?}", n.status)).collect();
        println!("states entered: {entered:?}\nstates at the end: {last:?}\nstatus at the end: {statuses:?}");
        println!("events after which a node showed a state its tasks contradict: {contradicted:?}");
        println!(
            "episodes idle with a task to move: {}\nprocesses Alondra would restart: {} {resumes:?}\nnodes free for 2 h it could close: {}\nmost threads in one node: {threads}",
            idle.len(),
            resumes.len(),
            closable.len()
        );
        if let Some((after, mut resumed)) = s.latest_snapshot().unwrap() {
            resumed.backfill_created_at();
            resumed.backfill_tasks();
            let n = s.replay(after, |seq, at, e| resumed.apply(seq, at, &e)).unwrap();
            println!("from the snapshot at seq {after} and {n} events after it:\n{}", summary(&resumed));
        }
    }

    #[test]
    fn compaction_keeps_what_replay_needs_and_drops_old_events() {
        let dir = std::env::temp_dir().join(format!("alondra-compact-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("t.db");
        let _ = std::fs::remove_file(&path);
        let mut s = Store::open(&path).unwrap();
        let mut view = View::default();
        for (at, name) in [(1_000, "a"), (2_000, "b"), (9_000, "c")] {
            let ev = Event::Hook(HookEvent { session_id: name.into(), event: "Stop".into(), ..Default::default() });
            let seq = s.append(at, &ev).unwrap();
            view.apply(seq, at, &ev);
        }
        let deleted = s.compact(&view, 10_000, 5_000).unwrap();
        assert_eq!(deleted, 2);
        let late = Event::Hook(HookEvent { session_id: "d".into(), event: "Stop".into(), ..Default::default() });
        let seq = s.append(11_000, &late).unwrap();
        view.apply(seq, 11_000, &late);
        drop(s);

        let s = Store::open(&path).unwrap();
        let (after, mut rebuilt) = s.latest_snapshot().unwrap().unwrap();
        s.replay(after, |q, at, e| rebuilt.apply(q, at, &e)).unwrap();
        assert_eq!(rebuilt, view);
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
