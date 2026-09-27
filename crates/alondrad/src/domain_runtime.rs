//! Serialization boundary for the SQL domain and its authenticated producers.
use crate::domain_authority::{Authority, Capability, Principal};
use crate::domain_store::{DomainStore, StoreError, TransportIdentity};
use alondra_core::domain_v2::*;
use anyhow::Result;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use tokio::sync::watch;

pub struct DomainRuntime {
    home: PathBuf,
    pub(crate) store: Mutex<DomainStore>,
    approval_home: PathBuf,
    authority: Mutex<Authority>,
    changed: watch::Sender<Revision>,
}
impl DomainRuntime {
    pub fn open(home: &Path) -> Result<Self> {
        let mut store = DomainStore::open(&home.join("domain-v2.db"))?;
        let state = store.load()?;
        let (changed, _) = watch::channel(state.revision);
        Ok(Self {
            home: home.to_path_buf(),
            store: Mutex::new(store),
            approval_home: home.to_path_buf(),
            authority: Mutex::new(Authority::new(state.data_epoch.0)),
            changed,
        })
    }
    pub fn projection(&self) -> Result<DomainProjection> {
        self.store.lock().expect("domain store poisoned").project()
    }
    pub fn snapshot(&self, limit: usize) -> Result<Vec<u8>> {
        self.store
            .lock()
            .expect("domain store poisoned")
            .project_bounded(limit)
            .map_err(|error| {
                if error.is::<crate::domain_store::ProjectionTooLarge>() {
                    crate::http::OverviewTooLarge.into()
                } else {
                    error
                }
            })
    }
    pub fn state(&self) -> Result<DomainState> {
        self.store.lock().expect("domain store poisoned").load()
    }
    pub fn subscribe(&self) -> watch::Receiver<Revision> {
        self.changed.subscribe()
    }

    pub fn issue(&self, principal: Principal) -> Result<Capability, &'static str> {
        self.authority
            .lock()
            .expect("domain authority poisoned")
            .issue(principal)
    }
    pub fn revoke_agent(&self, session: &str) {
        self.authority
            .lock()
            .expect("domain authority poisoned")
            .revoke_agent(session);
    }
    pub fn revoke_execution(&self, execution: &str) {
        self.authority
            .lock()
            .expect("domain authority poisoned")
            .revoke_execution(execution);
    }

    /// `authorize` rechecks mutable transport approval while holding the writer lock.
    /// The actor is constructed by trusted adapters, never decoded from client JSON.
    pub fn execute(
        &self,
        actor: &Actor,
        envelope: &CommandEnvelope,
        transport: Option<TransportIdentity<'_>>,
        authorize: impl FnOnce() -> std::result::Result<(), StoreError>,
    ) -> Result<CommandOutcome, StoreError> {
        let mut store = self.store.lock().expect("domain store poisoned");
        // Lock order is domain store → approval lease. Revocation takes only the
        // exclusive approval lease and never waits for the domain store.
        let _approval = if matches!(actor, Actor::User { .. }) {
            Some(crate::approval_lease::ApprovalLease::shared(
                &self.approval_home,
            )?)
        } else {
            None
        };
        authorize()?;
        let prepared = crate::domain_content::prepare(&self.home, envelope)?;
        let commit = store.execute_authenticated(actor, &prepared.envelope, transport);
        if matches!(&commit, Err(StoreError::Domain(_))) {
            prepared.discard_uncommitted(&self.home);
        }
        let commit = commit?;
        if !commit.replayed && commit.outcome.changed {
            self.changed.send_replace(commit.state.revision);
        }
        Ok(commit.outcome)
    }

    pub fn execute_bearer(
        &self,
        bearer: &str,
        envelope: &CommandEnvelope,
    ) -> Result<CommandOutcome, StoreError> {
        // Keep the capability lease until commit: concurrent revocation cannot pass this write.
        let authority = self.authority.lock().expect("domain authority poisoned");
        let principal = authority
            .resolve(&envelope.data_epoch.0, bearer)
            .map_err(|message| DomainError::new("forbidden", message))?;
        let mut store = self.store.lock().expect("domain store poisoned");
        let state = store.load()?;
        let actor = match principal {
            Principal::Agent {
                session_id,
                task_binding,
                ..
            } => {
                if let Some(binding) = task_binding {
                    let task = command_task(&state, &envelope.command);
                    if task.as_ref().map(|id| id.0.as_str()) != Some(binding.as_str()) {
                        return Err(DomainError::new(
                            "not_assigned",
                            "provider child is bound to a different task",
                        )
                        .into());
                    }
                }
                Actor::Agent {
                    agent_id: session_id.into(),
                }
            }
            Principal::Orchestrator { generation, .. } => Actor::Orchestrator { generation },
        };
        let prepared = crate::domain_content::prepare(&self.home, envelope)?;
        let commit = store.execute_authenticated(&actor, &prepared.envelope, None);
        if matches!(&commit, Err(StoreError::Domain(_))) {
            prepared.discard_uncommitted(&self.home);
        }
        let commit = commit?;
        if !commit.replayed && commit.outcome.changed {
            self.changed.send_replace(commit.state.revision);
        }
        Ok(commit.outcome)
    }
}

