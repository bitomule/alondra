//! Opt-in native-core integration; compiled only into the macOS test binary.
//! The runner supplies synthetic credentials over stdin. Never use default Keychain APIs here.
use super::*;
use base64::Engine as _;
use security_framework::os::macos::{
    keychain::{CreateOptions, SecKeychain},
    passwords::find_generic_password,
};
use std::{os::unix::fs::MetadataExt, path::PathBuf};

const SERVICE: &str = "dev.davidcollado.alondra.native-core-smoke";

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Fixture {
    phase: String,
    directory: PathBuf,
    keychain_password: String,
    realm: String,
    sub: String,
    refresh_token: String,
    port: u16,
}

impl Fixture {
    fn validate(&self) -> Result<()> {
        let suffix = self
            .realm
            .strip_prefix("alondra-session-smoke-")
            .context("fixture realm required")?;
        anyhow::ensure!(
            suffix.len() == 32 && suffix.bytes().all(|b| b.is_ascii_hexdigit()),
            "invalid fixture realm"
        );
        anyhow::ensure!(
            self.port != 0 && !self.sub.is_empty() && !self.refresh_token.is_empty(),
            "invalid fixture"
        );
        // This is an accidental-fixture guard, not JWT signature validation. The SSH
        // fixture obtains and owns the real grant; production credentials are never inputs.
        let parts: Vec<_> = self.refresh_token.split('.').collect();
        anyhow::ensure!(parts.len() == 3, "fixture token shape invalid");
        let claims: serde_json::Value = serde_json::from_slice(&URL_SAFE_NO_PAD.decode(parts[1])?)?;
        anyhow::ensure!(
            claims["iss"].as_str() == Some(self.issuer().as_str())
                && claims["sub"].as_str() == Some(self.sub.as_str())
                && claims["azp"].as_str() == Some(CLIENT_ID),
            "fixture token binding differs"
        );
        anyhow::ensure!(
            self.keychain_password.len() >= 32,
            "fixture password required"
        );
        let path = std::fs::canonicalize(&self.directory)?;
        anyhow::ensure!(
            path == self.directory && path.parent() == Some(std::path::Path::new("/private/tmp")),
            "unsafe fixture directory"
        );
        anyhow::ensure!(
            path.file_name()
                .and_then(|s| s.to_str())
                .is_some_and(|s| s.starts_with("alondra-native-core-")),
            "unsafe fixture directory"
        );
        let meta = std::fs::symlink_metadata(&path)?;
        anyhow::ensure!(
            meta.is_dir() && meta.uid() == unsafe { libc::getuid() } && meta.mode() & 0o077 == 0,
            "unsafe fixture permissions"
        );
        anyhow::ensure!(
            matches!(self.phase.as_str(), "offline" | "retry"),
            "invalid fixture phase"
        );
        Ok(())
    }

    fn issuer(&self) -> String {
        format!(
            "https://auth.getalondra.com/realms/{}",
            self.realm
        )
    }

    fn read(&self, keychain: &SecKeychain) -> Result<Session> {
        let (bytes, _) =
            find_generic_password(Some(std::slice::from_ref(keychain)), SERVICE, &self.realm)?;
        let session: Session = serde_json::from_slice(bytes.as_ref())?;
        anyhow::ensure!(
            session.issuer == self.issuer()
                && session.client_id == CLIENT_ID
                && session.sub == self.sub
                && session.refresh_token == self.refresh_token,
            "unexpected fixture session"
        );
        Ok(session)
    }

