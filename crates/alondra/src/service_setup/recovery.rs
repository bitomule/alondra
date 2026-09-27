//! Explicit local trust replacement. Never called as a response to an HTTP error.
//! The durable operation is also a daemon-start barrier until both local files
//! and the generation pointer are fsynced. Network uncertainty keeps the same ID.
use super::*;
use alondra_core::home_lock::{HomeLock, RECOVERY};
use serde_json::Value;
use std::{collections::BTreeSet, io::BufRead, path::PathBuf};

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Operation {
    version: u8,
    confirmed_at_ms: u64,
    local_uid: u32,
    id: String,
    phase: String,
    old_config: String,
    new_config: String,
    old_devices: Option<String>,
    new_devices: String,
    old_pending_pairing: Option<String>,
    revoked_device_ids: Vec<String>,
}

pub(super) fn valid_id(id: &str) -> bool {
    id.len() == 32 && id.bytes().all(|b| b.is_ascii_hexdigit())
}
fn read_optional(path: &Path) -> Result<Option<String>> {
    let mut options = OpenOptions::new();
    options.read(true).custom_flags(libc::O_NOFOLLOW);
    match options.open(path) {
        Ok(mut file) => {
            use std::io::Read;
            anyhow::ensure!(
                file.metadata()?.is_file(),
                "recovery input is not a regular file"
            );
            let mut text = String::new();
            file.read_to_string(&mut text)?;
            Ok(Some(text))
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error.into()),
    }
}
fn atomic(path: &Path, bytes: &[u8]) -> Result<()> {
    let parent = path.parent().context("missing recovery parent")?;
    if let Ok(meta) = fs::symlink_metadata(path) {
        anyhow::ensure!(
            meta.is_file() && !meta.file_type().is_symlink(),
            "unsafe recovery destination"
        );
    }
    let stage = parent.join(format!(
        ".recovery-write-{}",
        URL_SAFE_NO_PAD.encode(random::<16>()?)
    ));
    let result = (|| {
        let mut file = OpenOptions::new()
            .create_new(true)
            .write(true)
            .mode(0o600)
            .open(&stage)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        fs::rename(&stage, path)?;
        fs::File::open(parent)?.sync_all()?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&stage);
    }
    result
}
fn persist(home: &Path, op: &Operation) -> Result<()> {
    atomic(&home.join(RECOVERY), &serde_json::to_vec_pretty(op)?)
}
fn read_operation(home: &Path) -> Result<Option<Operation>> {
    let Some(text) = read_optional(&home.join(RECOVERY))? else {
        return Ok(None);
    };
    let op: Operation = serde_json::from_str(&text)?;
    anyhow::ensure!(
        op.version == 1
            && valid_id(&op.id)
            && ["prepared", "enrollment_attempted", "enrolled", "committed"]
                .contains(&op.phase.as_str()),
        "invalid recovery operation"
    );
    Ok(Some(op))
}
fn generation(home: &Path, id: &str) -> PathBuf {
    home.join("service-generations").join(id)
}
fn prepare(home: &Path, start_new_completed: bool) -> Result<Operation> {
    if let Some(op) = read_operation(home)? {
        if op.phase != "committed" || !start_new_completed {
            return Ok(op);
        }
    }
    let old_config =
        read_optional(&home.join(SERVICE_FILE))?.context("no existing service to recover")?;
    let old: ServiceConfig = toml::from_str(&old_config)?;
    let old_base = identity_base(home)?;
    let (old_identity, _) = load_or_create_keys(&old_base)?;
    anyhow::ensure!(
        old.mac_id == old_identity.mac_id && old.relay_url.trim_end_matches('/') == RELAY,
        "existing service identity or relay mismatch"
    );
    let id = random::<16>()?
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect::<String>();
    let parent = home.join("service-generations");
    fs::create_dir_all(&parent)?;
    anyhow::ensure!(
        !fs::symlink_metadata(&parent)?.file_type().is_symlink(),
        "generation directory must not be a symlink"
    );
    fs::set_permissions(&parent, fs::Permissions::from_mode(0o700))?;
    let base = generation(home, &id);
    fs::create_dir(&base)?;
    fs::set_permissions(&base, fs::Permissions::from_mode(0o700))?;
    let (new, _) = load_or_create_keys(&base)?;
    let prefix = format!("service-generations/{id}");
    let config = ServiceConfig {
        relay_url: old.relay_url.clone(),
        mac_id: new.mac_id,
        owner_sub: old.owner_sub.clone(),
        signing_key_path: format!("{prefix}/service-keys/{SIGNING_FILE}"),
        agreement_key_path: format!("{prefix}/service-keys/{AGREEMENT_FILE}"),
        journal_path: Some(format!("{prefix}/service-receipts.sqlite")),
        devices: vec![],
    };
    let mut revoked = BTreeSet::new();
    for device in &old.devices {
        revoked.insert(
            device
                .get("device_id")
                .and_then(toml::Value::as_str)
                .context("invalid old service device")?
                .to_owned(),
        );
    }
    let journal = old_base.join("service-receipts.sqlite");
    // Preserve the old journal byte-for-byte; include formerly removed bindings.
    if journal.try_exists()? {
        anyhow::ensure!(
            !fs::symlink_metadata(&journal)?.file_type().is_symlink(),
            "old journal must not be a symlink"
        );
        let connection = rusqlite::Connection::open_with_flags(
            &journal,
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
        )?;
        let mut query = connection.prepare("SELECT device_id FROM service_enrollments")?;
        for id in query.query_map([], |row| row.get::<_, String>(0))? {
            revoked.insert(id?);
        }
    }
    let old_pending_pairing = read_optional(&home.join("service-pairing-pending.json"))?;
    if let Some(pending) = &old_pending_pairing {
        let value: Value = serde_json::from_str(pending)?;
        anyhow::ensure!(
            value["mac_id"].as_str() == Some(old.mac_id.as_str()),
            "pending pairing belongs to another Mac"
        );
        revoked.insert(
            value["phone"]["device_id"]
                .as_str()
                .context("invalid pending device")?
                .to_owned(),
        );
    }
    let old_devices = read_optional(&home.join("devices.json"))?;
    let mut devices: Vec<Value> = serde_json::from_str(old_devices.as_deref().unwrap_or("[]"))?;
    for device in &devices {
        anyhow::ensure!(device["id"].is_string(), "invalid local approval registry");
    }
    devices.retain(|device| !revoked.contains(device["id"].as_str().unwrap()));
    let op = Operation {
        version: 1,
        confirmed_at_ms: SystemTime::now().duration_since(UNIX_EPOCH)?.as_millis() as u64,
        local_uid: unsafe { libc::getuid() },
        id,
        phase: "prepared".into(),
        old_config,
        new_config: toml::to_string_pretty(&config)?,
        old_devices,
        new_devices: serde_json::to_string_pretty(&devices)?,
        old_pending_pairing,
        revoked_device_ids: revoked.into_iter().collect(),
    };
    fs::File::open(&base)?.sync_all()?;
    fs::File::open(&parent)?.sync_all()?;
    persist(home, &op)?;
    Ok(op)
}

