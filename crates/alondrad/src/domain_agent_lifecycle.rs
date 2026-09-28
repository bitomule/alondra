//! Trusted lifecycle writes for Claude sessions managed by this daemon.
//!
//! `AgentId` identifies one managed instance; the Claude conversation UUID is
//! separate. These functions must only be called from the daemon's launch path
//! and its process callbacks. MCP
//! input, environment variables and a PID supplied by a client are not evidence
//! that an Agent exists.

use crate::domain_runtime::DomainRuntime;
use crate::domain_store::StoreError;
use alondra_core::domain_v2::{
    Actor, AgentId, Command, CommandEnvelope, CommandId, CommandOutcome, DataEpoch, DomainError,
    Revision, PROTOCOL_VERSION,
};
use sha2::{Digest, Sha256};

pub(crate) struct ManagedIdentity {
    pub(crate) agent_id: AgentId,
    pub(crate) claude_session_id: String,
}

/// Keep this cursor unchanged when retrying an uncertain write. The receipt
/// hashes the entire envelope, including its expected revision.
pub(crate) struct LifecycleEvent {
    event_id: String,
    data_epoch: DataEpoch,
    expected_revision: Revision,
}

pub(crate) fn prepare_event(
    domain: &DomainRuntime,
    event_id: &str,
) -> Result<LifecycleEvent, StoreError> {
    if event_id.is_empty() {
        return Err(DomainError::new("invalid_input", "lifecycle event ID is required").into());
    }
    let state = domain.state().map_err(StoreError::Storage)?;
    Ok(LifecycleEvent {
        event_id: event_id.to_owned(),
        data_epoch: state.data_epoch,
        expected_revision: state.revision,
    })
}

/// Allocate distinct random IDs for one Agent instance and its Claude conversation.
/// The launch path persists the technical Claude session before assigning work.
pub(crate) fn new_identity() -> Result<ManagedIdentity, StoreError> {
    Ok(ManagedIdentity {
        agent_id: AgentId(format!(
            "agent-{}",
            crate::orchestrator_process::uuid().map_err(StoreError::Storage)?
        )),
        claude_session_id: crate::orchestrator_process::uuid().map_err(StoreError::Storage)?,
    })
}

/// Register a daemon-launched managed Agent before it is assigned work.
///
/// The caller must reuse the whole event after an uncertain response.
pub(crate) fn register_instance(
    domain: &DomainRuntime,
    identity: &ManagedIdentity,
    name: &str,
    event: &LifecycleEvent,
) -> Result<CommandOutcome, StoreError> {
    submit(
        domain,
        "agent-register",
        event,
        Command::RegisterAgent {
            agent_id: identity.agent_id.clone(),
            name: name.to_owned(),
        },
    )
}

/// Record a transition observed by the daemon's managed process supervisor.
///
/// Each distinct spawn or exit needs a distinct event. Reuse the whole event
/// and payload only when retrying an uncertain commit.
pub(crate) fn observe_instance(
    domain: &DomainRuntime,
    agent_id: &AgentId,
    connected: bool,
    event: &LifecycleEvent,
) -> Result<CommandOutcome, StoreError> {
    submit(
        domain,
        "agent-observe",
        event,
        Command::ObserveAgent {
            agent_id: agent_id.clone(),
            connected,
        },
    )
}

fn submit(
    domain: &DomainRuntime,
    kind: &str,
    event: &LifecycleEvent,
    command: Command,
) -> Result<CommandOutcome, StoreError> {
    let digest = Sha256::digest(event.event_id.as_bytes());
    let hexadecimal: String = digest.iter().map(|byte| format!("{byte:02x}")).collect();
    let command_id = CommandId(format!("{kind}-{hexadecimal}"));
    let envelope = CommandEnvelope {
        protocol_version: PROTOCOL_VERSION,
        data_epoch: event.data_epoch.clone(),
        command_id,
        expected_revision: event.expected_revision,
        command,
    };
    domain.execute(&Actor::System, &envelope, None, || Ok(()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::random_hex;

    struct Fixture(std::path::PathBuf);
    impl Fixture {
        fn new() -> Self {
            let home =
                std::env::temp_dir().join(format!("alondra-agent-lifecycle-{}", random_hex(8)));
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

    #[test]
    fn registration_and_status_survive_restart_and_replay_once() {
        let fixture = Fixture::new();
        let identity = new_identity().unwrap();
        let id = identity.agent_id.clone();
        assert_ne!(id.0, identity.claude_session_id);
        let domain = fixture.open();
        let register = prepare_event(&domain, "launch-one").unwrap();
        let registered = register_instance(&domain, &identity, "Builder", &register).unwrap();
        assert!(registered.changed);
        assert_eq!(registered.created_ids, vec![id.0.clone()]);
        assert!(!domain.state().unwrap().agents[&id].connected);
        let replay = register_instance(&domain, &identity, "Builder", &register).unwrap();
        assert_eq!(replay.revision, registered.revision);
        let spawn = prepare_event(&domain, "launch-one-spawn").unwrap();
        let spawned = observe_instance(&domain, &id, true, &spawn).unwrap();
        assert!(spawned.changed);
        let exit = prepare_event(&domain, "launch-one-exit").unwrap();
        let stopped = observe_instance(&domain, &id, false, &exit).unwrap();
        assert!(stopped.changed);
        drop(domain);

        let reopened = fixture.open();
        assert!(!reopened.state().unwrap().agents[&id].connected);
        let replay = observe_instance(&reopened, &id, false, &exit).unwrap();
        assert_eq!(replay.revision, stopped.revision);
        let restart = prepare_event(&reopened, "launch-two-spawn").unwrap();
        let restarted = observe_instance(&reopened, &id, true, &restart).unwrap();
        assert!(restarted.changed);
        assert!(reopened.state().unwrap().agents[&id].connected);
    }

    #[test]
    fn lifecycle_ids_cannot_be_reused_for_another_payload() {
        let fixture = Fixture::new();
        let domain = fixture.open();
        let identity = new_identity().unwrap();
        let id = identity.agent_id.clone();
        let register = prepare_event(&domain, "launch-one").unwrap();
        register_instance(&domain, &identity, "Builder", &register).unwrap();
        let error = register_instance(&domain, &identity, "Other", &register).unwrap_err();
        assert!(
            matches!(error, StoreError::Domain(ref value) if value.code == "idempotency_conflict")
        );
        let status = prepare_event(&domain, "status-one").unwrap();
        observe_instance(&domain, &id, false, &status).unwrap();
        let error = observe_instance(&domain, &id, true, &status).unwrap_err();
        assert!(
            matches!(error, StoreError::Domain(ref value) if value.code == "idempotency_conflict")
        );
    }

    #[test]
    fn unknown_agent_and_empty_event_are_rejected_without_rows() {
        let fixture = Fixture::new();
        let domain = fixture.open();
        let identity = new_identity().unwrap();
        let id = identity.agent_id.clone();
        let unknown = prepare_event(&domain, "unknown-spawn").unwrap();
        assert!(observe_instance(&domain, &id, true, &unknown).is_err());
        assert!(prepare_event(&domain, "").is_err());
        assert!(domain.state().unwrap().agents.is_empty());
    }
}
