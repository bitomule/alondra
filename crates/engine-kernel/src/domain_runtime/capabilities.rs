//! Durable registry of issued producer capabilities. Only SHA-256 digests are
//! written; a bearer token exists in the producer's environment alone.
use crate::domain_authority::{Authority, Principal};
use alondra_domain_rules::{DomainState, NodeEnd};
use anyhow::{Context, Result, ensure};
use std::collections::BTreeSet;
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

/// The agents whose host may still run or be adopted: the current node of a task that is not over
/// and whose node Alondra did not close, a node the user kept, and the orchestrator's session.
/// Earlier nodes of a task, and nodes of tasks that ended, hold no host.
pub(super) fn live_agents(state: &DomainState) -> BTreeSet<String> {
    state.tasks.values()
        .filter(|task| match task.wrap_up.node {
            Some(NodeEnd::Closed | NodeEnd::Gone) => false,
            Some(NodeEnd::KeptByUser | NodeEnd::UsersOwn) => true,
            None => !task.state.terminal(),
        })
        .filter_map(|task| task.agent_id.as_ref().map(|agent| agent.0.clone()))
        .chain(state.orchestrator_session.as_ref().map(|agent| agent.0.clone()))
        .collect()
}

/// A missing file is an empty registry. A file from another epoch was fenced
/// by that epoch change, so its entries are dropped. An unreadable or unsafe
/// file stops startup instead of silently reissuing authority. Agents whose
/// host can no longer run lose their capability before the daemon serves.
pub(super) fn load(home: &Path, state: &DomainState) -> Result<Authority> {
    let epoch = state.data_epoch.0.clone();
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
    let mut authority = Authority::restore(epoch, entries);
    let batches: Vec<String> = authority.entries().filter_map(|(_, principal)| match principal {
        Principal::Orchestrator { execution_id, .. } => Some(execution_id.clone()),
        Principal::Agent { .. } => None,
    }).collect();
    let live = live_agents(state);
    let ended = authority.retain_agents(|agent| live.contains(agent));
    if !batches.is_empty() || ended {
        for execution in batches { authority.revoke_execution(&execution); }
        persist(home, &authority).context("retiring the capabilities of previous orchestrator batches and ended hosts before startup")?;
    }
    Ok(authority)
}

