//! The owner of an SDK execution, on the domain's writer lane: it admits the execution with
//! `AdmitExecution`, launches the host, sends each dispatch the store issued for its task,
//! stages what the host observes and acknowledges it only once readers see the commit.
//! Every step commits through `DomainRuntime::own`; nothing here writes the store itself.
pub(crate) mod backend;
pub(crate) mod seals;
pub(crate) use backend::SdkBackend;
pub(crate) use seals::Seals;

use crate::domain_runtime::DomainRuntime;
use crate::domain_store::{Attached, Preparer, StoreError};
use alondra_agent_application::InputMetadata;
use alondra_agent_runtime::owned_host::VerifiedArtifact;
use alondra_core::domain::*;
use alondra_domain::ActivityView;
use alondra_foundation::InputId;
use alondra_provider_claude::{ClaudeConfiguration, ClaudeWorker, WorkerProfile};
use alondra_provider_contract::{AdapterError, Capability, Handoff, PreparedCommand, ProviderBinding};
use alondra_store::{AuthorityScope, Committed, DispatchAttempt, DispatchPermit, DispatchReceipt};
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

/// The host an SDK execution runs on, as the domain records it: never `claude-bg`.
pub(crate) const HOST: &str = "alondra-sdk";
const DRIVER: &str = "claude-agent-sdk";

/// Whether `agent` holds the current execution of a task that runs through the SDK.
pub(crate) fn runs_through_sdk(state: &DomainState, agent: &AgentId) -> bool {
    state.tasks.values().filter(|task| task.agent_id.as_ref() == Some(agent)).any(|task| {
        state
            .lifecycle_journal
            .get(&task.id)
            .and_then(|journal| replay(&task.id, journal).ok())
            .is_some_and(|lifecycle| lifecycle.execution().is_some_and(|execution| execution.host().as_str() == HOST))
    })
}

/// What a launch through the SDK starts: the task, in its worktree, told `prompt`.
pub(crate) struct Launch {
    pub(crate) task: TaskId,
    pub(crate) name: String,
    pub(crate) directory: String,
    pub(crate) prompt: String,
    /// Alondra's node plugin: its MCP server is how the task reaches Alondra.
    pub(crate) plugin: PathBuf,
}

#[derive(Debug)]
pub(crate) struct Failed(pub(crate) String);
impl std::fmt::Display for Failed {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}
fn failed(what: &str) -> impl Fn(StoreError) -> Failed + '_ {
    move |error| Failed(format!("{what}: {}", store_text(&error)))
}
fn store_text(error: &StoreError) -> String {
    match error {
        StoreError::Domain(error) => error.message.clone(),
        StoreError::Storage(error) => format!("{error:#}"),
    }
}
fn adapter(what: &str) -> impl Fn(AdapterError) -> Failed + '_ {
    move |error| Failed(format!("{what}: {error:?}"))
}

impl Preparer for ClaudeWorker {
    fn stage_input(&mut self, metadata: InputMetadata<'_, '_>) -> Result<(), AdapterError> {
        ClaudeWorker::stage_input(self, metadata)
    }
    fn prepare(&mut self, attempt: &DispatchAttempt, metadata: InputMetadata<'_, '_>) -> Result<PreparedCommand, AdapterError> {
        ClaudeWorker::prepare(self, attempt, metadata)
    }
}

/// One execution's owner: its worker and the task whose dispatches only it claims.
pub(crate) struct Driver {
    domain: Arc<DomainRuntime>,
    worker: ClaudeWorker,
    scope: AuthorityScope,
    task: TaskId,
    agent: AgentId,
    permits: VecDeque<DispatchPermit>,
    /// The input last handed to the host, until the task's lifecycle no longer waits for it.
    awaiting: Option<InputId>,
}

