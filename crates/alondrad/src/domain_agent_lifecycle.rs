//! Trusted lifecycle writes for Claude sessions managed by this daemon.
//!
//! `AgentId` identifies one managed instance; the Claude conversation UUID is
//! separate. These functions must only be called from the daemon's launch path
//! and its process callbacks. MCP
//! input, environment variables and a PID supplied by a client are not evidence
//! that an Agent exists.

use crate::domain_runtime::DomainRuntime;
use crate::domain_store::StoreError;
use alondra_core::domain::{
    Actor, AgentId, Command, CommandEnvelope, CommandId, CommandOutcome, DataEpoch, DomainError, DomainState,
    Millis, NodeEvent, Listed, Revision, PROTOCOL_VERSION,
};
use std::collections::BTreeMap;
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
    at: Millis,
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
        data_epoch: state.data_epoch.clone(),
        expected_revision: state.revision,
        at: alondra_core::now_millis(),
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
            at: event.at,
        },
    )
}

/// Hand the node machine something that happened to a node. Each distinct happening needs a
/// distinct event; reuse the whole event only when retrying an uncertain commit.
pub(crate) fn move_instance(
    domain: &DomainRuntime,
    agent_id: &AgentId,
    happened: NodeEvent,
    event: &LifecycleEvent,
) -> Result<CommandOutcome, StoreError> {
    submit(domain, "agent-move", event, Command::MoveNode { agent_id: agent_id.clone(), event: happened, at: event.at })
}

/// Records `happened` when the node machine would move the node for it, and says whether it did.
/// An event the machine refuses (a late report, a closed node) or that changes nothing is
/// dropped without a write.
pub(crate) fn node_event(domain: &DomainRuntime, agent_id: &AgentId, happened: NodeEvent) -> Result<bool, StoreError> {
    node_event_at(domain, agent_id, happened, alondra_core::now_millis())
}

/// `node_event` for something that happened at `at`, by the reporter's clock.
pub(crate) fn node_event_at(domain: &DomainRuntime, agent_id: &AgentId, happened: NodeEvent, at: Millis) -> Result<bool, StoreError> {
    let state = domain.state().map_err(StoreError::Storage)?;
    if !matches!(state.node_next(agent_id, &happened, at), Ok(Some(_))) {
        return Ok(false);
    }
    let id = format!("{}-{}-{}", happened.key(), agent_id.0, crate::auth::random_hex(8));
    submit_on_current(domain, "agent-move", &id, &state, Command::MoveNode { agent_id: agent_id.clone(), event: happened, at })
}

/// Keeps what a node said as a turn of it ended: what the user reads if that turn ends with
/// nothing for them.
pub(crate) fn record_last_words(domain: &DomainRuntime, agent_id: &AgentId, text: &str) -> Result<bool, StoreError> {
    let state = domain.state().map_err(StoreError::Storage)?;
    if text.trim().is_empty() || state.last_words.get(agent_id).is_some_and(|kept| kept == text.trim()) {
        return Ok(false);
    }
    let id = format!("last-words-{}-{}", agent_id.0, crate::auth::random_hex(8));
    submit_on_current(domain, "agent-last-words", &id, &state, Command::RecordLastWords { agent_id: agent_id.clone(), text: text.to_owned() })
}

/// Hands the node machine the listing of real sessions, when it moves any node.
#[cfg(test)]
pub(crate) fn reconcile(domain: &DomainRuntime, listed: BTreeMap<AgentId, Listed>) -> Result<bool, StoreError> {
    reconcile_at(domain, listed, alondra_core::now_millis())
}

/// `reconcile` with the listing stamped `at`, when it was asked for: it says nothing of what
/// happened after.
pub(crate) fn reconcile_at(domain: &DomainRuntime, listed: BTreeMap<AgentId, Listed>, at: alondra_core::Millis) -> Result<bool, StoreError> {
    let state = domain.state().map_err(StoreError::Storage)?;
    if !state.reconciling_moves(&listed, at) {
        return Ok(false);
    }
    let id = format!("reconcile-{}", crate::auth::random_hex(8));
    submit_on_current(domain, "agent-reconcile", &id, &state, Command::ReconcileNodes { listed, at })
}

/// The node machine judges a move on the state it commits against, so the move is never refused
/// as stale because another command committed since `state` was read.
fn submit_on_current(domain: &DomainRuntime, kind: &str, event_id: &str, state: &DomainState, command: Command) -> Result<bool, StoreError> {
    let envelope = CommandEnvelope {
        protocol_version: PROTOCOL_VERSION,
        data_epoch: state.data_epoch.clone(),
        command_id: command_id(kind, event_id),
        expected_revision: 0,
        command,
    };
    domain.execute_on_current(&envelope).map(|outcome| outcome.changed)
}

fn command_id(kind: &str, event_id: &str) -> CommandId {
    let digest = Sha256::digest(event_id.as_bytes());
    let hexadecimal: String = digest.iter().map(|byte| format!("{byte:02x}")).collect();
    CommandId(format!("{kind}-{hexadecimal}"))
}