// Test seams are explicit arguments; production always supplies the fixed relay
// registrar and normal account provider. No environment origin/token overrides.
fn execute(
    home: &Path,
    owner: &str,
    mut op: Operation,
    register: &mut impl FnMut(&PublicMac, &SigningKey) -> Result<()>,
    checkpoint: &mut impl FnMut(&str) -> Result<()>,
) -> Result<Operation> {
    if op.phase == "committed" {
        return Ok(op);
    };
    let old: ServiceConfig = toml::from_str(&op.old_config)?;
    let new: ServiceConfig = toml::from_str(&op.new_config)?;
    anyhow::ensure!(
        old.owner_sub == owner && new.owner_sub == owner,
        "recovery cannot transfer this Mac to another account"
    );
    let current = read_optional(&home.join(SERVICE_FILE))?
        .context("missing service config during recovery")?;
    anyhow::ensure!(
        current == op.old_config || current == op.new_config,
        "service config changed outside recovery"
    );
    let devices = read_optional(&home.join("devices.json"))?;
    anyhow::ensure!(
        devices == op.old_devices || devices.as_deref() == Some(&op.new_devices),
        "local approvals changed outside recovery"
    );
    let pending = read_optional(&home.join("service-pairing-pending.json"))?;
    anyhow::ensure!(
        pending == op.old_pending_pairing || pending.is_none(),
        "pending pairing changed outside recovery"
    );
    let (identity, signing) = load_or_create_keys(&generation(home, &op.id))?;
    anyhow::ensure!(
        identity.mac_id == new.mac_id,
        "recovery generation identity mismatch"
    );
    checkpoint("prepared")?;
    if op.phase == "prepared" || op.phase == "enrollment_attempted" {
        op.phase = "enrollment_attempted".into();
        persist(home, &op)?;
        checkpoint("before_register")?;
        register(&identity, &signing)?;
        checkpoint("after_register")?;
        op.phase = "enrolled".into();
        persist(home, &op)?;
    }
    checkpoint("enrolled")?;
    atomic(&home.join("devices.json"), op.new_devices.as_bytes())?;
    checkpoint("approvals_removed")?;
    if pending.is_some() {
        fs::remove_file(home.join("service-pairing-pending.json"))?;
        fs::File::open(home)?.sync_all()?;
    }
    atomic(&home.join(SERVICE_FILE), op.new_config.as_bytes())?;
    checkpoint("config_published")?;
    op.phase = "committed".into();
    persist(home, &op)?;
    atomic(
        &generation(home, &op.id).join("recovery-audit.json"),
        &serde_json::to_vec_pretty(&op)?,
    )?;
    checkpoint("committed")?;
    Ok(op)
}