    fn run(&self) -> Result<()> {
        self.validate()?;
        // Fail before creating anything if proxy-based routing could leave the fixture.
        anyhow::ensure!(
            !std::env::vars_os().any(|(name, _)| name
                .to_string_lossy()
                .to_ascii_lowercase()
                .ends_with("_proxy")),
            "proxy environment forbidden"
        );
        let _no_ui = SecKeychain::disable_user_interaction()?;
        let path = self.directory.join("native-smoke.keychain");
        let keychain = if self.phase == "offline" {
            anyhow::ensure!(!path.exists(), "fixture keychain already exists");
            CreateOptions::new()
                .password(&self.keychain_password)
                .prompt_user(false)
                .create(&path)?
        } else {
            let meta = std::fs::symlink_metadata(&path)?;
            anyhow::ensure!(
                meta.is_file()
                    && !meta.file_type().is_symlink()
                    && meta.uid() == unsafe { libc::getuid() },
                "unsafe fixture keychain"
            );
            let mut keychain = SecKeychain::open(&path)?;
            keychain.unlock(Some(&self.keychain_password))?;
            keychain
        };
        if self.phase == "offline" {
            let active = Session {
                issuer: self.issuer(),
                client_id: CLIENT_ID.into(),
                sub: self.sub.clone(),
                refresh_token: self.refresh_token.clone(),
                state: SessionState::Active,
            };
            keychain.add_generic_password(SERVICE, &self.realm, &serde_json::to_vec(&active)?)?;
            let cache = (
                serde_json::to_vec(&active)?,
                AccountToken {
                    sub: self.sub.clone(),
                    access_token: "synthetic-cache-sentinel".into(),
                    valid_for: Duration::from_secs(300),
                },
                Instant::now(),
            );
            anyhow::ensure!(
                cached_for_session(Some(&cache), &active)?.is_some(),
                "active cache fixture failed"
            );
            let completed = logout_transition(
                active,
                |pending| {
                    let (_, mut item) = find_generic_password(
                        Some(std::slice::from_ref(&keychain)),
                        SERVICE,
                        &self.realm,
                    )?;
                    item.set_password(&serde_json::to_vec(pending)?)?;
                    Ok(())
                },
                |_| bail!("injected offline failure"),
                || bail!("offline transition must retain credential"),
            )?;
            anyhow::ensure!(!completed, "offline transition unexpectedly completed");
            let pending = self.read(&keychain)?;
            anyhow::ensure!(
                pending.state == SessionState::PendingRevocation
                    && require_active(&pending).is_err()
                    && cached_for_session(Some(&cache), &pending).is_err(),
                "pending session usable"
            );
        } else {
            let pending = self.read(&keychain)?;
            anyhow::ensure!(
                pending.state == SessionState::PendingRevocation
                    && require_active(&pending).is_err(),
                "pending state did not survive process exit"
            );
            // The test endpoint is constructed, never accepted as a caller-provided URL.
            // Production discovery remains HTTPS-only; it is deliberately not exercised here.
            let endpoint = format!(
                "http://127.0.0.1:{}/realms/{}/protocol/openid-connect/revoke",
                self.port, self.realm
            );
            let discovery = Discovery {
                issuer: self.issuer(),
                authorization_endpoint: String::new(),
                token_endpoint: String::new(),
                userinfo_endpoint: String::new(),
                revocation_endpoint: Some(endpoint),
            };
            let completed = logout_transition(
                pending,
                |_| bail!("pending state must not be rewritten"),
                |session| revoke_token(&agent(), &discovery, &session.refresh_token),
                || {
                    let (_, item) = find_generic_password(
                        Some(std::slice::from_ref(&keychain)),
                        SERVICE,
                        &self.realm,
                    )?;
                    item.delete();
                    match find_generic_password(
                        Some(std::slice::from_ref(&keychain)),
                        SERVICE,
                        &self.realm,
                    ) {
                        Err(error) if error.code() == -25300 => Ok(()),
                        _ => bail!("fixture credential deletion not verified"),
                    }
                },
            )?;
            anyhow::ensure!(completed, "real revocation failed");
        }
        Ok(())
    }
}

#[test]
#[ignore = "Requires explicit native-core smoke runner, disposable Pi realm and temporary Keychain"]
fn offline_logout_then_real_revoke() {
    // No unwrap/Debug of parsed inputs or HTTP errors: the harness must never print tokens.
    let result = (|| -> Result<()> {
        anyhow::ensure!(
            std::env::var("ALONDRA_NATIVE_CORE_SMOKE").as_deref() == Ok("1"),
            "opt-in required"
        );
        let mut bytes = Vec::new();
        std::io::stdin()
            .take(64 * 1024 + 1)
            .read_to_end(&mut bytes)?;
        anyhow::ensure!(bytes.len() <= 64 * 1024, "fixture input exceeded bound");
        let fixture: Fixture = serde_json::from_slice(&bytes)?;
        fixture.run()
    })();
    assert!(
        result.is_ok(),
        "native-core smoke failed; fixture details withheld"
    );
    println!("ALONDRA_NATIVE_CORE_PASS");
}
