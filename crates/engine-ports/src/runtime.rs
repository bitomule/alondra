//! The runtime the ports are called on, and what a piece of the engine asks of it: the journal with
//! its launch queue, the live read and the read index, the subscription to revisions, and the
//! cards Alondra raises and withdraws as system commands. A piece written against [`Runtime`]
//! never needs the runtime's own type.
use crate::Registry;
use crate::journal::OwnerJournal;
use crate::journal::owner::StagedOutcome;
use crate::journal::{Journal, ReadTransaction, RequestLaunch, StoreError, TransportIdentity};
use alondra_store::{AuthorityScope, Committed, ContextPage, ContextRef};
use alondra_foundation::principal::{Capability, Principal};
use std::time::Duration;
use alondra_domain_rules::*;
use alondra_foundation::runtime_mutex::RuntimeMutex;
use std::any::Any;
use std::path::Path;
use std::sync::Arc;
use tokio::sync::watch;

/// The runtime as the trait object the ports take, from a concrete runtime as from one already
/// behind `dyn`: what lets a piece's extension trait cover both with one impl.
pub trait AsRuntime {
    fn as_runtime(&self) -> &dyn Runtime;
}

impl<R: Runtime> AsRuntime for R {
    fn as_runtime(&self) -> &dyn Runtime {
        self
    }
}

pub trait Runtime: Any + Send + Sync + AsRuntime {
    fn ports(&self) -> &Registry;
    /// The state each piece of the engine keeps in this runtime: read it with [`crate::extension`].
    fn extensions(&self) -> &crate::extensions::Extensions;
    /// Alondra's home.
    fn home(&self) -> &Path;
    /// The domain's last committed state.
    fn state(&self) -> Arc<DomainState>;
    /// The last committed state with its read index: both of the same revision.
    fn read(&self) -> Arc<ReadSnapshot>;
    /// A read of the committed journal that never waits for the writer: what is read through it is
    /// one commit's state.
    fn live_read(&self) -> ReadTransaction<'_>;
    /// The journal the runtime commits through, its launch queue among what it keeps.
    fn store(&self) -> &RuntimeMutex<Box<dyn Journal>>;
    /// Changes to the revision of every commit that changes the domain.
    fn subscribe(&self) -> watch::Receiver<Revision>;
    /// Changes when a snapshot is replaced, including a reload at the same domain revision.
    fn subscribe_snapshots(&self) -> watch::Receiver<u64>;
    /// Changes to the revision of every commit that changes a schedule.
    fn subscribe_schedules(&self) -> watch::Receiver<Revision>;
    /// Wakes whoever waits on a change of the domain after something outside it ended: the waiter
    /// looks again at what that end settled.
    fn wake_subscribers(&self);
    /// A command the daemon issues itself, judged on the state it commits against.
    fn execute_as_system(&self, envelope: CommandEnvelope) -> Result<CommandOutcome, StoreError>;
    /// Commits `envelope` as `actor`; `authorize` rechecks mutable transport approval while the
    /// writer lock is held. [`execute`](Runtime::execute) is the form callers use.
    fn execute_authorized(
        &self,
        actor: &Actor,
        envelope: &CommandEnvelope,
        transport: Option<TransportIdentity<'_>>,
        launch: Option<&RequestLaunch>,
        authorize: &mut dyn FnMut() -> Result<(), StoreError>,
    ) -> Result<CommandOutcome, StoreError>;
    /// Provider evidence, its normalization and the domain's decision committed in one
    /// transaction. [`own`](Runtime::own) is the form callers use.
    fn own_with(&self, scope: AuthorityScope, stage: &mut dyn FnMut(&mut dyn OwnerJournal) -> Result<(), StoreError>) -> Result<Vec<StagedOutcome>, StoreError>;