/// Admits the execution, starts its host and hands it the launch: once this returns, the task
/// runs through the SDK, and any failure before it is the launch's own, never a `claude --bg`.
pub(crate) fn start(domain: Arc<DomainRuntime>, backend: &SdkBackend, launch: Launch) -> Result<Driver, Failed> {
    let suffix = crate::auth::random_hex(8);
    let agent = AgentId(format!("sdk-{suffix}"));
    let state = domain.state().map_err(|error| Failed(format!("reading the domain: {error:#}")))?;
    let envelope = |name: &str, command: Command| CommandEnvelope {
        protocol_version: PROTOCOL_VERSION,
        data_epoch: state.data_epoch.clone(),
        command_id: format!("sdk-{name}-{}-{suffix}", launch.task).into(),
        expected_revision: 0,
        command,
    };
    // One binding for the daemon's SDK host: the store keeps one active binding per host, so a
    // binding of each launch would revoke every task after the first.
    let binding = ProviderBinding::new(
        format!("{DRIVER}-{HOST}"),
        DRIVER.into(),
        HOST.into(),
        "sandbox".into(),
        1,
        1,
        BTreeSet::from([Capability::OfferInput]),
        "native-login".into(),
        1,
    )
    .map_err(|error| Failed(format!("the provider binding: {error:?}")))?;
    let profile = WorkerProfile {
        plugins: vec![launch.plugin.clone()],
        settings: crate::plugins::launch_settings_with(None).as_object().cloned().unwrap_or_default(),
    };
    let mut configuration = configuration(backend, &launch.directory, &agent.0, true, Some(profile))?;
    let scope = AuthorityScope::new(state.data_epoch.clone()).map_err(|error| Failed(format!("{error:?}")))?;
    domain
        .execute_on_current(&envelope("register", Command::RegisterAgent {
            agent_id: agent.clone(),
            name: launch.name.clone(),
            at: alondra_core::now_millis(),
        }))
        .map_err(failed("registering the node"))?;
    let capability = domain.bind_agent(&agent.0).map_err(|error| Failed(format!("issuing the node's capability: {error}")))?;
    configuration.native_environment.extend(node_environment(domain.home(), &agent, capability.expose_for_child_environment()));
    let mut worker = ClaudeWorker::configured(binding.clone(), configuration).map_err(adapter("configuring the SDK worker"))?;
    let admit = envelope("admit", Command::AdmitExecution {
        task_id: launch.task.clone(),
        agent_id: agent.clone(),
        conversation: format!("sdk-conversation-{suffix}"),
        instance: agent.0.clone(),
        host: HOST.into(),
    });
    let (first, _) = domain
        .own(scope.clone(), |owner| {
            owner.stage_command(&Actor::System, &admit, Attached::None)?;
            owner.launch(&launch.task, &launch.prompt, &launch.directory, &binding, &mut worker)?;
            let lifecycle = owner.lifecycle(&launch.task)?;
            Ok(lifecycle.pending_inputs().first().map(|input| (*input).clone()))
        })
        .map_err(failed("admitting the execution"))?;
    let mut driver = Driver { domain, worker, scope, task: launch.task, agent, permits: VecDeque::new(), awaiting: None };
    let mut sealed = driver.sealed();
    let grant = sealed.owners.pop().ok_or_else(|| Failed("the admission issued no owner grant".into()))?;
    let (state, _) = driver.domain.own(driver.scope.clone(), |owner| owner.adapter_state(&grant)).map_err(failed("reading the provider state"))?;
    driver.sealed();
    driver.worker.install_owner(grant, state).map_err(adapter("starting the SDK host"))?;
    let permit = driver.permits.pop_front().ok_or_else(|| Failed("the admission issued no launch permit".into()))?;
    driver.dispatch(permit)?;
    driver.awaiting = first;
    Ok(driver)
}

/// The launch of a sandbox daemon with an SDK backend: the task runs through the SDK or does not
/// start, with the reason. It never falls back to `claude --bg`.
pub(crate) async fn launch(domain: &DomainRuntime, launch: Launch) -> Result<serde_json::Value, DomainError> {
    let Some((backend, runtime)) = domain.sdk() else {
        return Err(DomainError::new("unavailable", "this daemon has no SDK backend"));
    };
    let backend = backend.clone();
    let task = launch.task.clone();
    let driver = tokio::task::spawn_blocking(move || start(runtime, &backend, launch))
        .await
        .map_err(|error| DomainError::new("launch_failed", format!("the SDK launch stopped: {error}")))?
        .map_err(|Failed(why)| DomainError::new("launch_failed", format!("the task did not start through the Agent SDK: {why}")))?;
    let agent = driver.agent().clone();
    domain.sdk_owners.lock().expect("SDK owners poisoned").push(Owner::spawn(driver));
    domain.live.forget_closed_task(&task.0);
    Ok(serde_json::json!({"agent_id": agent, "short_id": agent.0.chars().take(8).collect::<String>(), "task": task}))
}