/// Exhaustive scope resolution for delegated producers; unscoped commands are never admitted.
fn command_task(state: &DomainState, command: &Command) -> Option<TaskId> {
    match command {
        Command::CreateTask { task_id, .. }
        | Command::AssignTask { task_id, .. }
        | Command::SetDependencies { task_id, .. }
        | Command::StartTask { task_id }
        | Command::CreateDocument { task_id, .. }
        | Command::ReviseDocument { task_id, .. }
        | Command::LinkTaskDocument { task_id, .. }
        | Command::DecideDocumentReview { task_id, .. }
        | Command::RegisterPr { task_id, .. }
        | Command::PresentResult { task_id, .. }
        | Command::PublishStackReadiness { task_id, .. }
        | Command::CompleteTask { task_id, .. }
        | Command::CancelTask { task_id }
        | Command::ReopenTask { task_id }
        | Command::ArchiveTask { task_id, .. } => Some(task_id.clone()),
        Command::PublishInformation { scope, .. }
        | Command::AskQuestion { scope, .. }
        | Command::RequestAction { scope, .. } => match scope {
            Scope::Task { task_id } => Some(task_id.clone()),
            Scope::Orchestrator { .. } => None,
        },
        Command::MarkRead { interaction_id }
        | Command::AnswerQuestion { interaction_id, .. }
        | Command::LinkInteractionSubject { interaction_id, .. } => {
            interaction_task(state, interaction_id)
        }
        Command::SubmitFeedback {
            presentation_id, ..
        }
        | Command::AcceptDelivery {
            presentation_id, ..
        }
        | Command::AuthorizeMerge {
            presentation_id, ..
        } => interaction_task(state, presentation_id),
        Command::ClaimMergeAuthorization {
            authorization_id, ..
        }
        | Command::ObserveMergeAttempt {
            authorization_id, ..
        } => state
            .merge_authorizations
            .get(authorization_id)
            .map(|authorization| authorization.task_id.clone()),
        Command::ObservePrState { pr_id, .. } => {
            state.pull_requests.get(pr_id).map(|pr| pr.task_id.clone())
        }
        Command::CreateRequest { .. }
        | Command::CloseRequest { .. }
        | Command::RegisterAgent { .. }
        | Command::ObserveAgent { .. }
        | Command::AdvanceOrchestratorGeneration { .. } => None,
    }
}
fn interaction_task(state: &DomainState, id: &InteractionId) -> Option<TaskId> {
    match &state.interactions.get(id)?.scope {
        Scope::Task { task_id } => Some(task_id.clone()),
        Scope::Orchestrator { .. } => None,
    }
}

#[cfg(test)]
mod approval_tests {
    use super::*;
    use crate::auth::Auth;
    use base64::{Engine as _, engine::general_purpose::STANDARD};
    use p256::ecdsa::SigningKey;
    use std::sync::{Arc, mpsc};
    use std::time::Duration;

    #[test]
    fn external_revocation_waits_for_authorized_domain_commit() {
        let home = std::env::temp_dir().join(format!(
            "alondra-domain-lease-{}",
            crate::auth::random_hex(8)
        ));
        std::fs::create_dir_all(&home).unwrap();
        let auth = Auth::open(&home).unwrap();
        let key = SigningKey::from_slice(&[9; 32]).unwrap();
        let device = auth
            .pair(
                &auth.new_pairing_code(),
                "Test Phone",
                &STANDARD.encode(key.verifying_key().to_sec1_bytes()),
            )
            .unwrap();
        let runtime = Arc::new(DomainRuntime::open(&home).unwrap());
        let state = runtime.state().unwrap();
        let envelope = CommandEnvelope {
            protocol_version: PROTOCOL_VERSION,
            data_epoch: state.data_epoch,
            command_id: "command-lease".into(),
            expected_revision: state.revision,
            command: Command::CreateRequest {
                request_id: "request-lease".into(),
                body: "Work".into(),
            },
        };
        let (checked_tx, checked_rx) = mpsc::channel();
        let (resume_tx, resume_rx) = mpsc::channel();
        let writing = runtime.clone();
        let writer_home = home.clone();
        let device_id = device.id.clone();
        let writer = std::thread::spawn(move || {
            writing.execute(
                &Actor::User {
                    user_id: format!("device:{device_id}"),
                },
                &envelope,
                None,
                || {
                    assert!(
                        Auth::open(&writer_home)
                            .unwrap()
                            .list()
                            .iter()
                            .any(|entry| entry.id == device_id)
                    );
                    checked_tx.send(()).unwrap();
                    resume_rx.recv().unwrap();
                    Ok(())
                },
            )
        });
        checked_rx.recv().unwrap();
        let (started_tx, started_rx) = mpsc::channel();
        let (revoked_tx, revoked_rx) = mpsc::channel();
        let revoker_home = home.clone();
        let revoke_id = device.id.clone();
        let revoker = std::thread::spawn(move || {
            started_tx.send(()).unwrap();
            let changed = Auth::open(&revoker_home)
                .unwrap()
                .revoke(&revoke_id)
                .unwrap();
            revoked_tx.send(changed).unwrap();
        });
        started_rx.recv().unwrap();
        assert!(
            revoked_rx.recv_timeout(Duration::from_millis(30)).is_err(),
            "revoke returned while writer still held approval lease"
        );
        resume_tx.send(()).unwrap();
        assert!(writer.join().unwrap().unwrap().changed);
        assert!(revoked_rx.recv_timeout(Duration::from_secs(2)).unwrap());
        revoker.join().unwrap();
        assert!(
            runtime
                .state()
                .unwrap()
                .requests
                .contains_key(&"request-lease".into())
        );
        assert!(
            !Auth::open(&home)
                .unwrap()
                .list()
                .iter()
                .any(|entry| entry.id == device.id)
        );
        drop(runtime);
        std::fs::remove_dir_all(home).unwrap();
    }
}
