//! Explicit two-phase real-service test; never compiled into the distributed CLI.
//! Runs the actual CLI recovery orchestration and signed registrar. OIDC tokens
//! are supplied by the owned Keycloak fixture; Keychain/launchd are not exercised.
use super::*;
use std::os::unix::fs::MetadataExt;
const LOCAL_CONFIG_ORIGIN: &str = "http://127.0.0.1:1";

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Fixture {
    scope: String,
    project: String,
    phase: String,
    relay: String,
    owner: String,
    token: String,
}
fn fixture() -> Result<(Fixture, PathBuf)> {
    let path = PathBuf::from(
        std::env::var("ALONDRA_RECOVERY_CLI_FIXTURE").context("owned fixture file required")?,
    );
    anyhow::ensure!(
        path.file_name().is_some_and(|name| name == "fixture.json"),
        "unexpected fixture filename"
    );
    let dir = path.parent().context("fixture parent")?;
    let meta = fs::symlink_metadata(dir)?;
    anyhow::ensure!(
        meta.is_dir()
            && !meta.file_type().is_symlink()
            && meta.mode() & 0o077 == 0
            && meta.uid() == unsafe { libc::getuid() },
        "fixture directory ownership invalid"
    );
    let file = fs::symlink_metadata(&path)?;
    anyhow::ensure!(
        file.is_file()
            && !file.file_type().is_symlink()
            && file.mode() & 0o077 == 0
            && file.uid() == meta.uid()
            && file.len() < 65536,
        "fixture file ownership invalid"
    );
    let value: Fixture = serde_json::from_str(&read_optional(&path)?.context("fixture absent")?)?;
    anyhow::ensure!(
        value.scope == "alondra-cli-recovery-v1"
            && value.project.starts_with("alondra-system-")
            && value.project.len() == 27
            && value.project[15..].bytes().all(|b| b.is_ascii_hexdigit()),
        "fixture scope invalid"
    );
    let marker: Value = serde_json::from_str(
        &read_optional(&dir.join("marker.json"))?.context("fixture marker absent")?,
    )?;
    anyhow::ensure!(
        marker == serde_json::json!({"scope":"alondra-cli-recovery-v1","project":value.project}),
        "fixture marker mismatch"
    );
    let relay = url::Url::parse(&value.relay)?;
    anyhow::ensure!(
        relay.scheme() == "http"
            && relay.host_str() == Some("127.0.0.1")
            && relay.port().is_some_and(|port| port >= 1024)
            && relay.username().is_empty()
            && relay.password().is_none()
            && relay.path() == "/"
            && relay.query().is_none()
            && relay.fragment().is_none(),
        "relay must be owned loopback fixture"
    );
    anyhow::ensure!(
        !value.owner.is_empty() && !value.token.is_empty(),
        "synthetic auth missing"
    );
    Ok((value, dir.to_owned()))
}
fn macs(fixture: &Fixture) -> Result<Value> {
    let mut response = http()
        .get(format!("{}/v1/macs", fixture.relay.trim_end_matches('/')))
        .header("authorization", format!("Bearer {}", fixture.token))
        .call()?;
    anyhow::ensure!(
        response.status().as_u16() == 200,
        "fixture account directory request failed"
    );
    Ok(response.body_mut().with_config().limit(65536).read_json()?)
}
fn signed(
    fixture: &Fixture,
    identity: &PublicMac,
    key: &SigningKey,
    method: &str,
    path: &str,
    body: &[u8],
) -> Result<u16> {
    let relay = fixture.relay.trim_end_matches('/');
    let mut response = http().get(format!("{relay}/v1/bootstrap")).call()?;
    anyhow::ensure!(
        response.status().as_u16() == 200,
        "fixture bootstrap failed"
    );
    let bootstrap: Bootstrap = response.body_mut().with_config().limit(4096).read_json()?;
    let epoch: [u8; 16] = URL_SAFE_NO_PAD
        .decode(bootstrap.epoch)?
        .try_into()
        .map_err(|_| anyhow!("fixture epoch invalid"))?;
    let at = SystemTime::now().duration_since(UNIX_EPOCH)?.as_millis() as u64;
    let nonce = random::<16>()?;
    let signature = sign_http_challenge(
        key,
        &identity.mac_id,
        method,
        path,
        &epoch,
        body,
        at,
        &nonce,
    )?;
    // This helper only exercises signed Mac routes. The actual enrollment uses
    // service_setup::register_at, not an independent registration implementation.
    anyhow::ensure!(method == "POST", "unsupported fixture method");
    let response = http()
        .post(format!("{relay}{path}"))
        .header("content-type", "application/json")
        .header("x-device-id", &identity.mac_id)
        .header("x-relay-epoch", URL_SAFE_NO_PAD.encode(epoch))
        .header("x-device-timestamp-ms", at.to_string())
        .header("x-device-nonce", URL_SAFE_NO_PAD.encode(nonce))
        .header("x-device-signature", signature)
        .send(body)?;
    Ok(response.status().as_u16())
}
#[test]
#[ignore = "requires owned two-phase restored Docker backend fixture; no personal credentials"]
fn cli_recovery_against_restored_service() -> Result<()> {
    let (fixture, dir) = fixture()?;
    let home = dir.join("home");
    let relay = fixture.relay.trim_end_matches('/');
    if fixture.phase == "prepare" {
        anyhow::ensure!(!home.exists(), "fresh fixture HOME required");
        fs::create_dir(&home)?;
        fs::set_permissions(&home, fs::Permissions::from_mode(0o700))?;
        let _lock = HomeLock::shared(&home)?;
        let (identity, key) = load_or_create_keys(&home)?;
        register_at(&identity, &key, &fixture.token, relay)?;
        service_file_at(&home, &identity, &fixture.owner, true, LOCAL_CONFIG_ORIGIN)?;
        let client = format!("phone-recovery-{}", URL_SAFE_NO_PAD.encode(random::<16>()?));
        let device = serde_json::json!({"device_id":client,"owner_sub":fixture.owner,"signing_key":identity.signing_key});
        assert_eq!(
            signed(
                &fixture,
                &identity,
                &key,
                "POST",
                "/v1/devices",
                &serde_json::to_vec(&device)?
            )?,
            201
        );
        let mut config: ServiceConfig =
            toml::from_str(&fs::read_to_string(home.join(SERVICE_FILE))?)?;
        config.devices = vec![toml::Value::try_from(
            serde_json::json!({"device_id":client,"signing_key":identity.signing_key,"agreement_key":identity.agreement_key}),
        )?];
        atomic(
            &home.join(SERVICE_FILE),
            toml::to_string_pretty(&config)?.as_bytes(),
        )?;
        atomic(
            &home.join("devices.json"),
            &serde_json::to_vec(&serde_json::json!([
            {"id":client,"name":"Synthetic service approval","public_key":identity.signing_key,"created_at":0},
            {"id":"local-fixture-only","name":"Synthetic local approval","public_key":"preserved","created_at":0}]))?,
        )?;
        let db = rusqlite::Connection::open(home.join("service-receipts.sqlite"))?;
        db.execute_batch("CREATE TABLE service_enrollments(device_id TEXT); CREATE TABLE service_receipts(request_id TEXT,state TEXT); INSERT INTO service_receipts VALUES('preserved-request','executing');")?;
        db.execute("INSERT INTO service_enrollments VALUES(?1)", [&client])?;
        atomic(&home.join("tasks-evidence"), b"preserved local tasks")?;
        atomic(
            &dir.join("public-state.json"),
            &serde_json::to_vec(
                &serde_json::json!({"old_mac":identity.mac_id,"old_client":client}),
            )?,
        )?;
        return Ok(());
    }
    anyhow::ensure!(fixture.phase == "resume", "unknown fixture phase");
    let old: Value = serde_json::from_str(
        &read_optional(&dir.join("public-state.json"))?.context("prepare phase missing")?,
    )?;
    let (old_identity, old_key) = load_or_create_keys(&home)?;
    assert_eq!(old["old_mac"], old_identity.mac_id);
    let rejected = register_at(&old_identity, &old_key, &fixture.token, relay)
        .expect_err("quarantined identity must not re-enroll");
    assert!(rejected.to_string().contains("explicit recovery is required"),
        "old identity must receive enrollment conflict, not an unrelated auth or network error");
    let before = macs(&fixture)?;
    let mut output = Vec::new();
    let outcome = interactive_with(
        &home,
        "Mac Studio",
        || Ok(()),
        &mut &b"Mac Studio\n"[..],
        &mut output,
        LOCAL_CONFIG_ORIGIN,
        || Ok((fixture.owner.clone(), fixture.token.clone())),
        &mut |identity, key, token| register_at(identity, key, token, relay),
    )?;
    assert_eq!(outcome, RecoveryOutcome::Committed);
    let config: ServiceConfig = toml::from_str(&fs::read_to_string(home.join(SERVICE_FILE))?)?;
    let (new_identity, new_key) = load_or_create_keys(&identity_base(&home)?)?;
    assert_ne!(new_identity.mac_id, old_identity.mac_id);
    assert!(config.devices.is_empty());
    assert_ne!(new_identity.signing_key, old_identity.signing_key);
    assert_ne!(new_identity.agreement_key, old_identity.agreement_key);
    assert!(config.journal_path.is_some());
    let after = macs(&fixture)?;
    assert_eq!(
        after["macs"].as_array().context("directory shape")?.len(),
        before["macs"].as_array().context("directory shape")?.len() + 1
    );
    assert!(
        after["macs"]
            .as_array()
            .unwrap()
            .iter()
            .any(|v| v["mac_id"] == new_identity.mac_id)
    );
    // The signed registrar itself is idempotent; a new key does not revive IDs.
    register_at(&new_identity, &new_key, &fixture.token, relay)?;
    assert_eq!(macs(&fixture)?, after);
    let old_client = serde_json::json!({"device_id":old["old_client"],"owner_sub":fixture.owner,"signing_key":old_identity.signing_key});
    assert_eq!(
        signed(
            &fixture,
            &new_identity,
            &new_key,
            "POST",
            "/v1/devices",
            &serde_json::to_vec(&old_client)?
        )?,
        409
    );
    let rejected = register_at(&old_identity, &old_key, &fixture.token, relay)
        .expect_err("old identity must remain quarantined after fresh enrollment");
    assert!(rejected.to_string().contains("explicit recovery is required"),
        "old identity must receive enrollment conflict, not an unrelated auth or network error");
    let approvals: Vec<Value> =
        serde_json::from_str(&fs::read_to_string(home.join("devices.json"))?)?;
    assert_eq!(approvals.len(), 1);
    assert_eq!(approvals[0]["id"], "local-fixture-only");
    assert_eq!(
        fs::read(home.join("tasks-evidence"))?,
        b"preserved local tasks"
    );
    let db = rusqlite::Connection::open_with_flags(
        home.join("service-receipts.sqlite"),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )?;
    let state: String = db.query_row(
        "SELECT state FROM service_receipts WHERE request_id='preserved-request'",
        [],
        |r| r.get(0),
    )?;
    assert_eq!(state, "executing");
    assert_eq!(
        interactive_with(
            &home,
            "Mac Studio",
            || panic!("cancel must not stop"),
            &mut &b"\n"[..],
            &mut output,
            LOCAL_CONFIG_ORIGIN,
            || panic!("cancel must not authenticate"),
            &mut |_, _, _| panic!("cancel must not register")
        )?,
        RecoveryOutcome::Cancelled
    );
    assert_eq!(macs(&fixture)?, after);
    atomic(
        &dir.join("public-result.json"),
        &serde_json::to_vec(
            &serde_json::json!({"status":"passed","old_mac":old_identity.mac_id,"new_mac":new_identity.mac_id,"old_client":old["old_client"],"new_bindings":0}),
        )?,
    )?;
    Ok(())
}
