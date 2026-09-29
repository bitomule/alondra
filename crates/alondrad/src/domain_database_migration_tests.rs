use super::*;
use rusqlite::params;
use sha2::{Digest, Sha256};
use std::os::unix::fs::{DirBuilderExt, PermissionsExt};

struct Home(PathBuf);
impl Home {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "alondra-database-migration-{}",
            crate::auth::random_hex(12)
        ));
        fs::DirBuilder::new().mode(0o700).create(&path).unwrap();
        Self(path)
    }
    fn path(&self) -> &Path {
        &self.0
    }
}
impl Drop for Home {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn committed_cutover(home: &Path) {
    let marker = crate::domain_cutover::Marker {
        format: 1,
        phase: crate::domain_cutover::Phase::Committed,
        backup_manifest_sha256: "a".repeat(64),
        source_seq: 0,
        data_epoch: "b".repeat(64),
    };
    fs::write(
        home.join(crate::domain_cutover::MARKER),
        serde_json::to_vec(&marker).unwrap(),
    )
    .unwrap();
    fs::set_permissions(
        home.join(crate::domain_cutover::MARKER),
        fs::Permissions::from_mode(0o600),
    )
    .unwrap();
}

fn source_database(home: &Path) -> Connection {
    let path = home.join(SOURCE);
    let store = crate::domain_store::DomainStore::open(&path).unwrap();
    drop(store);
    let conn = Connection::open(&path).unwrap();
    conn.execute("INSERT INTO schedules
        (id,title,text,project,options,disallowed_tools,frequency,hour,minute,weekday,interval,start_at,enabled,next_run,revision,deleted_at)
        VALUES ('schedule-keep','Synthetic schedule','Synthetic task',NULL,'{}','[]','daily',9,30,NULL,NULL,NULL,1,4102444800000,7,NULL)", []).unwrap();
    conn.execute("INSERT INTO schedule_runs
        (id,schedule_id,origin,claim_revision,due_at,claimed_at,finished_at,state,request_id,task_id,error,definition_snapshot,launch)
        VALUES ('run-keep','schedule-keep','legacy_last_observed',NULL,NULL,NULL,100,'interrupted',NULL,NULL,'legacy outcome unavailable',NULL,NULL)", []).unwrap();
    conn.execute(
        "INSERT INTO local_command_results(sender,request_id,digest,result) VALUES(?1,?2,?3,?4)",
        params![
            "device-1",
            "receipt-1",
            vec![5_u8; 32],
            r#"{"accepted":true}"#
        ],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO semantic_receipts(actor,command_id,data_epoch,digest,result)
        SELECT 'system','command-keep',data_epoch,?1,'{}' FROM domain_meta WHERE singleton=1",
        [vec![7_u8; 32]],
    )
    .unwrap();
    conn.pragma_update(None, "journal_mode", "WAL").unwrap();
    conn.pragma_update(None, "wal_autocheckpoint", 0).unwrap();
    conn.execute(
        "UPDATE schedules SET next_run=4102444799000 WHERE id='schedule-keep'",
        [],
    )
    .unwrap();
    committed_cutover(home);
    fs::create_dir_all(home.join("files/domain-v2")).unwrap();
    fs::write(
        home.join("files/domain-v2/kept-attachment"),
        b"synthetic attachment bytes",
    )
    .unwrap();
    conn
}

fn assert_migrated(home: &Path) {
    let path = home.join(DATABASE);
    let conn = Connection::open_with_flags(&path, OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
    let schedule: (String, i64, i64) = conn
        .query_row("SELECT id,next_run,revision FROM schedules", [], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?))
        })
        .unwrap();
    assert_eq!(schedule, ("schedule-keep".into(), 4_102_444_799_000, 7));
    assert_eq!(
        conn.query_row(
            "SELECT count(*) FROM schedule_runs WHERE id='run-keep'",
            [],
            |r| r.get::<_, i64>(0)
        )
        .unwrap(),
        1
    );
    assert_eq!(
        conn.query_row(
            "SELECT count(*) FROM local_command_results WHERE request_id='receipt-1'",
            [],
            |r| r.get::<_, i64>(0)
        )
        .unwrap(),
        1
    );
    assert_eq!(
        conn.query_row(
            "SELECT count(*) FROM semantic_receipts WHERE command_id='command-keep'",
            [],
            |r| r.get::<_, i64>(0)
        )
        .unwrap(),
        1
    );
    drop(conn);
    assert_eq!(
        fs::read(home.join("files/domain-v2/kept-attachment")).unwrap(),
        b"synthetic attachment bytes"
    );
    assert!(
        home.join(SOURCE).exists(),
        "source is retained as a private recovery copy"
    );
    assert!(home.join(MARKER).exists());
    let state = crate::domain_runtime::DomainRuntime::open(home)
        .unwrap()
        .state()
        .unwrap();
    let schedule = &state.schedule.schedules[&"schedule-keep".into()];
    assert_eq!(schedule.next_run, Some(4_102_444_799_000));
    assert_eq!(schedule.revision, 7);
    assert!(state.schedule.runs.contains_key(&"run-keep".into()));
}

fn file_digest(path: &Path) -> Vec<u8> {
    Sha256::digest(fs::read(path).unwrap()).to_vec()
}

#[test]
fn migration_copies_wal_state_and_preserves_schedule_receipts_and_attachments() {
    let home = Home::new();
    let _source_writer = source_database(home.path());
    assert!(
        home.path().join(format!("{SOURCE}-wal")).exists(),
        "fixture must retain committed WAL frames"
    );
    let source_digest = file_digest(&home.path().join(SOURCE));
    let wal_digest = file_digest(&home.path().join(format!("{SOURCE}-wal")));
    assert!(migrate_locked(home.path(), |_| Ok(())).unwrap());
    assert_migrated(home.path());
    assert_eq!(active_path(home.path()), home.path().join(DATABASE));
    assert_eq!(file_digest(&home.path().join(SOURCE)), source_digest);
    assert_eq!(
        file_digest(&home.path().join(format!("{SOURCE}-wal"))),
        wal_digest
    );
    assert!(migrate_locked(home.path(), |_| Ok(())).unwrap());
    assert_migrated(home.path());
}

#[test]
fn committed_migration_allows_normal_writes_and_restarts_without_losing_them() {
    let home = Home::new();
    let source_writer = source_database(home.path());
    assert!(
        fs::metadata(home.path().join(format!("{SOURCE}-wal")))
            .unwrap()
            .len()
            > 0,
        "fence fixture needs nonempty committed WAL frames"
    );
    let (migrated, fence) = migrate_and_fence(home.path()).unwrap();
    assert!(migrated);
    let original_source_digest = read_marker(home.path()).unwrap().unwrap().source_digest;
    drop(fence);
    source_writer
        .execute_batch("PRAGMA wal_checkpoint(TRUNCATE)")
        .unwrap();
    drop(source_writer);
    assert_ne!(
        database_digest(&home.path().join(SOURCE)).unwrap(),
        original_source_digest,
        "checkpointing WAL should change source file bytes without changing its logical data"
    );

    let target = home.path().join(DATABASE);
    let mut store = crate::domain_store::DomainStore::open(&target).unwrap();
    let state = store.load().unwrap();
    let envelope = alondra_core::domain_v2::CommandEnvelope {
        protocol_version: 2,
        data_epoch: state.data_epoch,
        command_id: "post-migration-request".into(),
        expected_revision: state.revision,
        command: alondra_core::domain_v2::Command::CreateRequest {
            request_id: "request-after-migration".into(),
            body: "A normal durable post-migration write".into(),
            auto_merge: false,
        },
    };
    store
        .execute_authenticated(
            &alondra_core::domain_v2::Actor::Orchestrator { generation: 0 },
            &envelope,
            None,
        )
        .unwrap();
    drop(store);

    let (migrated, _fence) = migrate_and_fence(home.path()).unwrap();
    assert!(migrated);
    let mut restarted = crate::domain_store::DomainStore::open(&target).unwrap();
    let state = restarted.load().unwrap();
    assert!(
        state
            .requests
            .contains_key(&"request-after-migration".into())
    );
    assert!(state.revision > envelope.expected_revision);
    assert!(home.path().join(SOURCE).exists());
}

#[test]
fn source_fence_preserves_read_access_and_blocks_historical_writes() {
    let home = Home::new();
    let source_writer = source_database(home.path());
    let source_digest = database_digest(&home.path().join(SOURCE)).unwrap();
    let (migrated, fence) = migrate_and_fence(home.path()).unwrap();
    assert!(migrated);
    assert_eq!(
        database_digest(&home.path().join(SOURCE)).unwrap(),
        source_digest
    );

    let readonly =
        Connection::open_with_flags(home.path().join(SOURCE), OpenFlags::SQLITE_OPEN_READ_ONLY)
            .unwrap();
    assert!(
        readonly
            .query_row("SELECT 1", [], |row| row.get::<_, i64>(0))
            .is_ok()
    );
    drop(readonly);

    source_writer
        .busy_timeout(Duration::from_millis(50))
        .unwrap();
    assert!(source_writer.execute_batch("BEGIN IMMEDIATE").is_err());
    drop(fence);
    source_writer
        .execute_batch("BEGIN IMMEDIATE; ROLLBACK")
        .unwrap();
}

#[test]
fn interrupted_migration_resumes_at_each_durable_phase() {
    for failed_after in [
        Checkpoint::Prepared,
        Checkpoint::BackupPage,
        Checkpoint::BackupComplete,
        Checkpoint::Published,
        Checkpoint::Committed,
        Checkpoint::Cleaned,
    ] {
        let home = Home::new();
        let _source_writer = source_database(home.path());
        let result = migrate_locked(home.path(), |phase| {
            if phase == failed_after {
                anyhow::bail!("injected crash after {phase:?}");
            }
            Ok(())
        });
        assert!(
            result.is_err(),
            "phase {failed_after:?} must simulate interruption"
        );
        let interrupted_stage_id = if failed_after == Checkpoint::BackupPage {
            let marker = read_marker(home.path()).unwrap().unwrap();
            assert!(stage_path(home.path(), &marker.stage_id).exists());
            assert!(owner_path(&stage_path(home.path(), &marker.stage_id)).exists());
            Some(marker.stage_id)
        } else {
            None
        };
        assert!(
            migrate_locked(home.path(), |_| Ok(())).unwrap(),
            "phase {failed_after:?} must recover"
        );
        if let Some(stage_id) = interrupted_stage_id {
            let marker = read_marker(home.path()).unwrap().unwrap();
            assert_ne!(marker.stage_id, stage_id);
            assert!(interrupted_path(home.path(), &stage_id).exists());
            assert!(owner_path(&interrupted_path(home.path(), &stage_id)).exists());
        }
        assert_migrated(home.path());
    }
}

#[test]
fn migration_refuses_ambiguous_or_uncommitted_sources_without_touching_them() {
    let home = Home::new();
    let source = home.path().join(SOURCE);
    let _ = crate::domain_store::DomainStore::open(&source).unwrap();
    let target = home.path().join(DATABASE);
    let _ = crate::domain_store::DomainStore::open(&target).unwrap();
    assert!(migrate_locked(home.path(), |_| Ok(())).is_err());
    assert!(source.exists() && target.exists());

    let other = Home::new();
    let _ = crate::domain_store::DomainStore::open(&other.path().join(SOURCE)).unwrap();
    assert!(migrate_locked(other.path(), |_| Ok(())).is_err());
    assert!(other.path().join(SOURCE).exists());
    assert!(
        !migrate(other.path()).unwrap(),
        "a markerless legacy HOME is outside this migration"
    );
    assert!(other.path().join(SOURCE).exists());
}

#[test]
fn unknown_staging_file_is_preserved_and_never_overwritten() {
    let home = Home::new();
    let _source_writer = source_database(home.path());
    let source_digest = file_digest(&home.path().join(SOURCE));
    let stage = home.path().join(NEXT);
    fs::write(&stage, b"unrecognized private file").unwrap();
    fs::set_permissions(&stage, fs::Permissions::from_mode(0o600)).unwrap();

    assert!(migrate_locked(home.path(), |_| Ok(())).is_err());
    assert_eq!(fs::read(stage).unwrap(), b"unrecognized private file");
    assert_eq!(file_digest(&home.path().join(SOURCE)), source_digest);
    assert!(!home.path().join(DATABASE).exists());
}

#[test]
fn staging_database_without_matching_owner_record_is_preserved() {
    let home = Home::new();
    let _source_writer = source_database(home.path());
    let stopped = migrate_locked(home.path(), |phase| {
        if phase == Checkpoint::Prepared {
            anyhow::bail!("injected stop after marker");
        }
        Ok(())
    });
    assert!(stopped.is_err());

    let marker = read_marker(home.path()).unwrap().unwrap();
    let stage = stage_path(home.path(), &marker.stage_id);
    let unknown = b"preexisting unowned staging data";
    fs::write(&stage, unknown).unwrap();
    fs::set_permissions(&stage, fs::Permissions::from_mode(0o600)).unwrap();
    assert!(migrate_locked(home.path(), |_| Ok(())).is_err());
    assert_eq!(fs::read(stage).unwrap(), unknown);
    assert!(home.path().join(SOURCE).exists());
    assert!(!home.path().join(DATABASE).exists());
}

#[test]
fn staging_database_with_a_mismatched_owner_record_is_preserved() {
    let home = Home::new();
    let _source_writer = source_database(home.path());
    let stopped = migrate_locked(home.path(), |phase| {
        if phase == Checkpoint::Prepared {
            anyhow::bail!("injected stop after marker");
        }
        Ok(())
    });
    assert!(stopped.is_err());

    let marker = read_marker(home.path()).unwrap().unwrap();
    let stage = stage_path(home.path(), &marker.stage_id);
    backup_database(&home.path().join(SOURCE), &stage).unwrap();
    let owner = owner_path(&stage);
    fs::write(
        &owner,
        serde_json::to_vec(&StageOwner {
            stage_id: crate::auth::random_hex(12),
            source_digest: marker.source_digest.clone(),
            source_content_digest: marker.source_content_digest.clone(),
        })
        .unwrap(),
    )
    .unwrap();
    fs::set_permissions(&owner, fs::Permissions::from_mode(0o600)).unwrap();
    let stage_digest = file_digest(&stage);
    let owner_digest = file_digest(&owner);

    assert!(migrate_locked(home.path(), |_| Ok(())).is_err());
    assert_eq!(file_digest(&stage), stage_digest);
    assert_eq!(file_digest(&owner), owner_digest);
    assert!(home.path().join(SOURCE).exists());
    assert!(!home.path().join(DATABASE).exists());
}

#[test]
fn source_change_after_prepare_blocks_activation_and_preserves_both_files() {
    let home = Home::new();
    let source_writer = source_database(home.path());
    let result = migrate_locked(home.path(), |phase| {
        if phase == Checkpoint::Prepared {
            anyhow::bail!("injected stop after marker");
        }
        Ok(())
    });
    assert!(result.is_err());

    source_writer
        .execute(
            "UPDATE schedules SET next_run=4102444700000 WHERE id='schedule-keep'",
            [],
        )
        .unwrap();
    assert!(migrate_locked(home.path(), |_| Ok(())).is_err());
    assert!(home.path().join(SOURCE).exists());
    assert!(!home.path().join(DATABASE).exists());
    assert!(home.path().join(MARKER).exists());
}

#[test]
fn prepared_migration_resumes_after_sqlite_checkpoints_the_source_wal() {
    let home = Home::new();
    let source_writer = source_database(home.path());
    let stopped = migrate_locked(home.path(), |phase| {
        if phase == Checkpoint::Prepared {
            anyhow::bail!("injected stop after prepared marker");
        }
        Ok(())
    });
    assert!(stopped.is_err());
    let original_physical_digest = read_marker(home.path()).unwrap().unwrap().source_digest;
    source_writer
        .execute_batch("PRAGMA wal_checkpoint(TRUNCATE)")
        .unwrap();
    drop(source_writer);
    assert_ne!(
        database_digest(&home.path().join(SOURCE)).unwrap(),
        original_physical_digest,
        "the WAL checkpoint changes physical layout but not database content"
    );

    assert!(migrate_locked(home.path(), |_| Ok(())).unwrap());
    assert_migrated(home.path());
}

#[test]
fn prepared_marker_never_overwrites_an_existing_target() {
    let home = Home::new();
    let _source_writer = source_database(home.path());
    let stopped = migrate_locked(home.path(), |phase| {
        if phase == Checkpoint::Prepared {
            anyhow::bail!("injected stop after marker");
        }
        Ok(())
    });
    assert!(stopped.is_err());

    let target = home.path().join(DATABASE);
    let _ = crate::domain_store::DomainStore::open(&target).unwrap();
    let target_digest = file_digest(&target);
    let source_digest = file_digest(&home.path().join(SOURCE));
    assert!(migrate_locked(home.path(), |_| Ok(())).is_err());
    assert_eq!(file_digest(&target), target_digest);
    assert_eq!(file_digest(&home.path().join(SOURCE)), source_digest);
}

#[test]
fn prepared_target_with_matching_epoch_and_revision_but_different_rows_is_rejected() {
    let home = Home::new();
    let _source_writer = source_database(home.path());
    let stopped = migrate_locked(home.path(), |phase| {
        if phase == Checkpoint::Prepared {
            anyhow::bail!("injected stop after marker");
        }
        Ok(())
    });
    assert!(stopped.is_err());

    let source = home.path().join(SOURCE);
    let target = home.path().join(DATABASE);
    backup_database(&source, &target).unwrap();
    Connection::open(&target)
        .unwrap()
        .execute(
            "UPDATE schedules SET next_run=4102444700000 WHERE id='schedule-keep'",
            [],
        )
        .unwrap();
    let target_digest = file_digest(&target);
    let source_digest = file_digest(&source);
    assert_eq!(
        inspect_database(&source).unwrap(),
        inspect_database(&target).unwrap()
    );

    assert!(migrate_locked(home.path(), |_| Ok(())).is_err());
    assert_eq!(file_digest(&target), target_digest);
    assert_eq!(file_digest(&source), source_digest);
    assert_eq!(
        read_marker(home.path()).unwrap().unwrap().phase,
        Phase::Prepared
    );
}

#[test]
fn operational_entry_requires_exclusive_home_lock() {
    let home = Home::new();
    let _source_writer = source_database(home.path());
    let lock = alondra_core::home_lock::HomeLock::shared(home.path()).unwrap();
    assert!(migrate(home.path()).is_err());
    assert!(home.path().join(SOURCE).exists());
    drop(lock);
    assert!(migrate(home.path()).unwrap());
    assert_migrated(home.path());
}