/// Adopts the host of `task` a previous daemon left running: it reserves the next owner generation,
/// adopts the live host with the same session, reconciles every dispatch a crash left unresolved
/// against the receipt the host retained, and resumes from where the lifecycle waits. It launches
/// no process; if the host is gone the adoption fails and the execution is left as it is.
pub(crate) fn adopt(domain: Arc<DomainRuntime>, backend: &SdkBackend, task: TaskId) -> Result<Driver, Failed> {
    let state = domain.state().map_err(|error| Failed(format!("reading the domain: {error:#}")))?;
    let agent = state.tasks.get(&task).and_then(|t| t.agent_id.clone()).ok_or_else(|| Failed("the task has no node to adopt".into()))?;
    let scope = AuthorityScope::new(state.data_epoch.clone()).map_err(|error| Failed(format!("{error:?}")))?;
    let ((stamp, directory, unresolved), _) = domain.own(scope.clone(), |owner| owner.adopt(&task)).map_err(failed("reserving the next owner"))?;
    let grant = domain.seals.take(&task).owners.pop().ok_or_else(|| Failed("the reservation issued no owner grant".into()))?;
    let owner_generation = grant.generation();
    let mut worker = ClaudeWorker::configured(grant.binding().clone(), configuration(backend, &directory, stamp.instance().as_str(), false, None)?)
        .map_err(adapter("configuring the SDK worker"))?;
    let (adapter_state, _) = domain.own(scope.clone(), |owner| owner.adapter_state(&grant)).map_err(failed("reading the provider state"))?;
    domain.seals.take(&task);
    worker.adopt_owner(grant, adapter_state).map_err(adapter("adopting the SDK host"))?;
    let mut driver = Driver { domain, worker, scope, task: task.clone(), agent, permits: VecDeque::new(), awaiting: None };
    for command in unresolved {
        let handoff = driver.worker.reconcile(&command.command).map_err(adapter("reconciling a dispatch"))?;
        let receipt = match handoff {
            Handoff::Offered => DispatchReceipt::Handed,
            Handoff::RefusedBeforeOffer => DispatchReceipt::Rejected,
            Handoff::Uncertain => DispatchReceipt::Uncertain,
        };
        let stamp = stamp.clone();
        let intent = command.intent_id;
        driver.domain.own(driver.scope.clone(), |owner| owner.resolve(&stamp, intent, owner_generation, receipt)).map_err(failed("resolving a dispatch"))?;
    }
    driver.sealed();
    let read = driver.domain.read();
    driver.awaiting = read.state().lifecycle_journal.get(&task).and_then(|journal| replay(&task, journal).ok())
        .and_then(|lifecycle| lifecycle.pending_inputs().first().map(|input| (*input).clone()));
    drop(read);
    Ok(driver)
}

/// At startup, adopts the live host of every task that was running through the SDK and is not
/// terminal. A host that is gone leaves its execution as it is, with the reason in the log.
pub(crate) fn adopt_all(domain: &Arc<DomainRuntime>, backend: &SdkBackend) {
    let Ok(state) = domain.state() else { return };
    let tasks: Vec<TaskId> = state
        .tasks
        .values()
        .filter(|task| {
            task.agent_id.is_some()
                && state.lifecycle_journal.get(&task.id).and_then(|journal| replay(&task.id, journal).ok()).is_some_and(|lifecycle| {
                    lifecycle.execution().is_some_and(|stamp| stamp.host().as_str() == HOST)
                        && !matches!(lifecycle.phase(), alondra_domain::TaskPhase::Completed | alondra_domain::TaskPhase::Cancelled)
                })
        })
        .map(|task| task.id.clone())
        .collect();
    for task in tasks {
        match adopt(Arc::clone(domain), backend, task.clone()) {
            Ok(driver) => {
                domain.sdk_owners.lock().expect("SDK owners poisoned").push(Owner::spawn(driver));
                domain.live.forget_closed_task(&task.0);
                log!("the SDK host of {task} was adopted: the same Claude keeps its session");
            }
            Err(Failed(why)) => log!("the SDK host of {task} was not adopted and its execution is left as it is: {why}"),
        }
    }
}

