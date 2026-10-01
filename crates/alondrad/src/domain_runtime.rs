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
    schedule_changed: watch::Sender<Revision>,
    pub(crate) reads: crate::domain_reads::Cache,
    pub(crate) live: crate::v2_client_routes::Live,
    /// User messages for sessions in the user's own terminal, handed at their next Stop hook.
    pub(crate) held:
        Mutex<std::collections::BTreeMap<alondra_core::domain_v2::AgentId, Vec<String>>>,
    /// Tasks whose Agent was already reminded, at a turn end, to report through Alondra.
    pub(crate) nudged: Mutex<std::collections::BTreeSet<alondra_core::domain_v2::TaskId>>,
}
impl DomainRuntime {
    pub(crate) fn transport_receipt(
        &self,
        identity: TransportIdentity<'_>,
        authorize: impl FnOnce() -> std::result::Result<(), StoreError>,
    ) -> Result<Option<CommandOutcome>, StoreError> {
        let mut store = self.store.lock().expect("domain store poisoned");
        let _approval = crate::approval_lease::ApprovalLease::shared(&self.approval_home)?;
        authorize()?;
        store.transport_receipt(identity)
    }

    pub fn open(home: &Path) -> Result<Self> {
        Self::open_at(home, &crate::domain_database_migration::active_path(home))
    }

    pub(crate) fn open_at(home: &Path, database: &Path) -> Result<Self> {
        let mut store = DomainStore::open(database)?;
        let state = store.load()?;
        let (changed, _) = watch::channel(state.revision);
        let (schedule_changed, _) = watch::channel(state.revision);
        let authority = capabilities::load(home, state.data_epoch.0)?;
        Ok(Self {
            home: home.to_path_buf(),
            store: Mutex::new(store),
            approval_home: home.to_path_buf(),
            authority: Mutex::new(authority),
            changed,
            schedule_changed,
            reads: Default::default(),
            live: Default::default(),
            held: Default::default(),
            nudged: Default::default(),
        })
    }
    pub(crate) fn home(&self) -> &Path {
        &self.home
    }
    pub fn state(&self) -> Result<DomainState> {
        self.store.lock().expect("domain store poisoned").load()
    }
    pub(crate) fn schedule_request_tools(
        &self,
        request_id: &RequestId,
    ) -> Result<Option<Vec<String>>> {
        self.store
            .lock()
            .expect("domain store poisoned")
            .schedule_request_tools(request_id)
    }
    pub(crate) fn was_schedule_launch_failed(&self, request_id: &RequestId) -> Result<bool> {
        self.store
            .lock()
            .expect("domain store poisoned")
            .was_schedule_launch_failed(request_id)
    }
    pub fn subscribe(&self) -> watch::Receiver<Revision> {
        self.changed.subscribe()
    }
    pub(crate) fn subscribe_schedules(&self) -> watch::Receiver<Revision> {
        self.schedule_changed.subscribe()
    }

    /// Issued capabilities outlive this process: the Claude session keeps its
    /// bearer across daemon restarts, so the digest is persisted before the
    /// token is handed out.
    pub fn issue(&self, principal: Principal) -> Result<Capability, StoreError> {
        let mut authority = self.authority.lock().expect("domain authority poisoned");
        let capability = authority
            .issue(principal)
            .map_err(|message| DomainError::new("forbidden", message))?;
        capabilities::persist(&self.home, &authority)?;
        Ok(capability)
    }

    /// Bind a Claude session that the domain already knows as an Agent, such as
    /// one imported at cutover or resumed after a restart. Any earlier bearer
    /// for that session stops working the moment the new one is issued.
    pub fn bind_agent(&self, session_id: &str) -> Result<Capability, StoreError> {
        let mut authority = self.authority.lock().expect("domain authority poisoned");
        let state = self.store.lock().expect("domain store poisoned").load()?;
        if !state.agents.contains_key(&AgentId(session_id.to_owned())) {
            return Err(DomainError::new("not_found", "Agent does not exist").into());
        }
        authority.revoke_agent(session_id);
        let capability = authority
            .issue(Principal::Agent {
                session_id: session_id.to_owned(),
                task_binding: None,
                provider_agent_id: None,
            })
            .map_err(|message| DomainError::new("forbidden", message))?;
        capabilities::persist(&self.home, &authority)?;
        Ok(capability)
    }

