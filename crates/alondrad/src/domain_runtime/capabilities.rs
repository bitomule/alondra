//! Durable registry of issued producer capabilities. Only SHA-256 digests are
//! written; a bearer token exists in the producer's environment alone.
use crate::domain_authority::{Authority, Principal};
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use std::fs::{File, OpenOptions};
use std::io::{Read, Write};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::Path;

const FILE: &str = "domain-capabilities.json";
const MAX_BYTES: u64 = 4 * 1024 * 1024;

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Registry {
    format: u32,
    epoch: String,
    entries: Vec<Entry>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Entry {
    digest: String,
    principal: Principal,
}

/// A missing file is an empty registry. A file from another epoch was fenced
/// by that epoch change, so its entries are dropped. An unreadable or unsafe
/// file stops startup instead of silently reissuing authority.
pub(super) fn load(home: &Path, epoch: String) -> Result<Authority> {
    let path = home.join(FILE);
    let file = match OpenOptions::new().read(true).custom_flags(libc::O_NOFOLLOW).open(&path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Authority::new(epoch)),
        Err(error) => return Err(error).with_context(|| format!("opening {}", path.display())),
    };
    let meta = file.metadata()?;
    ensure!(
        meta.is_file() && meta.nlink() == 1 && meta.uid() == unsafe { libc::geteuid() }
            && meta.permissions().mode() & 0o077 == 0,
        "unsafe domain capability registry {}", path.display()
    );
    let mut bytes = Vec::new();
    file.take(MAX_BYTES + 1).read_to_end(&mut bytes)?;
    ensure!(bytes.len() as u64 <= MAX_BYTES, "domain capability registry too large");
    let registry: Registry = serde_json::from_slice(&bytes)
        .with_context(|| format!("invalid domain capability registry {}", path.display()))?;
    ensure!(registry.format == 1, "unsupported domain capability registry format");
    if registry.epoch != epoch {
        return Ok(Authority::new(epoch));
    }
    let entries = registry.entries.into_iter().map(|entry| {
        let digest = decode_digest(&entry.digest)?;
        Ok((digest, entry.principal))
    }).collect::<Result<Vec<_>>>()?;
    Ok(Authority::restore(epoch, entries))
}

/// Written before any new token is exposed and after every revocation, so a
/// restart can neither resurrect a revoked bearer nor forget a live one.
pub(super) fn persist(home: &Path, authority: &Authority) -> Result<()> {
    let registry = Registry {
        format: 1,
        epoch: authority.epoch().to_owned(),
        entries: authority.entries().map(|(digest, principal)| Entry {
            digest: digest.iter().map(|byte| format!("{byte:02x}")).collect(),
            principal: principal.clone(),
        }).collect(),
    };
    let next = home.join(format!("{FILE}.next"));
    match std::fs::remove_file(&next) {
        Ok(()) => (),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => (),
        Err(error) => return Err(error).with_context(|| format!("clearing {}", next.display())),
    }
    let mut file = OpenOptions::new().write(true).create_new(true).mode(0o600)
        .custom_flags(libc::O_NOFOLLOW).open(&next)?;
    file.write_all(&serde_json::to_vec(&registry)?)?;
    file.sync_all()?;
    std::fs::rename(&next, home.join(FILE))?;
    File::open(home)?.sync_all()?;
    Ok(())
}