    /// An owner transaction whose commit `project` then writes outside the journal, such as the
    /// options file a settings change rewrites; a failed projection is repaired on the next read.
    fn own_projected_with(
        &self,
        scope: AuthorityScope,
        stage: &mut dyn FnMut(&mut dyn OwnerJournal) -> Result<(), StoreError>,
        project: &mut dyn FnMut(&mut dyn Journal) -> anyhow::Result<()>,
    ) -> Result<Vec<StagedOutcome>, StoreError>;
    /// What the journal issued for the execution `node` runs for `task`, with commits readers
    /// already see. A node the user kept after its task closed never takes what a later execution
    /// of the same task was issued.
    fn take_seals(&self, task: &TaskId, node: &AgentId) -> Committed;
    /// Blocks until something of `node`'s execution of `task` arrives, `stopped` says the owner
    /// ends, or `timeout` passes.
    fn wait_for_seals(&self, task: &TaskId, node: &AgentId, timeout: Duration, stopped: &dyn Fn() -> bool) -> Committed;
    /// Blocks for `timeout`, or until `stopped` says the owner ends, taking nothing.
    fn pause_for_seals(&self, timeout: Duration, stopped: &dyn Fn() -> bool);
    /// Drops the sealed observations of `task` no host is left to acknowledge.
    fn forget_sealed_observations(&self, task: &TaskId);
    /// Commits `envelope` as Alondra itself, at whatever revision the domain is at; a storage
    /// failure comes back as the refusal a producer gets.
    fn system_command(&self, envelope: CommandEnvelope) -> Result<CommandOutcome, DomainError>;
    /// Wakes the owners of the SDK hosts to what waits for them.
    fn wake_seals(&self);
    /// From now on `bell` rings whenever something of `node`'s execution of `task` arrives, while
    /// its owner keeps it.
    fn ring_on_seals(&self, task: &TaskId, node: &AgentId, bell: &std::sync::Arc<alondra_core::clock::Bell>);
    /// The plan `task` starts with, if it has one.
    fn launch_plan(&self, task: &TaskId) -> Option<LaunchPlan>;

    /// A new capability for a session the domain knows as an Agent; its earlier one stops working.
    fn bind_agent(&self, session_id: &str) -> Result<Capability, StoreError>;

    fn revoke_agent(&self, session: &str) -> Result<(), StoreError>;

    /// The principal of a bearer let change `what`, recorded before it acts.
    fn admit_change(&self, bearer: &str, what: &str) -> Result<Principal, StoreError>;

    fn resolve_bearer(&self, bearer: &str) -> Result<Principal, StoreError>;

    /// The digest of the capability the session holds now, if it holds one.
    fn agent_capability(&self, session: &str) -> Option<[u8; 32]>;

    /// Revokes the capability named by `digest`: one its session was issued after it stays.
    fn revoke_capability(&self, digest: &[u8; 32]) -> Result<(), StoreError>;