pub(super) fn interactive(
    home: &Path,
    stop: impl FnOnce() -> Result<()>,
    input: &mut impl BufRead,
    output: &mut impl Write,
) -> Result<RecoveryOutcome> {
    interactive_with(
        home,
        stop,
        input,
        output,
        || {
            let account = crate::account::ensure_token_for_enrollment()?;
            Ok((account.sub, account.access_token))
        },
        &mut |identity, signing, token| register(identity, signing, token),
    )
}

// Production passes its fixed account provider and registrar above. Tests inject
// these two boundaries to exercise confirmation/stop/resume without Keychain.
fn interactive_with(
    home: &Path,
    stop: impl FnOnce() -> Result<()>,
    input: &mut impl BufRead,
    output: &mut impl Write,
    account: impl FnOnce() -> Result<(String, String)>,
    registrar: &mut impl FnMut(&PublicMac, &SigningKey, &str) -> Result<()>,
) -> Result<RecoveryOutcome> {
    let existing = read_operation(home)?;
    if let Some(op) = &existing {
        if op.phase == "committed" {
            writeln!(
                output,
                "Previous service recovery {} is complete. Existing identity will be kept.",
                op.id
            )?;
            writeln!(
                output,
                "Start a DIFFERENT recovery and remove service client trust again? Type RECOVER to confirm; Enter keeps the current identity."
            )?;
        } else {
            writeln!(
                output,
                "Resume service recovery {}. The same new identity will be reused; old client approvals will be removed.",
                op.id
            )?;
            writeln!(output, "Type RECOVER to resume; Enter cancels.")?;
        }
    } else {
        let old: ServiceConfig = toml::from_str(
            &read_optional(&home.join(SERVICE_FILE))?.context("service is not configured")?,
        )?;
        writeln!(
            output,
            "Recover service identity for {}. Tasks and receipts stay on this Mac. The daemon will stop; all clients of this service need a NEW pairing. Other local approvals stay unchanged.",
            old.mac_id
        )?;
        writeln!(output, "Type RECOVER to confirm; Enter cancels.")?;
    }
    output.flush()?;
    let mut answer = String::new();
    input.read_line(&mut answer)?;
    if answer.trim() != "RECOVER" {
        writeln!(output, "No service identity or approval changed.")?;
        return Ok(RecoveryOutcome::Cancelled);
    }
    let (owner, token) = account()?;
    let before_text =
        read_optional(&home.join(SERVICE_FILE))?.context("service configuration disappeared")?;
    if let Some(op) = &existing {
        if op.phase != "committed" {
            anyhow::ensure!(
                before_text == op.old_config || before_text == op.new_config,
                "service configuration is outside the recorded recovery; retry after inspecting it"
            );
        }
    }
    let before: ServiceConfig = toml::from_str(&before_text)?;
    anyhow::ensure!(
        before.owner_sub == owner,
        "sign in with this Mac's existing account before recovery; account transfer is not allowed"
    );
    stop()?;
    let _lock = HomeLock::recovery(home)?;
    // Legacy binaries predate the lock. Never replace trust while their per-home
    // hook socket is still live. They must be stopped explicitly first.
    anyhow::ensure!(
        std::os::unix::net::UnixStream::connect(home.join("hooks.sock")).is_err(),
        "a legacy daemon still owns this HOME; stop it before recovery"
    );
    let current = read_operation(home)?;
    anyhow::ensure!(
        serde_json::to_value(&current)? == serde_json::to_value(&existing)?,
        "recovery changed while awaiting confirmation; retry"
    );
    let after_text =
        read_optional(&home.join(SERVICE_FILE))?.context("service configuration disappeared")?;
    anyhow::ensure!(
        after_text == before_text,
        "service configuration changed while stopping the daemon; retry"
    );
    let op = match current {
        Some(op) if op.phase != "committed" => op,
        _ => prepare(home, true)?,
    };
    let op = execute(
        home,
        &owner,
        op,
        &mut |identity, signing| registrar(identity, signing, &token),
        &mut |_| Ok(()),
    )?;
    writeln!(
        output,
        "Service recovery {} committed. Old clients remain unpaired. Run setup to start the daemon and create a new pairing.",
        op.id
    )?;
    Ok(RecoveryOutcome::Committed) // exclusive lock drops before setup can launch a daemon
}