fn decode_digest(text: &str) -> Result<[u8; 32]> {
    ensure!(text.len() == 64, "invalid capability digest");
    let mut digest = [0u8; 32];
    for (index, byte) in digest.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&text[index * 2..index * 2 + 2], 16)
            .context("invalid capability digest")?;
    }
    Ok(digest)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain_runtime::DomainRuntime;
    use alondra_core::domain::*;

    struct Fixture(std::path::PathBuf);
    impl Fixture {
        fn new() -> Self {
            let home = std::env::temp_dir().join(format!("alondra-capabilities-{}", crate::auth::random_hex(8)));
            std::fs::create_dir(&home).unwrap();
            Self(home)
        }
        fn open(&self) -> DomainRuntime {
            DomainRuntime::open(&self.0).unwrap()
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn register(domain: &DomainRuntime, agent: &str) {
        let state = domain.state().unwrap();
        domain.execute(&Actor::System, &CommandEnvelope {
            protocol_version: PROTOCOL_VERSION, data_epoch: state.data_epoch.clone(),
            command_id: format!("register-{agent}").into(), expected_revision: state.revision,
            command: Command::RegisterAgent { agent_id: agent.into(), name: agent.into() },
        }, None, || Ok(())).unwrap();
    }

    #[test]
    fn issued_capabilities_survive_restart_and_revocation_is_durable() {
        let fixture = Fixture::new();
        let domain = fixture.open();
        register(&domain, "session-a");
        register(&domain, "session-b");
        let a = domain.bind_agent("session-a").unwrap().expose_for_child_environment().to_owned();
        let b = domain.bind_agent("session-b").unwrap().expose_for_child_environment().to_owned();
        let run = domain.bind_orchestrator("run-1").unwrap().expose_for_child_environment().to_owned();
        assert_eq!(std::fs::metadata(fixture.0.join(FILE)).unwrap().permissions().mode() & 0o777, 0o600);
        let registry = std::fs::read_to_string(fixture.0.join(FILE)).unwrap();
        for token in [&a, &b, &run] {
            assert!(!registry.contains(token.as_str()), "bearer token must never be written");
        }
        drop(domain);

        let reopened = fixture.open();
        assert!(matches!(reopened.resolve_bearer(&a).unwrap(), Principal::Agent { session_id, .. } if session_id == "session-a"));
        assert!(matches!(reopened.resolve_bearer(&run).unwrap(), Principal::Orchestrator { execution_id, generation: 0 } if execution_id == "run-1"));
        reopened.revoke_agent("session-a").unwrap();
        reopened.revoke_execution("run-1").unwrap();
        assert!(reopened.resolve_bearer(&a).is_err());
        drop(reopened);

        let again = fixture.open();
        assert!(again.resolve_bearer(&a).is_err(), "revocation must survive restart");
        assert!(again.resolve_bearer(&run).is_err());
        assert!(again.resolve_bearer(&b).is_ok(), "an unrelated capability is kept");
    }

    #[test]
    fn rebinding_a_session_fences_its_previous_bearer_and_needs_a_known_agent() {
        let fixture = Fixture::new();
        let domain = fixture.open();
        register(&domain, "session-a");
        let first = domain.bind_agent("session-a").unwrap().expose_for_child_environment().to_owned();
        let second = domain.bind_agent("session-a").unwrap().expose_for_child_environment().to_owned();
        assert!(domain.resolve_bearer(&first).is_err());
        assert!(domain.resolve_bearer(&second).is_ok());
        let unknown = domain.bind_agent("session-unknown").err().expect("unknown agent is refused");
        assert!(matches!(unknown, crate::domain_store::StoreError::Domain(error) if error.code == "not_found"));
        drop(domain);
        let reopened = fixture.open();
        assert!(reopened.resolve_bearer(&first).is_err());
        assert!(reopened.resolve_bearer(&second).is_ok());
    }

    #[test]
    fn a_registry_from_another_epoch_or_an_unsafe_file_is_not_trusted() {
        let fixture = Fixture::new();
        let domain = fixture.open();
        register(&domain, "session-a");
        let token = domain.bind_agent("session-a").unwrap().expose_for_child_environment().to_owned();
        drop(domain);
        let path = fixture.0.join(FILE);
        let mut registry: serde_json::Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        registry["epoch"] = serde_json::Value::String("f".repeat(64));
        std::fs::write(&path, serde_json::to_vec(&registry).unwrap()).unwrap();
        let reopened = fixture.open();
        assert!(reopened.resolve_bearer(&token).is_err(), "another epoch's entries are dropped");
        drop(reopened);

        std::fs::write(&path, b"not json").unwrap();
        assert!(DomainRuntime::open(&fixture.0).is_err(), "a corrupt registry stops startup");
        std::fs::write(&path, serde_json::to_vec(&registry).unwrap()).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert!(DomainRuntime::open(&fixture.0).is_err(), "a world-readable registry stops startup");
    }
}