    /// What a transport already committed under `identity`; `authorize` rechecks its approval
    /// while the writer lock is held.
    fn transport_receipt(&self, identity: TransportIdentity<'_>, authorize: &mut dyn FnMut() -> Result<(), StoreError>) -> Result<Option<CommandOutcome>, StoreError>;

    /// Runs what a test set to run just before an owner acknowledges its commit.
    fn before_acknowledge(&self);

    /// The public conversation of `task`, for its own node's bearer only.
    fn read_current_task_context(&self, bearer: &str, task: &TaskId, after: Option<&ContextRef>, limit: usize) -> Result<ContextPage, DomainError>;

    /// A Problem authored by Alondra. `id` names it: raising it twice leaves the first. A newer
    /// Problem with the same `cause` in the same place replaces the open one.
    #[allow(clippy::too_many_arguments)]
    fn raise_problem(
        &self,
        id: &str,
        system: Option<SystemCard>,
        cause: Option<&str>,
        scope: Scope,
        need: String,
        why: String,
        diagnosis: String,
        options: Vec<QuestionOption>,
    ) -> Result<CommandOutcome, StoreError> {
        let state = self.state();
        if state.interactions.contains_key(&InteractionId(id.to_owned())) {
            return Err(DomainError::new("invalid_input", "Problem already raised").into());
        }
        self.execute_as_system(CommandEnvelope {
            protocol_version: PROTOCOL_VERSION,
            data_epoch: state.data_epoch.clone(),
            command_id: format!("raise-{id}").into(),
            expected_revision: state.revision,
            command: Command::RaiseProblem {
                attachments: vec![],
                interaction_id: id.to_owned().into(),
                scope,
                body: diagnosis.clone(),
                options,
                card: CardSpec { cause: cause.map(str::to_owned), system, ..CardSpec::new(CardKind::Problem, need).why(why).content(ContentKind::Diagnosis, diagnosis) },
            },
        })
    }

    /// A Problem whose `body` holds the numbers clients word in their own language, with no
    /// content of its own. `id` names it: raising it twice leaves the first.
    #[allow(clippy::too_many_arguments)]
    fn raise_worded_problem(
        &self,
        id: &str,
        system: Option<SystemCard>,
        cause: &str,
        need: String,
        why: String,
        body: String,
        options: Vec<QuestionOption>,
        recommended: Option<&str>,
    ) -> Result<CommandOutcome, StoreError> {
        self.raise_worded_problem_in(Scope::Orchestrator { request_id: None }, id, system, cause, need, why, body, options, recommended)
    }

    /// `raise_worded_problem` where `scope` says.
    #[allow(clippy::too_many_arguments)]
    fn raise_worded_problem_in(
        &self,
        scope: Scope,
        id: &str,
        system: Option<SystemCard>,
        cause: &str,
        need: String,
        why: String,
        body: String,
        options: Vec<QuestionOption>,
        recommended: Option<&str>,
    ) -> Result<CommandOutcome, StoreError> {
        self.raise_worded_problem_with_form(scope, id, system, cause, need, why, body, options, recommended, None)
    }

    /// `raise_worded_problem_in` with a form the user fills beside the option `accept`.
    #[allow(clippy::too_many_arguments)]
    fn raise_worded_problem_with_form(
        &self,
        scope: Scope,
        id: &str,
        system: Option<SystemCard>,
        cause: &str,
        need: String,
        why: String,
        body: String,
        options: Vec<QuestionOption>,
        recommended: Option<&str>,
        form: Option<alondra_core::requested_form::RequestedForm>,
    ) -> Result<CommandOutcome, StoreError> {
        let state = self.state();
        if state.interactions.contains_key(&InteractionId(id.to_owned())) {
            return Err(DomainError::new("invalid_input", "Problem already raised").into());
        }
        let mut card = CardSpec::new(CardKind::Problem, need).why(why);
        card.recommended = recommended.map(str::to_owned);
        card.system = system;
        card.form = form;
        self.execute_as_system(CommandEnvelope {
            protocol_version: PROTOCOL_VERSION,
            data_epoch: state.data_epoch.clone(),
            command_id: format!("raise-{id}").into(),
            expected_revision: state.revision,
            command: Command::RaiseProblem {
                attachments: vec![],
                interaction_id: id.to_owned().into(),
                scope,
                body,
                options,
                card: card.cause(cause),
            },
        })
    }

    /// Something the orchestrator hears in its next batch; `id` names it, so telling it twice
    /// leaves the first.
    fn tell_orchestrator(&self, id: &str, system: Option<SystemCard>, body: String) -> Result<CommandOutcome, StoreError> {
        let state = self.state();
        if state.interactions.contains_key(&InteractionId(id.to_owned())) {
            return Err(DomainError::new("invalid_input", "Already told").into());
        }
        self.execute_as_system(CommandEnvelope {
            protocol_version: PROTOCOL_VERSION,
            data_epoch: state.data_epoch.clone(),
            command_id: format!("tell-{id}").into(),
            expected_revision: state.revision,
            command: tell_command(id, system, body),
        })
    }

    /// A Know card authored by Alondra, shown to the user once: `id` names it, so publishing it
    /// again leaves the first.
    fn publish_notice(&self, id: &str, body: String) -> Result<CommandOutcome, StoreError> {
        let state = self.state();
        if state.interactions.contains_key(&InteractionId(id.to_owned())) {
            return Err(DomainError::new("invalid_input", "Notice already published").into());
        }
        self.execute_as_system(CommandEnvelope {
            protocol_version: PROTOCOL_VERSION,
            data_epoch: state.data_epoch.clone(),
            command_id: format!("notice-{id}").into(),
            expected_revision: state.revision,
            command: Command::PublishInformation {
                attachments: vec![],
                interaction_id: id.to_owned().into(),
                scope: Scope::Orchestrator { request_id: None },
                recipient: Recipient::User,
                card: CardSpec::new(CardKind::Know, "Alondra update").content(ContentKind::Report, &body),
                body,
            },
        })
    }

    /// A Decide card authored by Alondra that holds nothing back. `need` and `why` are the English
    /// source; clients render the text in their own language from the id and `body`. `id` names
    /// it, so asking twice leaves the first.
    #[allow(clippy::too_many_arguments)]
    fn ask_user(
        &self,
        id: &str,
        system: Option<SystemCard>,
        need: String,
        why: String,
        body: String,
        options: Vec<QuestionOption>,
        recommended: &str,
    ) -> Result<CommandOutcome, StoreError> {
        let state = self.state();
        if state.interactions.contains_key(&InteractionId(id.to_owned())) {
            return Err(DomainError::new("invalid_input", "Question already asked").into());
        }
        let mut card = CardSpec::new(CardKind::Decide, need).why(why);
        card.recommended = Some(recommended.to_owned());
        card.system = system;
        self.execute_as_system(CommandEnvelope {
            protocol_version: PROTOCOL_VERSION,
            data_epoch: state.data_epoch.clone(),
            command_id: format!("ask-{id}").into(),
            expected_revision: state.revision,
            command: Command::AskQuestion {
                attachments: vec![],
                interaction_id: id.to_owned().into(),
                scope: Scope::Orchestrator { request_id: None },
                recipient: Recipient::User,
                body,
                blocking: false,
                options,
                card,
            },
        })
    }

    /// A card of Alondra's that stopped mattering.
    fn withdraw_problem(&self, id: &InteractionId) -> Result<CommandOutcome, StoreError> {
        let state = self.state();
        self.execute_as_system(CommandEnvelope {
            protocol_version: PROTOCOL_VERSION,
            data_epoch: state.data_epoch.clone(),
            command_id: format!("withdraw-{id}-{}", state.revision).into(),
            expected_revision: state.revision,
            command: Command::Withdraw { interaction_id: id.clone() },
        })
    }
}

impl dyn Runtime + '_ {
    /// Commits `envelope` as `actor`; `authorize` rechecks mutable transport approval while the
    /// writer lock is held.
    pub fn execute(
        &self,
        actor: &Actor,
        envelope: &CommandEnvelope,
        transport: Option<TransportIdentity<'_>>,
        authorize: impl FnOnce() -> Result<(), StoreError>,
    ) -> Result<CommandOutcome, StoreError> {
        self.execute_with(actor, envelope, transport, None, authorize)
    }