/// Written before any new token is exposed and after every revocation, so a
/// restart can neither resurrect a revoked bearer nor forget a live one.
pub(super) fn persist(home: &Path, authority: &Authority) -> Result<()> {
    alondra_daemon_engine_model::faults::write(home, "capability")?;
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
    let written = (|| {
        alondra_daemon_engine_model::faults::write(home, "capability_create")?;
        let mut file = OpenOptions::new().write(true).create_new(true).mode(0o600)
            .custom_flags(libc::O_NOFOLLOW).open(&next)?;
        alondra_daemon_engine_model::faults::write(home, "capability_write")?;
        file.write_all(&serde_json::to_vec(&registry)?)?;
        alondra_daemon_engine_model::faults::write(home, "capability_sync")?;
        file.sync_all()?;
        alondra_daemon_engine_model::faults::write(home, "capability_rename")?;
        std::fs::rename(&next, home.join(FILE))
    })();
    if written.is_err() {
        let _ = std::fs::remove_file(&next);
    }
    written.with_context(|| format!("writing {}", home.join(FILE).display()))?;
    alondra_daemon_engine_model::faults::write(home, "capability_directory_sync")?;
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
    use crate::locks::Poisonable as _;
    use alondra_domain_rules::*;

    struct Fixture(crate::test_support::Directory);
    impl Fixture {
        fn new() -> Self {
            let home = crate::test_support::Directory::new();
            std::fs::create_dir(&home).unwrap();
            Self(home)
        }
        fn open(&self) -> DomainRuntime {
            crate::domain_runtime::DomainRuntime::open_unwired(&self.0).unwrap()
        }
        /// Opens the domain with the registry on disk as it is, as a daemon that ran without
        /// pruning it at its start did.
        fn open_unpruned(&self) -> DomainRuntime {
            let saved = std::fs::read(self.0.join(FILE)).unwrap();
            let domain = self.open();
            std::fs::write(self.0.join(FILE), &saved).unwrap();
            let state = domain.state().unwrap();
            *domain.authority.lock_or_recover("the domain authority") = load_unpruned(&self.0, state.data_epoch.0.clone());
            domain
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn run(domain: &DomainRuntime, actor: Actor, command: Command) {
        let state = domain.state().unwrap();
        domain.execute(&actor, &CommandEnvelope {
            protocol_version: PROTOCOL_VERSION, data_epoch: state.data_epoch.clone(),
            command_id: format!("c-{}", state.revision).into(), expected_revision: state.revision, command,
        }, None, || Ok(())).unwrap();
    }

    /// An agent the domain knows that no task names: an earlier node of a task, or one never admitted.
    fn register_bare(domain: &DomainRuntime, agent: &str) {
        run(domain, Actor::System, Command::RegisterAgent { agent_id: agent.into(), name: agent.into(), at: 0 });
    }

    /// The current node of an open task of its own.
    fn register(domain: &DomainRuntime, agent: &str) {
        register_bare(domain, agent);
        let orchestrator = Actor::Orchestrator { generation: 0 };
        run(domain, orchestrator.clone(), Command::CreateRequest { request_id: format!("r-{agent}").into(), body: "Work".into(), auto_merge: false, maintenance: false, message: false });
        run(domain, orchestrator, Command::CreateTask { plan_items: None, continues: None,
            task_id: format!("task-{agent}").into(), request_id: format!("r-{agent}").into(), title: agent.into(), agent_id: Some(agent.into()), review_prs: false,
            dependencies: Default::default(), stack: None, launch: Some(LaunchPlan::new("node", "/tmp")),
        });
        run(domain, Actor::Agent { agent_id: agent.into() }, Command::StartTask { task_id: format!("task-{agent}").into() });
    }

    fn registry_agents(fixture: &Fixture) -> Vec<String> {
        let registry: serde_json::Value = serde_json::from_slice(&std::fs::read(fixture.0.join(FILE)).unwrap()).unwrap();
        let mut agents: Vec<String> = registry["entries"].as_array().unwrap().iter()
            .filter_map(|entry| entry["principal"]["session_id"].as_str().map(str::to_owned)).collect();
        agents.sort();
        agents
    }

    // Production, 17 November: the registry reached 4087 of 4096 capabilities, one for every host
    // that ever ran, because none was retired when its host ended and every restart loaded them
    // all again. Opening the domain keeps only the capabilities of nodes whose host may still run.
    #[test]
    fn opening_the_domain_retires_the_capabilities_of_nodes_whose_host_can_no_longer_run() {
        let fixture = Fixture::new();
        let domain = fixture.open();
        register(&domain, "working");
        register(&domain, "closed");
        register_bare(&domain, "replaced");
        let tokens: Vec<String> = ["working", "closed", "replaced"].iter()
            .map(|agent| domain.bind_agent(agent).unwrap().expose_for_child_environment().to_owned()).collect();
        run(&domain, Actor::System, Command::RecordNodeEnd { task_id: "task-closed".into(), end: NodeEnd::Closed, agent_id: Some("closed".into()) });
        assert_eq!(registry_agents(&fixture), ["closed", "replaced", "working"]);
        drop(domain);

        let reopened = fixture.open();
        assert_eq!(registry_agents(&fixture), ["working"], "the pruned registry is written before the daemon serves");
        assert!(reopened.resolve_bearer(&tokens[0]).is_ok(), "the node that may still work keeps its capability for its adoption");
        assert!(reopened.resolve_bearer(&tokens[1]).is_err());
        assert!(reopened.resolve_bearer(&tokens[2]).is_err());
    }

    fn load_unpruned(home: &Path, epoch: String) -> Authority {
        let registry: Registry = serde_json::from_slice(&std::fs::read(home.join(FILE)).unwrap()).unwrap();
        Authority::restore(epoch, registry.entries.into_iter().map(|entry| (decode_digest(&entry.digest).unwrap(), entry.principal)))
    }

    fn full_registry(fixture: &Fixture, agent: &str) {
        let domain = fixture.open();
        let state = domain.state().unwrap();
        drop(domain);
        let entries = (0..crate::domain_authority::CAPACITY).map(|index| {
            let mut digest = [0u8; 32];
            digest[..8].copy_from_slice(&(index as u64).to_be_bytes());
            (digest, Principal::Agent { session_id: agent.into(), task_binding: None, provider_agent_id: None })
        });
        persist(&fixture.0, &Authority::restore(state.data_epoch.0.clone(), entries)).unwrap();
    }

    fn open_capacity_cards(domain: &DomainRuntime) -> usize {
        domain.state().unwrap().interactions.values()
            .filter(|card| card.system_card == Some(SystemCard::CapabilityCapacity) && !card.withdrawn).count()
    }

    #[test]
    fn a_full_registry_retires_the_capabilities_of_ended_hosts_and_issues() {
        let fixture = Fixture::new();
        let domain = fixture.open();
        register(&domain, "working");
        register_bare(&domain, "ended");
        drop(domain);
        full_registry(&fixture, "ended");
        let domain = fixture.open_unpruned();
        let token = domain.bind_agent("working").expect("the ended hosts' capabilities make room").expose_for_child_environment().to_owned();
        assert!(domain.resolve_bearer(&token).is_ok());
        assert_eq!(registry_agents(&fixture), ["working"]);
        assert_eq!(open_capacity_cards(&domain), 0);
    }

    #[test]
    fn a_registry_full_of_working_nodes_refuses_with_its_own_code_and_one_card_until_one_ends() {
        let fixture = Fixture::new();
        let domain = fixture.open();
        register(&domain, "working");
        register(&domain, "next");
        drop(domain);
        full_registry(&fixture, "working");
        let domain = fixture.open();
        for _ in 0..3 {
            let refused = domain.bind_agent("next").err().expect("no room");
            assert!(matches!(&refused, crate::domain_store::StoreError::Domain(error) if error.code == alondra_domain_rules::capability_capacity_card::CAPABILITY_CAPACITY_CODE), "{refused}");
        }
        assert_eq!(open_capacity_cards(&domain), 1, "refused starts show one card, not one per try");
        domain.revoke_agent("working").unwrap();
        assert_eq!(open_capacity_cards(&domain), 0, "room again withdraws the card");
        assert!(domain.bind_agent("next").is_ok());
    }

    #[test]
    fn agent_adoption_capabilities_survive_but_orchestrator_batches_get_fresh_authority_after_restart() {
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
        assert!(reopened.resolve_bearer(&run).is_err(), "a native orchestrator batch never resumes across daemon restart");
        let fresh = reopened.bind_orchestrator("run-2").unwrap().expose_for_child_environment().to_owned();
        assert!(reopened.resolve_bearer(&fresh).is_ok());
        reopened.revoke_agent("session-a").unwrap();
        reopened.end_execution("run-2");
        assert!(reopened.resolve_bearer(&a).is_err());
        drop(reopened);

        let again = fixture.open();
        assert!(again.resolve_bearer(&a).is_err(), "revocation must survive restart");
        assert!(again.resolve_bearer(&run).is_err());
        assert!(again.resolve_bearer(&fresh).is_err());
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

    // Production, 25 October 07:03: the disk filled up while the daemon started, and binding two
    // nodes could not write the registry. A capability that could not be written was never handed
    // out, so it is not kept in memory either: the registry written next holds only the live ones.
    #[test]
    fn a_capability_the_full_disk_could_not_write_is_not_kept_in_memory() {
        let fixture = Fixture::new();
        let domain = fixture.open();
        register(&domain, "session-a");
        register(&domain, "session-b");
        let a = domain.bind_agent("session-a").unwrap().expose_for_child_environment().to_owned();
        let before = std::fs::read(fixture.0.join(FILE)).unwrap();
        domain.full_disk.store(true, std::sync::atomic::Ordering::SeqCst);
        let refused = domain.bind_agent("session-b").err().expect("a registry that cannot be written issues nothing");
        assert!(matches!(&refused, crate::domain_store::StoreError::Storage(error) if alondra_engine_ports::journal::no_space(error)), "{refused}");
        assert_eq!(std::fs::read(fixture.0.join(FILE)).unwrap(), before, "the registry before is whole");
        domain.full_disk.store(false, std::sync::atomic::Ordering::SeqCst);
        domain.revoke_agent("session-unknown").unwrap();
        let registry: serde_json::Value = serde_json::from_slice(&std::fs::read(fixture.0.join(FILE)).unwrap()).unwrap();
        assert_eq!(registry["entries"].as_array().unwrap().len(), 1, "only session-a's capability is live: {registry}");
        assert!(domain.resolve_bearer(&a).is_ok());
        assert!(!fixture.0.join(format!("{FILE}.next")).exists());
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
        assert!(crate::domain_runtime::DomainRuntime::open_unwired(&fixture.0).is_err(), "a corrupt registry stops startup");
        std::fs::write(&path, serde_json::to_vec(&registry).unwrap()).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert!(crate::domain_runtime::DomainRuntime::open_unwired(&fixture.0).is_err(), "a world-readable registry stops startup");
    }

    #[test]
    fn failed_revocation_persistence_cannot_resurrect_handled_or_limited_batch_bearers() {
        for settled in [false, true] {
            let fixture = Fixture::new(); let domain = fixture.open();
            register(&domain, "session-a");
            let agent = domain.bind_agent("session-a").unwrap().expose_for_child_environment().to_owned();
            let bearer = domain.bind_orchestrator("old-run").unwrap().expose_for_child_environment().to_owned();
            std::fs::create_dir(fixture.0.join(format!("{FILE}.next"))).unwrap();
            domain.end_execution("old-run");
            assert!(domain.resolve_bearer(&bearer).is_err(), "live authority is retired even when its write fails");
            if settled { domain.store.lock().unwrap().finish_batch("old-run").unwrap(); }
            drop(domain);
            assert!(crate::domain_runtime::DomainRuntime::open_unwired(&fixture.0).is_err(), "the daemon must not serve until loaded batch retirement is durable");
            std::fs::remove_dir(fixture.0.join(format!("{FILE}.next"))).unwrap();
            let reopened = fixture.open();
            assert!(reopened.resolve_bearer(&bearer).is_err(), "handled and quota-limited batches both need new native authority");
            assert!(reopened.resolve_bearer(&agent).is_ok(), "same-session node adoption authority survives");
            drop(reopened);
            assert!(fixture.open().resolve_bearer(&bearer).is_err(), "retirement was persisted before serving");
        }
    }

}