/// The host's folder is `state_root/<instance>`, so the execution's own identity names it and a
/// daemon that comes back finds it without searching. A launch creates it; an adoption reuses it.
fn configuration(backend: &SdkBackend, directory: &str, instance: &str, create: bool, worker: Option<WorkerProfile>) -> Result<ClaudeConfiguration, Failed> {
    let state_directory = backend.state_root.join(instance);
    if create {
        std::fs::create_dir_all(&backend.state_root).map_err(|error| Failed(format!("creating the SDK state root: {error}")))?;
        use std::os::unix::fs::DirBuilderExt;
        std::fs::DirBuilder::new()
            .mode(0o700)
            .create(&state_directory)
            .map_err(|error| Failed(format!("creating the host's folder: {error}")))?;
    }
    Ok(ClaudeConfiguration {
        artifact: VerifiedArtifact::verify(&backend.node, &backend.host, &backend.node_sha256, &backend.host_sha256)
            .map_err(|error| Failed(format!("verifying the SDK host: {error}")))?,
        state_directory,
        host_environment: backend.host_environment.clone(),
        native_executable: backend.native.clone(),
        native_directory: directory.into(),
        native_environment: backend.native_environment.clone(),
        profile_id: "alondra".into(),
        account_id: "native-login".into(),
        host_release: env!("CARGO_PKG_VERSION").into(),
        io_timeout: Duration::from_secs(backend.io_timeout_seconds),
        confinement: backend.confinement.clone(),
        worker,
    })
}

/// How the task's Claude reaches Alondra through the node plugin's MCP server: as its node, with
/// the capability issued to it, by the daemon's own address. The plugin's hooks stay inert: the
/// host reports what Claude does.
fn node_environment(home: &std::path::Path, agent: &AgentId, capability: &str) -> BTreeMap<String, String> {
    let mut environment = BTreeMap::from([
        ("ALONDRA_HOME".to_owned(), home.display().to_string()),
        ("ALONDRA_SESSION_ID".to_owned(), agent.0.clone()),
        ("ALONDRA_DOMAIN_CAPABILITY".to_owned(), capability.to_owned()),
        ("ALONDRA_DOMAIN_PROTOCOL".to_owned(), "2".to_owned()),
        ("ALONDRA_SDK_HOST".to_owned(), "1".to_owned()),
    ]);
    for name in ["ALONDRA_PORT", "ALONDRA_SANDBOX"] {
        if let Ok(value) = std::env::var(name) {
            environment.insert(name.to_owned(), value);
        }
    }
    environment
}

impl Driver {
    pub(crate) fn agent(&self) -> &AgentId {
        &self.agent
    }

    /// What the store issued for this task since the last look; its permits wait their turn.
    fn sealed(&mut self) -> Committed {
        let mut sealed = self.domain.seals.take(&self.task);
        self.permits.extend(std::mem::take(&mut sealed.permits));
        sealed
    }

    /// Claims the dispatch, records the exact command under the owner, hands it to the host
    /// with no SQL held, and records how the handoff went.
    fn dispatch(&mut self, permit: DispatchPermit) -> Result<(), Failed> {
        let input = permit.input().clone();
        match self.domain.own(self.scope.clone(), |owner| owner.claim(permit)) {
            Ok(_) => {}
            Err(StoreError::Storage(error)) if stale(&error) => return Ok(()),
            Err(error) => return Err(failed("claiming the dispatch")(error)),
        }
        let attempt = self.sealed().attempts.pop().ok_or_else(|| Failed("the claim issued no attempt".into()))?;
        let generation = self.worker.owner_generation().map_err(adapter("reading the owner generation"))?;
        let worker = &mut self.worker;
        self.domain
            .own(self.scope.clone(), |owner| owner.record_command(attempt, generation, worker))
            .map_err(failed("recording the command"))?;
        let command = self.sealed().commands.pop().ok_or_else(|| Failed("the command left no seal".into()))?;
        let handoff = self.worker.submit(&command).unwrap_or(Handoff::Uncertain);
        let receipt = match handoff {
            Handoff::Offered => DispatchReceipt::Handed,
            Handoff::RefusedBeforeOffer => DispatchReceipt::Rejected,
            Handoff::Uncertain => DispatchReceipt::Uncertain,
        };
        self.domain
            .own(self.scope.clone(), |owner| owner.record_receipt(command, receipt))
            .map_err(failed("recording the handoff"))?;
        self.sealed();
        if handoff == Handoff::Offered {
            self.awaiting = Some(input);
        }
        Ok(())
    }