    pub fn execute_with(
        &self,
        actor: &Actor,
        envelope: &CommandEnvelope,
        transport: Option<TransportIdentity<'_>>,
        launch: Option<&RequestLaunch>,
        authorize: impl FnOnce() -> Result<(), StoreError>,
    ) -> Result<CommandOutcome, StoreError> {
        let mut authorize = Some(authorize);
        self.execute_authorized(actor, envelope, transport, launch, &mut || authorize.take().expect("a commit authorizes once")())
    }

    /// Provider evidence, its normalization and the domain's decision commit in one transaction.
    pub fn own<T>(
        &self,
        scope: AuthorityScope,
        stage: impl FnOnce(&mut dyn OwnerJournal) -> Result<T, StoreError>,
    ) -> Result<(T, Vec<StagedOutcome>), StoreError> {
        let mut stage = Some(stage);
        let mut value = None;
        let outcomes = self.own_with(scope, &mut |owner| {
            let stage = stage.take().expect("an owner transaction stages once");
            value = Some(stage(owner)?);
            Ok(())
        })?;
        Ok((value.expect("a committed owner transaction staged its value"), outcomes))
    }

    pub fn own_projected<T>(
        &self,
        scope: AuthorityScope,
        stage: impl FnOnce(&mut dyn OwnerJournal) -> Result<T, StoreError>,
        project: impl FnOnce(&mut dyn Journal) -> anyhow::Result<()>,
    ) -> Result<(T, Vec<StagedOutcome>), StoreError> {
        let (mut stage, mut project) = (Some(stage), Some(project));
        let mut value = None;
        let outcomes = self.own_projected_with(scope, &mut |owner| {
            let stage = stage.take().expect("an owner transaction stages once");
            value = Some(stage(owner)?);
            Ok(())
        }, &mut |journal| project.take().expect("a commit is projected once")(journal))?;
        Ok((value.expect("a committed owner transaction staged its value"), outcomes))
    }
}

/// The command that tells the orchestrator `body` as the update `id`.
pub fn tell_command(id: &str, system: Option<SystemCard>, body: String) -> Command {
    Command::PublishInformation {
        attachments: vec![],
        interaction_id: id.to_owned().into(),
        scope: Scope::Orchestrator { request_id: None },
        recipient: Recipient::Orchestrator,
        card: CardSpec { system, ..CardSpec::new(CardKind::Know, "Task update").content(ContentKind::Report, &body) },
        body,
    }
}
