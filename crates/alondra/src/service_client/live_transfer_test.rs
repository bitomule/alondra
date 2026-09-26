//! Explicit live pilot gate; excluded from ordinary tests and release artifacts.
use super::*;

#[test]
#[ignore = "requires explicit live pilot authorization, existing terminal pairing and fixed relay capacity"]
fn live_twenty_mib_round_trip() -> Result<()> {
    anyhow::ensure!(
        std::env::var("ALONDRA_LIVE_TRANSFER").as_deref() == Ok("20MiB"),
        "set ALONDRA_LIVE_TRANSFER=20MiB only for an authorized live run"
    );
    let mac = std::env::var("ALONDRA_LIVE_TRANSFER_MAC")?;
    anyhow::ensure!(valid_id(&mac), "explicit Mac ID required");
    let home = crate::client::config().home.canonicalize()?;
    let config: toml::Value = toml::from_str(&fs::read_to_string(home.join("service.toml"))?)?;
    anyhow::ensure!(
        config.get("mac_id").and_then(|v| v.as_str()) == Some(mac.as_str()),
        "this gate only permits the local executor reached through the public relay"
    );
    // No interactive login, registration or pairing. The regular account path owns refresh.
    let owner = crate::account::token_for_service()?.sub;
    let profile = load()?
        .profiles
        .into_iter()
        .find(|p| p.mac_id == mac && p.owner_sub == owner)
        .context("approved terminal profile required")?;
    profile.validate()?;
    let suffix: String = random::<16>()?.iter().map(|b| format!("{b:02x}")).collect();
    let name = format!("alondra-live-transfer-{suffix}.bin");
    let bytes: Vec<u8> = (0..20 * 1024 * 1024)
        .map(|i| ((i * 31 + 19) % 251) as u8)
        .collect();
    let expected = Sha256::digest(&bytes);
    // Match the TUI's real upload deadline; do not widen it merely to pass the gate.
    let started = Instant::now();
    println!("live transfer: uploading 20971520 synthetic bytes through the public relay");
    let stored = profile.upload(&name, &bytes, Duration::from_secs(120))?;
    let upload_ms = started.elapsed().as_millis();
    let path = std::path::PathBuf::from(stored["path"].as_str().context("missing stored path")?);
    let canonical = path.canonicalize()?;
    let uploads = home.join("files/uploads").canonicalize()?;
    let parent = canonical.parent().context("missing upload directory")?;
    anyhow::ensure!(
        parent.parent() == Some(uploads.as_path())
            && canonical.file_name().and_then(|n| n.to_str()) == Some(name.as_str())
            && fs::metadata(&canonical)?.len() == bytes.len() as u64,
        "refusing unexpected upload path"
    );
    anyhow::ensure!(
        Sha256::digest(fs::read(&canonical)?) == expected,
        "executor file hash differs from synthetic input"
    );
    println!("live transfer: upload verified; downloading through the public relay");
    let result = (|| -> Result<u128> {
        let route = format!(
            "/v1/file?{}",
            url::form_urlencoded::Serializer::new(String::new())
                .append_pair("path", canonical.to_str().context("non-UTF8 upload path")?)
                .finish()
        );
        let started = Instant::now();
        let downloaded = profile.file(&route, Duration::from_secs(120))?;
        anyhow::ensure!(
            downloaded.len() == bytes.len() && Sha256::digest(&downloaded) == expected,
            "downloaded bytes/hash differ from synthetic input"
        );
        Ok(started.elapsed().as_millis())
    })();
    // Remove only the exact generated file, and only while its content is still ours.
    anyhow::ensure!(
        canonical.canonicalize()? == canonical && Sha256::digest(fs::read(&canonical)?) == expected,
        "cleanup identity changed"
    );
    fs::remove_file(&canonical)?;
    fs::remove_dir(parent)?; // Non-recursive: an unexpected extra file prevents deletion.
    let download_ms = result?;
    let sha: String = expected.iter().map(|b| format!("{b:02x}")).collect();
    println!(
        "{}",
        json!({"bytes":bytes.len(),"sha256":sha,"upload_ms":upload_ms,
        "download_ms":download_ms,"cleaned":true,"route":"public-relay-e2e"})
    );
    Ok(())
}