    /// Bind one orchestrator execution to the current generation.
    pub fn bind_orchestrator(&self, execution_id: &str) -> Result<Capability, StoreError> {
        let mut authority = self.authority.lock().expect("domain authority poisoned");
        let state = self.store.lock().expect("domain store poisoned").load()?;
        authority.revoke_execution(execution_id);
        let capability = authority
            .issue(Principal::Orchestrator {
                generation: state.orchestrator_generation,
                execution_id: execution_id.to_owned(),
            })
            .map_err(|message| DomainError::new("forbidden", message))?;
        capabilities::persist(&self.home, &authority)?;
        Ok(capability)
    }

    pub fn revoke_agent(&self, session: &str) -> Result<()> {
        let mut authority = self.authority.lock().expect("domain authority poisoned");
        authority.revoke_agent(session);
        capabilities::persist(&self.home, &authority).map_err(anyhow::Error::from)
    }
    pub fn revoke_execution(&self, execution: &str) -> Result<()> {
        let mut authority = self.authority.lock().expect("domain authority poisoned");
        authority.revoke_execution(execution);
        capabilities::persist(&self.home, &authority).map_err(anyhow::Error::from)
    }

    /// Expose only the public protocol cursor to a daemon-authorized producer.
    /// Commands recheck the capability while holding the writer lock.
    pub fn resolve_bearer(&self, bearer: &str) -> Result<Principal, StoreError> {
        let authority = self.authority.lock().expect("domain authority poisoned");
        let state = self.store.lock().expect("domain store poisoned").load()?;
        authority
            .resolve(&state.data_epoch.0, bearer)
            .map_err(|message| DomainError::new("forbidden", message).into())
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
        self.execute_with(actor, envelope, transport, None, authorize)
    }

    /// What the user chose when asking for the Request, if it was asked for with choices.
    pub(crate) fn request_launch(
        &self,
        request_id: &RequestId,
    ) -> Result<Option<crate::domain_store::RequestLaunch>> {
        self.store
            .lock()
            .expect("domain store poisoned")
            .request_launch(request_id)
    }