    /// The next thing the host observed, committed with the domain's decision, and acknowledged
    /// only with the seal of a commit readers already see.
    pub(crate) fn observe(&mut self) -> Result<alondra_store::ObservationAcceptance, Failed> {
        let batch = self.worker.poll().map_err(adapter("reading the SDK host"))?.ok_or_else(|| Failed("the SDK host closed".into()))?;
        let generation = self.worker.owner_generation().map_err(adapter("reading the owner generation"))?;
        let binding = self.worker.binding().clone();
        let worker = &self.worker;
        let (acceptance, _) = self.domain
            .own(self.scope.clone(), |owner| owner.stage_observation(&binding, generation, &batch, worker.normalizer()))
            .map_err(failed("committing the observation"))?;
        for observation in self.sealed().observations {
            #[cfg(test)]
            self.domain.before_acknowledge();
            self.worker.acknowledge(&observation).map_err(adapter("acknowledging the observation"))?;
        }
        Ok(acceptance)
    }

    /// Whether the task's lifecycle waits for the host: an input it was handed and has not taken
    /// yet, or a turn or work of its own still going. Facts decide it, never a clock.
    fn expects_host(&mut self) -> Result<bool, Failed> {
        let state = Arc::clone(self.domain.read().state());
        let journal = state.lifecycle_journal.get(&self.task).ok_or_else(|| Failed("the task has no lifecycle".into()))?;
        let lifecycle = replay(&self.task, journal).map_err(|error| Failed(format!("{error:#}")))?;
        if self.awaiting.as_ref().is_some_and(|input| !lifecycle.pending_inputs().contains(&input)) {
            self.awaiting = None;
        }
        Ok(self.awaiting.is_some()
            || matches!(lifecycle.activity(), ActivityView::Starting | ActivityView::Working | ActivityView::OwnWork | ActivityView::Continuation))
    }

    /// One step: send what is waiting, else read the host when the lifecycle waits for it, else
    /// wait for a dispatch the store issues for this task.
    pub(crate) fn step(&mut self, stopped: &dyn Fn() -> bool) -> Result<(), Failed> {
        self.sealed();
        if let Some(permit) = self.permits.pop_front() {
            return self.dispatch(permit);
        }
        if self.expects_host()? {
            return self.observe().map(drop);
        }
        let sealed = self.domain.seals.wait(&self.task, stopped);
        self.permits.extend(sealed.permits);
        Ok(())
    }

    #[cfg(test)]
    pub(crate) fn rebind(&mut self, domain: Arc<DomainRuntime>) {
        self.domain = domain;
    }

    /// Claims and records the next waiting dispatch but does not send it: the crash state a daemon
    /// leaves when it dies after recording a command and before the host receives it.
    #[cfg(test)]
    pub(crate) fn claim_and_record_without_sending(&mut self) -> bool {
        self.sealed();
        let Some(permit) = self.permits.pop_front() else {
            return false;
        };
        self.domain.own(self.scope.clone(), |owner| owner.claim(permit)).unwrap();
        let attempt = self.sealed().attempts.pop().expect("the claim issued an attempt");
        let generation = self.worker.owner_generation().unwrap();
        let worker = &mut self.worker;
        self.domain.own(self.scope.clone(), |owner| owner.record_command(attempt, generation, worker)).unwrap();
        self.sealed();
        true
    }