#[cfg(test)]
mod tests {
    use super::*;
    struct Fixture(PathBuf);
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }
    fn fixture() -> Fixture {
        let home = std::env::temp_dir().join(format!(
            "alondra-recovery-{}",
            URL_SAFE_NO_PAD.encode(random::<16>().unwrap())
        ));
        fs::create_dir_all(&home).unwrap();
        let (identity, _) = load_or_create_keys(&home).unwrap();
        service_file(&home, &identity, "owner", true).unwrap();
        let mut config: ServiceConfig =
            toml::from_str(&fs::read_to_string(home.join(SERVICE_FILE)).unwrap()).unwrap();
        config.devices=vec![toml::Value::try_from(serde_json::json!({"device_id":"phone-old","signing_key":"public","agreement_key":"public"})).unwrap()];
        fs::write(home.join(SERVICE_FILE), toml::to_string(&config).unwrap()).unwrap();
        fs::write(home.join("devices.json"),serde_json::to_vec(&serde_json::json!([
            {"id":"phone-old","name":"Old service","public_key":"public","created_at":0},
            {"id":"client-removed","name":"Removed config","public_key":"public","created_at":0},
            {"id":"lan-keep","name":"Unrelated","public_key":"public","created_at":0}
        ])).unwrap()).unwrap();
        let db = rusqlite::Connection::open(home.join("service-receipts.sqlite")).unwrap();
        db.execute_batch("CREATE TABLE service_enrollments(device_id TEXT); INSERT INTO service_enrollments VALUES('client-removed'); CREATE TABLE service_receipts(sender TEXT,request_id TEXT,state TEXT,response BLOB); INSERT INTO service_receipts VALUES('phone-old','important','executing',NULL);").unwrap();
        fs::write(home.join("events.sqlite"), b"untouched task fixture").unwrap();
        fs::write(
            home.join("schedules.sqlite"),
            b"untouched schedules fixture",
        )
        .unwrap();
        Fixture(home)
    }
    fn assert_preserved(home: &Path) {
        assert_eq!(
            fs::read(home.join("events.sqlite")).unwrap(),
            b"untouched task fixture"
        );
        assert_eq!(
            fs::read(home.join("schedules.sqlite")).unwrap(),
            b"untouched schedules fixture"
        );
        let db = rusqlite::Connection::open_with_flags(
            home.join("service-receipts.sqlite"),
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
        )
        .unwrap();
        let state: String = db
            .query_row(
                "SELECT state FROM service_receipts WHERE request_id='important'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(state, "executing");
    }
    #[test]
    fn every_interruption_resumes_same_generation_without_old_bindings() {
        for phase in [
            "prepared",
            "before_register",
            "after_register",
            "enrolled",
            "approvals_removed",
            "config_published",
            "committed",
        ] {
            let fixture = fixture();
            let home = &fixture.0;
            let _lock = HomeLock::recovery(home).unwrap();
            let op = prepare(home, false).unwrap();
            let id = op.id.clone();
            let old: ServiceConfig = toml::from_str(&op.old_config).unwrap();
            let expected: ServiceConfig = toml::from_str(&op.new_config).unwrap();
            assert_ne!(old.mac_id, expected.mac_id);
            let mut calls = Vec::new();
            let mut registrar = |identity: &PublicMac, _: &SigningKey| {
                calls.push(identity.mac_id.clone());
                Ok(())
            };
            let result = execute(home, "owner", op, &mut registrar, &mut |point| {
                if point == phase {
                    bail!("injected interruption")
                }
                Ok(())
            });
            assert!(result.is_err());
            let pending = read_operation(home).unwrap().unwrap();
            assert_eq!(pending.id, id);
            if phase != "committed" {
                assert!(alondra_core::home_lock::ensure_ready(home).is_err());
            }
            let completed =
                execute(home, "owner", pending, &mut registrar, &mut |_| Ok(())).unwrap();
            assert_eq!(completed.phase, "committed");
            assert_eq!(completed.id, id);
            // Repeating an already-completed operation performs no network call.
            execute(
                home,
                "owner",
                completed,
                &mut |_, _| panic!("completed operation must not register again"),
                &mut |_| Ok(()),
            )
            .unwrap();
            assert!(calls.iter().all(|mac| mac == &expected.mac_id));
            if phase == "after_register" {
                assert_eq!(
                    calls.len(),
                    2,
                    "lost response retries the SAME signed identity"
                );
            }
            alondra_core::home_lock::ensure_ready(home).unwrap();
            let config: ServiceConfig =
                toml::from_str(&fs::read_to_string(home.join(SERVICE_FILE)).unwrap()).unwrap();
            assert_eq!(config.mac_id, expected.mac_id);
            assert!(config.devices.is_empty());
            assert!(config.journal_path.unwrap().contains(&id));
            let devices: Vec<Value> =
                serde_json::from_slice(&fs::read(home.join("devices.json")).unwrap()).unwrap();
            assert_eq!(devices.len(), 1);
            assert_eq!(devices[0]["id"], "lan-keep");
            let (reopened, _) = load_or_create_keys(&identity_base(home).unwrap()).unwrap();
            assert_eq!(reopened.mac_id, expected.mac_id);
            assert!(!service_file(home, &reopened, "owner", false).unwrap());
            assert_preserved(home);
        }
    }
    #[test]
    fn interactive_resumes_after_new_config_was_published() {
        let fixture = fixture();
        let home = &fixture.0;
        let op = {
            let _lock = HomeLock::recovery(home).unwrap();
            let op = prepare(home, false).unwrap();
            assert!(
                execute(
                    home,
                    "owner",
                    op.clone(),
                    &mut |_, _| Ok(()),
                    &mut |phase| {
                        if phase == "config_published" {
                            bail!("injected crash after new config")
                        }
                        Ok(())
                    }
                )
                .is_err()
            );
            op
        };
        assert_eq!(read_operation(home).unwrap().unwrap().phase, "enrolled");
        assert_eq!(
            read_optional(&home.join(SERVICE_FILE)).unwrap().unwrap(),
            op.new_config
        );
        let mut output = Vec::new();
        let mut stopped = false;
        let outcome = interactive_with(
            home,
            || {
                stopped = true;
                Ok(())
            },
            &mut &b"RECOVER\n"[..],
            &mut output,
            || Ok(("owner".into(), "synthetic-token".into())),
            &mut |_, _, _| panic!("enrolled recovery must not register again"),
        )
        .unwrap();
        assert_eq!(outcome, RecoveryOutcome::Committed);
        assert!(stopped);
        let completed = read_operation(home).unwrap().unwrap();
        assert_eq!(completed.id, op.id);
        assert_eq!(completed.phase, "committed");
        alondra_core::home_lock::ensure_ready(home).unwrap();
        // Declining a different operation is side-effect free, including login/stop.
        let outcome = interactive_with(
            home,
            || panic!("cancel must not stop daemon"),
            &mut &b"\n"[..],
            &mut output,
            || panic!("cancel must not read credentials"),
            &mut |_, _, _| panic!("cancel must not register"),
        )
        .unwrap();
        assert_eq!(outcome, RecoveryOutcome::Cancelled);
        assert_eq!(read_operation(home).unwrap().unwrap().id, op.id);
        assert_preserved(home);
    }

    #[test]
    fn duplicate_prepare_keeps_operation_and_generation() {
        let fixture = fixture();
        let home = &fixture.0;
        let _lock = HomeLock::recovery(home).unwrap();
        let first = prepare(home, false).unwrap();
        let second = prepare(home, false).unwrap();
        assert_eq!(first.id, second.id);
        assert_eq!(first.new_config, second.new_config);
        assert_eq!(
            fs::read_dir(home.join("service-generations"))
                .unwrap()
                .count(),
            1
        );
    }

    #[test]
    fn downtime_or_denial_never_commits_or_changes_approvals() {
        let fixture = fixture();
        let home = &fixture.0;
        let _lock = HomeLock::recovery(home).unwrap();
        let op = prepare(home, false).unwrap();
        let id = op.id.clone();
        let old = op.old_config.clone();
        let devices = op.old_devices.clone();
        assert!(
            execute(
                home,
                "other-owner",
                op.clone(),
                &mut |_, _| panic!("wrong owner must not contact relay"),
                &mut |_| Ok(())
            )
            .is_err()
        );
        assert!(
            execute(
                home,
                "owner",
                op,
                &mut |_, _| bail!("service unavailable or denied"),
                &mut |_| Ok(())
            )
            .is_err()
        );
        let pending = read_operation(home).unwrap().unwrap();
        assert_eq!(pending.id, id);
        assert_eq!(pending.phase, "enrollment_attempted");
        assert_eq!(read_optional(&home.join(SERVICE_FILE)).unwrap(), Some(old));
        assert_eq!(read_optional(&home.join("devices.json")).unwrap(), devices);
        assert!(alondra_core::home_lock::ensure_ready(home).is_err());
        assert_preserved(home);
    }
    #[test]
    fn external_changes_and_symlinks_fail_closed() {
        let fixture = fixture();
        let home = &fixture.0;
        let _lock = HomeLock::recovery(home).unwrap();
        let op = prepare(home, false).unwrap();
        fs::write(home.join("devices.json"), b"[]").unwrap();
        assert!(
            execute(
                home,
                "owner",
                op,
                &mut |_, _| panic!("changed approvals must block registration"),
                &mut |_| Ok(())
            )
            .is_err()
        );
        fs::remove_file(home.join(RECOVERY)).unwrap();
        std::os::unix::fs::symlink(home.join("devices.json"), home.join(RECOVERY)).unwrap();
        assert!(read_operation(home).is_err());
        assert!(alondra_core::home_lock::ensure_ready(home).is_err());
    }
    #[test]
    fn ordinary_enrollment_never_replaces_a_valid_existing_identity() {
        let fixture = fixture();
        let home = &fixture.0;
        let (before, _) = load_or_create_keys(home).unwrap();
        let (after, _) = load_or_create_keys(&identity_base(home).unwrap()).unwrap();
        assert_eq!(before.mac_id, after.mac_id);
        assert_eq!(before.signing_key, after.signing_key);
        assert_eq!(before.agreement_key, after.agreement_key);
        assert!(!home.join(RECOVERY).exists());
    }
}
