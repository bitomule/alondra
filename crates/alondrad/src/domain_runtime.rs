//! Serialization boundary for the SQL domain and its authenticated producers.
use crate::domain_authority::{Authority, Capability, Principal};
use crate::domain_store::{DomainStore, StoreError, TransportIdentity};
use alondra_core::domain_v2::*;
use anyhow::Result;
use std::path::Path;
use std::sync::Mutex;
use tokio::sync::watch;

pub struct DomainRuntime {
    pub(crate) store: Mutex<DomainStore>,
    authority: Mutex<Authority>,
    changed: watch::Sender<Revision>,
}
impl DomainRuntime {
    pub fn open(home: &Path) -> Result<Self> {
        let mut store = DomainStore::open(&home.join("domain-v2.db"))?;
        let state = store.load()?;
        let (changed, _) = watch::channel(state.revision);
        Ok(Self {
            store: Mutex::new(store),
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
        authorize: impl FnOnce() -> Result<()>,
    ) -> Result<CommandOutcome, StoreError> {
        let mut store = self.store.lock().expect("domain store poisoned");
        authorize()?;
        let commit = store.execute_authenticated(actor, envelope, transport)?;
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
        let commit = store.execute_authenticated(&actor, envelope, None)?;
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