fn submit(
    domain: &DomainRuntime,
    kind: &str,
    event: &LifecycleEvent,
    command: Command,
) -> Result<CommandOutcome, StoreError> {
    let envelope = CommandEnvelope {
        protocol_version: PROTOCOL_VERSION,
        data_epoch: event.data_epoch.clone(),
        command_id: command_id(kind, &event.event_id),
        expected_revision: event.expected_revision,
        command,
    };
    domain.execute(&Actor::System, &envelope, None, || Ok(()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use alondra_core::domain::NodePhase;

    struct Fixture(crate::test_support::Directory);
    impl Fixture {
        fn new() -> Self {
            let home = crate::test_support::Directory::new();
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

    /// The race of 15 October: the SessionEnd hook read the node still closing, the listing of
    /// sessions closed it, and the hook's event then reached a closed node. Closing it again is
    /// what was sought, so it goes through.
    #[test]
    fn an_end_that_reaches_a_node_already_closed_by_another_path_is_not_an_error() {
        let fixture = Fixture::new();
        let domain = fixture.open();
        let agent = AgentId("session-a".into());
        submit_on_current(&domain, "agent-register", "a", &domain.state().unwrap(), Command::RegisterAgent { agent_id: agent.clone(), name: "A".into(), at: 0 }).unwrap();
        assert!(node_event(&domain, &agent, NodeEvent::SessionClosed).unwrap());
        let seen_before = domain.state().unwrap();
        for (n, event) in [NodeEvent::SessionLost, NodeEvent::SessionClosed].into_iter().enumerate() {
            let moved = submit_on_current(&domain, "agent-move", &format!("late-{n}"), &seen_before,
                Command::MoveNode { agent_id: agent.clone(), event, at: alondra_core::now_millis() });
            assert_eq!(moved.map_err(|error| error.to_string()), Ok(false), "closing a closed node again changes nothing and is no error");
        }
        assert_eq!(domain.state().unwrap().agents[&agent].phase, NodePhase::Closed);
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
        assert_eq!(domain.state().unwrap().agents[&id].phase, NodePhase::Starting { reviving: false, idle_since: None });
        let replay = register_instance(&domain, &identity, "Builder", &register).unwrap();
        assert_eq!(replay.revision, registered.revision);
        let spawn = prepare_event(&domain, "launch-one-turn").unwrap();
        let spawned = move_instance(&domain, &id, NodeEvent::TurnStarted, &spawn).unwrap();
        assert!(spawned.changed);
        let exit = prepare_event(&domain, "launch-one-exit").unwrap();
        let stopped = move_instance(&domain, &id, NodeEvent::SessionClosed, &exit).unwrap();
        assert!(stopped.changed);
        drop(domain);

        let reopened = fixture.open();
        assert_eq!(reopened.state().unwrap().agents[&id].phase, NodePhase::Closed);
        let replay = move_instance(&reopened, &id, NodeEvent::SessionClosed, &exit).unwrap();
        assert_eq!(replay.revision, stopped.revision);
        let restart = prepare_event(&reopened, "launch-two-turn").unwrap();
        let refused = move_instance(&reopened, &id, NodeEvent::TurnStarted, &restart).unwrap_err();
        assert!(matches!(refused, StoreError::Domain(ref value) if value.code == "node_closed"), "a closed node stays closed");
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
        move_instance(&domain, &id, NodeEvent::TurnStarted, &status).unwrap();
        let error = move_instance(&domain, &id, NodeEvent::TurnEnded, &status).unwrap_err();
        assert!(
            matches!(error, StoreError::Domain(ref value) if value.code == "idempotency_conflict")
        );
    }

    #[test]
    fn a_node_event_judged_on_an_older_state_is_judged_again_not_lost() {
        let fixture = Fixture::new();
        let gate = fixture.open();
        let paused = new_identity().unwrap();
        register_instance(&gate, &paused, "Builder", &prepare_event(&gate, "launch-paused").unwrap()).unwrap();
        let elsewhere = fixture.open();
        let other = new_identity().unwrap();
        register_instance(&elsewhere, &other, "Other", &prepare_event(&elsewhere, "launch-other").unwrap()).unwrap();
        assert!(gate.state().unwrap().revision < elsewhere.state().unwrap().revision, "the gate's state is older");

        assert_eq!(node_event(&gate, &paused.agent_id, NodeEvent::TurnStarted).unwrap(), true);
        assert_eq!(gate.state().unwrap().agents[&paused.agent_id].phase, NodePhase::Working);
    }

    #[test]
    fn unknown_agent_and_empty_event_are_rejected_without_rows() {
        let fixture = Fixture::new();
        let domain = fixture.open();
        let identity = new_identity().unwrap();
        let id = identity.agent_id.clone();
        let unknown = prepare_event(&domain, "unknown-spawn").unwrap();
        assert!(move_instance(&domain, &id, NodeEvent::TurnStarted, &unknown).is_err());
        assert!(prepare_event(&domain, "").is_err());
        assert!(domain.state().unwrap().agents.is_empty());
    }
}