    /// Claims, records and sends the next waiting dispatch to the host, but does not record its
    /// receipt: the crash state a daemon leaves when it dies after the host offered the input.
    #[cfg(test)]
    pub(crate) fn claim_record_and_offer_without_receipt(&mut self) -> bool {
        self.sealed();
        let Some(permit) = self.permits.pop_front() else {
            return false;
        };
        self.domain.own(self.scope.clone(), |owner| owner.claim(permit)).unwrap();
        let attempt = self.sealed().attempts.pop().expect("the claim issued an attempt");
        let generation = self.worker.owner_generation().unwrap();
        let worker = &mut self.worker;
        self.domain.own(self.scope.clone(), |owner| owner.record_command(attempt, generation, worker)).unwrap();
        let command = self.sealed().commands.pop().expect("the command left a seal");
        assert_eq!(self.worker.submit(&command).unwrap(), Handoff::Offered, "the host offered the input");
        true
    }

    #[cfg(test)]
    pub(crate) fn pending_permits(&mut self) -> usize {
        self.sealed();
        self.permits.len()
    }

    #[cfg(test)]
    pub(crate) fn waits_for_host(&mut self) -> bool {
        self.expects_host().unwrap()
    }

    pub(crate) fn stop(&mut self) -> Result<(), Failed> {
        self.worker.stop().map_err(adapter("stopping the SDK host"))
    }
}

fn stale(error: &anyhow::Error) -> bool {
    matches!(
        error.downcast_ref::<alondra_store::Error>(),
        Some(alondra_store::Error::StalePermit)
    )
}

/// Runs the owner on a thread of its own until it is told to stop or the host goes.
pub(crate) struct Owner {
    stopped: Arc<AtomicBool>,
    domain: Arc<DomainRuntime>,
    host: Option<alondra_agent_runtime::owned_host::HostStop>,
    agent: AgentId,
    thread: Option<std::thread::JoinHandle<Result<(), Failed>>>,
}

impl Owner {
    pub(crate) fn spawn(mut driver: Driver) -> Self {
        let stopped = Arc::new(AtomicBool::new(false));
        let domain = Arc::clone(&driver.domain);
        let host = driver.worker.stopper();
        let agent = driver.agent.clone();
        let flag = Arc::clone(&stopped);
        let thread = std::thread::Builder::new()
            .name(format!("sdk-owner-{}", driver.task))
            .spawn(move || {
                let told = || flag.load(Ordering::SeqCst);
                while !told() {
                    if let Err(error) = driver.step(&told) {
                        log!("the SDK owner of {} stopped: {error}", driver.task);
                        break;
                    }
                }
                // Told to stop, the owner stops its host and waits for it. Ended otherwise, it
                // detaches and leaves the host and its Claude running for a daemon that comes back
                // to adopt, unless the host it launched has already exited: then it ends what is
                // left of the host's group and reaps it.
                if !told() && !driver.worker.host_exited() {
                    return Ok(());
                }
                let stopped = driver.stop();
                if let Err(error) = &stopped {
                    log!("the SDK host of {} did not stop cleanly: {error}", driver.task);
                }
                stopped
            })
            .expect("spawning the SDK owner");
        Self { stopped, domain, host, agent, thread: Some(thread) }
    }

    pub(crate) fn agent(&self) -> &AgentId {
        &self.agent
    }

    /// Tells the owner to stop, asks its host to stop so a read blocked on it returns, and waits
    /// until the owner has seen the host exit and its whole process group end. An error says the
    /// group did not empty, or the owner could not see it end.
    pub(crate) fn stop(&mut self) -> Result<(), Failed> {
        self.stopped.store(true, Ordering::SeqCst);
        self.domain.seals.wake();
        if let Some(host) = &self.host
            && let Err(error) = host.request()
        {
            log!("could not ask the SDK host of {} to stop: {error}", self.agent);
        }
        match self.thread.take().map(std::thread::JoinHandle::join) {
            None => Ok(()),
            Some(Ok(stopped)) => stopped,
            Some(Err(_)) => Err(Failed(format!("the SDK owner of {} panicked", self.agent))),
        }
    }
}

impl Drop for Owner {
    fn drop(&mut self) {
        let _ = self.stop();
    }
}

#[cfg(test)]
#[path = "sdk_owner/tests.rs"]
mod tests;