    pub(crate) fn execute_with(
        &self,
        actor: &Actor,
        envelope: &CommandEnvelope,
        transport: Option<TransportIdentity<'_>>,
        launch: Option<&crate::domain_store::RequestLaunch>,
        authorize: impl FnOnce() -> std::result::Result<(), StoreError>,
    ) -> Result<CommandOutcome, StoreError> {
        let mut store = self.store.lock().expect("domain store poisoned");
        // Lock order is domain store → approval lease. Revocation takes only the
        // exclusive approval lease and never waits for the domain store.
        let _approval = if matches!(actor, Actor::User { .. }) || transport.is_some() {
            Some(crate::approval_lease::ApprovalLease::shared(
                &self.approval_home,
            )?)
        } else {
            None
        };
        authorize()?;
        let prepared = crate::domain_content::prepare(&self.home, envelope)?;
        let commit = match launch {
            Some(launch) => store.execute_launching(actor, &prepared.envelope, transport, launch),
            None => store.execute_authenticated(actor, &prepared.envelope, transport),
        };
        if matches!(&commit, Err(StoreError::Domain(_))) {
            prepared.discard_uncommitted(&self.home);
        }
        let commit = commit?;
        if !commit.replayed && commit.outcome.changed {
            self.changed.send_replace(commit.state.revision);
            if schedule_command(&prepared.envelope.command) {
                self.schedule_changed.send_replace(commit.state.revision);
            }
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
        let commit = store.execute_filling_revision(&actor, &prepared.envelope);
        if matches!(&commit, Err(StoreError::Domain(_))) {
            prepared.discard_uncommitted(&self.home);
        }
        let commit = commit?;
        if !commit.replayed && commit.outcome.changed {
            self.changed.send_replace(commit.state.revision);
            if schedule_command(&prepared.envelope.command) {
                self.schedule_changed.send_replace(commit.state.revision);
            }
        }
        Ok(commit.outcome)
    }
}

fn schedule_command(command: &Command) -> bool {
    matches!(
        command,
        Command::CreateSchedule { .. }
            | Command::UpdateSchedule { .. }
            | Command::DeleteSchedule { .. }
            | Command::ClaimScheduleRun { .. }
    )
}

/// Exhaustive scope resolution for delegated producers; unscoped commands are never admitted.
fn command_task(state: &DomainState, command: &Command) -> Option<TaskId> {
    match command {
        Command::CreateTask { task_id, .. }
        | Command::AssignTask { task_id, .. }
        | Command::RetitleTask { task_id, .. }
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
        | Command::WithdrawQuestion { interaction_id }
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
        | Command::CreateSchedule { .. }
        | Command::UpdateSchedule { .. }
        | Command::DeleteSchedule { .. }
        | Command::ClaimScheduleRun { .. }
        | Command::RecordScheduleRunSent { .. }
        | Command::ClaimScheduleLaunch { .. }
        | Command::RecordScheduleLaunchHanded { .. }
        | Command::RecordScheduleRunFailed { .. }
        | Command::InterruptScheduleRunsOnRestart { .. }
        | Command::RegisterAgent { .. }
        | Command::ObserveAgent { .. }
        | Command::AdvanceOrchestratorGeneration { .. }
        | Command::HoldHook { .. } => None,
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
                auto_merge: false,
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

#[cfg(test)]
mod producer_revision_tests {
    use super::*;

    #[test]
    fn a_bearer_producer_may_omit_the_revision_and_retries_replay_its_receipt() {
        let home = std::env::temp_dir().join(format!(
            "alondra-domain-producer-revision-{}",
            crate::auth::random_hex(8)
        ));
        std::fs::create_dir_all(&home).unwrap();
        let runtime = DomainRuntime::open(&home).unwrap();
        let envelope = |runtime: &DomainRuntime, id: &str, expected: Option<Revision>, command| {
            let state = runtime.state().unwrap();
            CommandEnvelope {
                protocol_version: PROTOCOL_VERSION,
                data_epoch: state.data_epoch,
                command_id: id.into(),
                expected_revision: expected.unwrap_or(state.revision),
                command,
            }
        };
        runtime
            .execute(
                &Actor::System,
                &envelope(
                    &runtime,
                    "agent",
                    None,
                    Command::RegisterAgent {
                        agent_id: "agent-a".into(),
                        name: "A".into(),
                    },
                ),
                None,
                || Ok(()),
            )
            .unwrap();
        let user = Actor::User {
            user_id: "u".into(),
        };
        runtime
            .execute(
                &user,
                &envelope(
                    &runtime,
                    "request",
                    None,
                    Command::CreateRequest {
                        request_id: "request-a".into(),
                        body: "Work".into(),
                        auto_merge: false,
                    },
                ),
                None,
                || Ok(()),
            )
            .unwrap();
        runtime
            .execute(
                &user,
                &envelope(
                    &runtime,
                    "task",
                    None,
                    Command::CreateTask {
                        task_id: "task-a".into(),
                        request_id: "request-a".into(),
                        title: "Task".into(),
                        agent_id: Some("agent-a".into()),
                        review_prs: false,
                        dependencies: Default::default(),
                        stack: None,
                    },
                ),
                None,
                || Ok(()),
            )
            .unwrap();
        let token = runtime
            .issue(Principal::Agent {
                session_id: "agent-a".into(),
                task_binding: None,
                provider_agent_id: None,
            })
            .unwrap();
        let bearer = token.expose_for_child_environment();
        let publish = envelope(
            &runtime,
            "producer-info",
            Some(0),
            Command::PublishInformation {
                attachments: vec![],
                interaction_id: "info-a".into(),
                scope: Scope::Task {
                    task_id: "task-a".into(),
                },
                recipient: Recipient::User,
                body: "Progress".into(),
            },
        );
        let revision = runtime.state().unwrap().revision;
        let first = runtime.execute_bearer(bearer, &publish).unwrap();
        assert!(first.changed);
        assert_eq!(first.revision, revision + 1);
        assert_eq!(
            runtime.execute_bearer(bearer, &publish).unwrap(),
            first,
            "a retry with the bytes the producer sent replays its receipt"
        );
        assert_eq!(runtime.state().unwrap().revision, revision + 1);
        let stale = runtime.execute(
            &user,
            &CommandEnvelope {
                expected_revision: 0,
                ..envelope(
                    &runtime,
                    "user-zero",
                    None,
                    Command::CreateRequest {
                        request_id: "request-b".into(),
                        body: "More".into(),
                        auto_merge: false,
                    },
                )
            },
            None,
            || Ok(()),
        );
        assert!(
            matches!(stale, Err(StoreError::Domain(error)) if error.code == "stale_revision"),
            "a user command is never filled"
        );
        drop(runtime);
        std::fs::remove_dir_all(home).unwrap();
    }
}

mod capabilities;

#[cfg(test)]
mod mcp_flow_tests;
