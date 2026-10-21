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
use alondra_provider_claude::{ClaudeConfiguration, ClaudeWorker, Found, Retained, WorkerProfile, answered_by_event, answered_control, context_event, native_exited};
use alondra_provider_contract::{AdapterError, Capability, Handoff, OrderedObservation, PreparedCommand, ProviderBinding};
use alondra_store::{AuthorityScope, Committed, ControlPermit, DispatchAttempt, DispatchPermit, DispatchReceipt};
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

/// The host an SDK execution runs on, as the domain records it: never `claude-bg`.
pub(crate) const HOST: &str = "alondra-sdk";
const DRIVER: &str = "claude-agent-sdk";
/// How long the owner waits for the host before it looks again for a control to send: a quiet
/// turn never holds back an interrupt.
const WAKE: Duration = Duration::from_millis(250);

/// What an SDK execution's binding lets the domain ask of it.
fn capabilities() -> BTreeSet<Capability> {
    BTreeSet::from([
        Capability::OfferInput,
        Capability::Interrupt,
        Capability::SetModel,
        Capability::SetPermissionMode,
        Capability::Compact,
        Capability::McpStatus,
        Capability::Decisions,
        Capability::McpReconnect,
        Capability::McpSignIn,
        Capability::ForceStop,
    ])
}

fn receipt(handoff: Handoff) -> DispatchReceipt {
    match handoff {
        Handoff::Offered => DispatchReceipt::Handed,
        Handoff::RefusedBeforeOffer => DispatchReceipt::Rejected,
        Handoff::Uncertain => DispatchReceipt::Uncertain,
    }
}

/// Why nothing starts on a daemon without an SDK backend, which only a broken install has.
pub(crate) const NO_HOST: &str = "this Alondra has no Agent SDK host, so no task or orchestrator batch starts: reinstall Alondra (`brew reinstall alondra`)";

/// Whether `agent` holds the current execution of a task that runs through the SDK.
pub(crate) fn runs_through_sdk(state: &DomainState, agent: &AgentId) -> bool {
    state.tasks.values().filter(|task| task.agent_id.as_ref() == Some(agent)).any(|task| {
        state
            .lifecycle_of(&task.id).ok()
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
    pub(crate) options: LaunchOptions,
    /// The model the launch chose, recorded on the task when its execution is admitted.
    pub(crate) model: Option<TaskModel>,
}

/// What the task's Claude is launched with besides its prompt: the model and effort the launch
/// resolved and the tools its request denies. Kept beside the execution's host, so a host that
/// resumes the session launches Claude the same way.
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub(crate) struct LaunchOptions {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) effort: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(crate) disallowed_tools: Vec<String>,
}

const LAUNCH_OPTIONS: &str = "launch-options.json";

impl LaunchOptions {
    /// The effort levels the Agent SDK takes; any other a launch resolved is left to Claude.
    fn effort(&self) -> Option<String> {
        self.effort.clone().filter(|effort| matches!(effort.as_str(), "low" | "medium" | "high" | "xhigh" | "max"))
    }

    fn save(&self, state_directory: &std::path::Path) -> Result<(), Failed> {
        let text = serde_json::to_vec(self).map_err(|error| Failed(format!("the launch options: {error}")))?;
        std::fs::write(state_directory.join(LAUNCH_OPTIONS), text).map_err(|error| Failed(format!("keeping the launch options: {error}")))
    }

    fn load(state_directory: &std::path::Path) -> Result<Self, Failed> {
        match std::fs::read(state_directory.join(LAUNCH_OPTIONS)) {
            Ok(bytes) => serde_json::from_slice(&bytes).map_err(|error| Failed(format!("reading the launch options: {error}"))),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(error) => Err(Failed(format!("reading the launch options: {error}"))),
        }
    }
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

/// One execution's owner: its worker and the task whose dispatches and controls only it claims.
pub(crate) struct Driver {
    domain: Arc<DomainRuntime>,
    worker: ClaudeWorker,
    scope: AuthorityScope,
    task: TaskId,
    agent: AgentId,
    permits: VecDeque<DispatchPermit>,
    controls: VecDeque<ControlPermit>,
    /// The input last handed to the host, until the task's lifecycle no longer waits for it.
    awaiting: Option<InputId>,
    /// The host was launched to replace one that ended and has been handed nothing yet: it runs no
    /// Claude, so it has nothing to say whatever the lifecycle still waits for.
    replacement: bool,
    /// Controls the host took whose answer is an event of its own, until that event is staged.
    results: Vec<alondra_foundation::CommandId>,
    /// The host closed its end while this daemon ran: the fact that ends this driver.
    host_closed: bool,
    /// A step of this owner failed: its host is stopped and replaced like one that closed.
    owner_failed: bool,
    #[cfg(test)]
    pub(crate) fail_next_step: bool,
}

/// Admits the execution, starts its host and hands it the launch: once this returns, the task
/// runs through the SDK, and any failure before it is the launch's own, never a `claude --bg`.
pub(crate) fn start(domain: Arc<DomainRuntime>, backend: &SdkBackend, launch: Launch) -> Result<Driver, Failed> {
    start_bound(domain, backend, launch, capabilities(), None)
}

/// The `claude --bg` node whose session an SDK execution continues: its id, which is the session.
pub(crate) struct Continuation {
    pub(crate) node: AgentId,
}

/// Admits an SDK execution that continues the existing Claude session of `continuation`'s node, in
/// the node's own folder, and records the move's admission in the same transaction. The launch's
/// prompt is the first input: it opens that session with `resume`.
pub(crate) fn continue_session(domain: Arc<DomainRuntime>, backend: &SdkBackend, launch: Launch, continuation: Continuation) -> Result<Driver, Failed> {
    start_bound(domain, backend, launch, capabilities(), Some(continuation))
}

fn start_bound(domain: Arc<DomainRuntime>, backend: &SdkBackend, launch: Launch, capabilities: BTreeSet<Capability>, continuation: Option<Continuation>) -> Result<Driver, Failed> {
    let backend = &backend.for_start().map_err(|error| Failed(format!("{error:#}")))?;
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
    let binding = binding(backend, capabilities)?;
    let mut configuration = configuration(backend, &launch.directory, &agent.0, true, Some(worker_profile(&launch.plugin, &launch.options, &launch.directory)))?;
    launch.options.save(&configuration.state_directory)?;
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
    if let Some(continuation) = &continuation {
        worker.continue_session(&continuation.node.0).map_err(adapter("continuing the node's session"))?;
    }
    let admit = envelope("admit", Command::AdmitExecution {
        task_id: launch.task.clone(),
        agent_id: agent.clone(),
        conversation: format!("sdk-conversation-{suffix}"),
        instance: agent.0.clone(),
        host: HOST.into(),
        model: launch.model.clone(),
    });
    let (first, _) = domain
        .own(scope.clone(), |owner| {
            owner.stage_command(&Actor::System, &admit, Attached::None)?;
            owner.launch(&launch.task, &launch.prompt, &launch.directory, &binding, &mut worker)?;
            if let Some(continuation) = &continuation {
                owner.record_sdk_admitted(&continuation.node, &launch.task, &agent)?;
            }
            let lifecycle = owner.lifecycle(&launch.task)?;
            Ok(lifecycle.pending_inputs().first().map(|input| (*input).clone()))
        })
        .map_err(failed("admitting the execution"))?;
    #[cfg(test)]
    if continuation.is_some() && *domain.handovers.crash_after.lock().unwrap() == Some(crate::domain_store::HandoverStep::SdkAdmitted) {
        return Err(Failed("the daemon died after the admission's commit".into()));
    }
    let mut driver = Driver::new(domain, worker, scope, launch.task, agent);
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

/// One binding per SDK configuration, named by its digest: the store keeps one active binding per
/// provider instance, so a binding of each launch would revoke every task after the first, and a
/// configuration that changes (a new release's host) binds the next executions while the earlier
/// ones keep the binding they were admitted under.
fn binding(backend: &SdkBackend, capabilities: BTreeSet<Capability>) -> Result<ProviderBinding, Failed> {
    let configuration = backend.configuration_digest();
    let short = &configuration[..16];
    ProviderBinding::new(
        format!("{DRIVER}-{HOST}-{short}"),
        DRIVER.into(),
        format!("{HOST}-{short}"),
        configuration,
        1,
        3,
        capabilities,
        "native-login".into(),
        1,
    )
    .map_err(|error| Failed(format!("the provider binding: {error:?}")))
}

/// The launch of a daemon with an SDK backend: the task runs through the SDK or does not
/// start, with the reason. It never falls back to `claude --bg`.
pub(crate) async fn launch(domain: &DomainRuntime, launch: Launch) -> Result<serde_json::Value, DomainError> {
    let Some((backend, runtime)) = domain.sdk() else {
        return Err(DomainError::new("unavailable", NO_HOST));
    };
    let backend = backend.clone();
    let task = launch.task.clone();
    let driver = tokio::task::spawn_blocking(move || start(runtime, &backend, launch))
        .await
        .map_err(|error| DomainError::new("launch_failed", format!("the SDK launch stopped: {error}")))?
        .map_err(|Failed(why)| DomainError::new("launch_failed", format!("the task did not start through the Agent SDK: {why}")))?;
    let agent = driver.agent().clone();
    domain.add_sdk_owner(Owner::spawn(driver));
    domain.live.forget_closed_task(&task.0);
    Ok(serde_json::json!({"agent_id": agent, "short_id": agent.0.chars().take(8).collect::<String>(), "task": task}))
}

/// Takes over the execution of `task` a previous daemon left: it reserves the next owner
/// generation and finds the execution's host. A host that still runs is adopted with the same
/// session, and every dispatch a crash left unresolved is reconciled against the receipt it
/// retained. A host that ended (no process holds its lock, as after the Mac restarts) is replaced:
/// the evidence its journal kept is committed, each unresolved dispatch is resolved from it, and a
/// fresh host is launched that resumes the same native session on the next input. `plugin` is
/// Alondra's node plugin, which that fresh host's Claude loads.
pub(crate) fn adopt(domain: Arc<DomainRuntime>, backend: &SdkBackend, task: TaskId, plugin: Option<&std::path::Path>) -> Result<Driver, Failed> {
    take(domain, backend, task, plugin, true)?.ok_or_else(|| Failed("the ended host was not replaced".into()))
}

/// Launches the first host of the task's SDK execution, which a daemon that ended right after
/// admitting it never started (its folder holds no host): the launch it never sent goes now, with
/// a new owner generation.
pub(crate) fn launch_admitted(domain: Arc<DomainRuntime>, backend: &SdkBackend, task: TaskId, plugin: &std::path::Path) -> Result<Driver, Failed> {
    let backend = &backend.for_start().map_err(|error| Failed(format!("{error:#}")))?;
    let state = domain.state().map_err(|error| Failed(format!("reading the domain: {error:#}")))?;
    let agent = state.tasks.get(&task).and_then(|t| t.agent_id.clone()).ok_or_else(|| Failed("the task has no node".into()))?;
    let scope = AuthorityScope::new(state.data_epoch.clone()).map_err(|error| Failed(format!("{error:?}")))?;
    let (adoption, _) = domain
        .own(scope.clone(), |owner| {
            let adoption = owner.adopt(&task)?;
            owner.reissue_permit(&task)?;
            Ok(adoption)
        })
        .map_err(failed("reserving the next owner"))?;
    let mut sealed = domain.seals.take(&task);
    let grant = sealed.owners.pop().ok_or_else(|| Failed("the reservation issued no owner grant".into()))?;
    let permit = sealed.permits.pop().ok_or_else(|| Failed("the reservation issued no launch permit".into()))?;
    let instance = adoption.stamp.instance().as_str().to_owned();
    let options = LaunchOptions::load(&backend.state_root.join(&instance))?;
    let mut configuration = configuration(backend, &adoption.directory, &instance, false, Some(worker_profile(plugin, &options, &adoption.directory)))?;
    let capability = domain.bind_agent(&agent.0).map_err(|error| Failed(format!("issuing the node's capability: {error}")))?;
    configuration.native_environment.extend(node_environment(domain.home(), &agent, capability.expose_for_child_environment()));
    let mut worker = ClaudeWorker::configured(grant.binding().clone(), configuration).map_err(adapter("configuring the SDK worker"))?;
    let (adapter_state, _) = domain.own(scope.clone(), |owner| owner.adapter_state(&grant)).map_err(failed("reading the provider state"))?;
    domain.seals.take(&task);
    worker.install_owner(grant, adapter_state).map_err(adapter("starting the SDK host"))?;
    let mut driver = Driver::new(domain, worker, scope, task.clone(), agent);
    driver.dispatch(permit)?;
    let read = driver.domain.read();
    driver.awaiting = read.state().lifecycle_of(&task).ok()
        .and_then(|lifecycle| lifecycle.pending_inputs().first().map(|input| (*input).clone()));
    drop(read);
    Ok(driver)
}

/// Whether the host of the task's current SDK execution was ever started: its folder holds the
/// lock every host takes, which stays after it ends.
pub(crate) fn host_started(backend: &SdkBackend, domain: &DomainRuntime, task: &TaskId) -> bool {
    let read = domain.read();
    let instance = read.state().lifecycle_of(task).ok()
        .and_then(|lifecycle| lifecycle.execution().map(|stamp| stamp.instance().as_str().to_owned()));
    instance.is_none_or(|instance| backend.state_root.join(instance).join("host.lock").exists())
}

/// Ends the task's host, which no owner of this daemon holds (its owner stopped it, or the daemon
/// that owned it is gone), and commits what became of it: the evidence its journal kept, each
/// dispatch resolved from it, and the host's end, which closes its open requests and their cards.
/// A host that still runs is ended by the lock it holds. No host takes its place and no Claude
/// session is resumed: the task's next execution, if any, is a new one.
pub(crate) fn end_unowned_host(domain: Arc<DomainRuntime>, backend: &SdkBackend, task: TaskId) -> Result<(), Failed> {
    take(domain, backend, task, None, false).map(drop)
}

fn take(domain: Arc<DomainRuntime>, backend: &SdkBackend, task: TaskId, plugin: Option<&std::path::Path>, relaunch: bool) -> Result<Option<Driver>, Failed> {
    take_host(domain, backend, task, plugin, relaunch, true)
}

/// A live host that refuses its adoption (its owner's socket is gone, it runs another release's
/// artifact, its bootstrap no longer matches) can never be reached again: with `end_refused`, it is
/// ended by the lock it holds and the execution is taken again, now with a host that ended.
fn take_host(domain: Arc<DomainRuntime>, backend: &SdkBackend, task: TaskId, plugin: Option<&std::path::Path>, relaunch: bool, end_refused: bool) -> Result<Option<Driver>, Failed> {
    let backend = &backend.for_start().map_err(|error| Failed(format!("{error:#}")))?;
    let state = domain.state().map_err(|error| Failed(format!("reading the domain: {error:#}")))?;
    let agent = state.tasks.get(&task).and_then(|t| t.agent_id.clone()).ok_or_else(|| Failed("the task has no node to adopt".into()))?;
    let scope = AuthorityScope::new(state.data_epoch.clone()).map_err(|error| Failed(format!("{error:?}")))?;
    let (adoption, _) = domain
        .own(scope.clone(), |owner| {
            let adoption = owner.adopt(&task)?;
            if relaunch {
                owner.reissue_permit(&task)?;
            }
            Ok(adoption)
        })
        .map_err(failed("reserving the next owner"))?;
    let mut reserved = domain.seals.take(&task);
    let grant = reserved.owners.pop().ok_or_else(|| Failed("the reservation issued no owner grant".into()))?;
    let owner_generation = grant.generation();
    let stamp = adoption.stamp;
    let options = LaunchOptions::load(&backend.state_root.join(stamp.instance().as_str()))?;
    let profile = plugin.map(|plugin| worker_profile(plugin, &options, &adoption.directory));
    let mut worker = ClaudeWorker::configured(grant.binding().clone(), configuration(backend, &adoption.directory, stamp.instance().as_str(), false, profile)?)
        .map_err(adapter("configuring the SDK worker"))?;
    let (adapter_state, _) = domain.own(scope.clone(), |owner| owner.adapter_state(&grant)).map_err(failed("reading the provider state"))?;
    domain.seals.take(&task);
    let journal = match worker.find_host(&grant, &adapter_state).map_err(adapter("finding the SDK host"))? {
        Found::Live => None,
        Found::Ended(journal) => Some(journal),
    };
    let Some(journal) = journal else {
        if !relaunch {
            if !end_refused {
                return Err(Failed("the host still runs".into()));
            }
            log!("the SDK host of {task} still runs with no owner: it is stopped without resuming its Claude session");
            drop(worker);
            alondra_agent_runtime::owned_host::EndedHost::end_holders(&backend.state_root.join(stamp.instance().as_str()))
                .map_err(|error| Failed(format!("its host did not stop: {error}")))?;
            return take_host(domain, backend, task, plugin, relaunch, false);
        }
        if let Err(error) = worker.adopt_owner(grant, adapter_state) {
            if !end_refused {
                return Err(adapter("adopting the SDK host")(error));
            }
            log!("the live SDK host of {task} refused its adoption ({error:?}): it is stopped and a host that resumes the same Claude session takes its place");
            drop(worker);
            alondra_agent_runtime::owned_host::EndedHost::end_holders(&backend.state_root.join(stamp.instance().as_str()))
                .map_err(|error| Failed(format!("its host refused to be taken over and did not stop: {error}")))?;
            return take_host(domain, backend, task, plugin, relaunch, false);
        }
        let mut driver = Driver::new(domain, worker, scope, task.clone(), agent);
        driver.permits.extend(reserved.permits);
        for command in adoption.commands {
            let handoff = driver.worker.reconcile(&command.command).map_err(adapter("reconciling a dispatch"))?;
            let intent = command.intent_id;
            driver.domain.own(driver.scope.clone(), |owner| owner.resolve(&stamp, intent, owner_generation, receipt(handoff))).map_err(failed("resolving a dispatch"))?;
        }
        for control in adoption.controls {
            let handoff = driver.worker.reconcile(&control.command).map_err(adapter("reconciling a control"))?;
            driver.domain.own(driver.scope.clone(), |owner| owner.resolve_control(&stamp, control.id, owner_generation, receipt(handoff))).map_err(failed("resolving a control"))?;
            if handoff == Handoff::Offered {
                driver.results.extend(answered_by_event(&control.command));
            }
        }
        driver.sealed();
        let read = driver.domain.read();
        driver.awaiting = read.state().lifecycle_of(&task).ok()
            .and_then(|lifecycle| lifecycle.pending_inputs().first().map(|input| (*input).clone()));
        drop(read);
        return Ok(Some(driver));
    };
    if relaunch && plugin.is_none() {
        return Err(Failed("the host ended and Alondra's node plugin, which its replacement loads, was not found".into()));
    }
    for event in journal.events() {
        let batch = OrderedObservation { owner_generation, execution: stamp.clone(), cursor: event.cursor, evidence: event.payload.clone() };
        let binding = grant.binding().clone();
        let (normalizer, home) = (worker.normalizer(), domain.home());
        commit_observation(&domain, &scope, &binding, owner_generation, &batch, normalizer, home)
            .map_err(failed("committing what the ended host observed"))?;
    }
    let retained = |command: &[u8]| -> Result<DispatchReceipt, Failed> {
        Ok(match journal.retained(command).map_err(adapter("reading the ended host's receipt"))? {
            Retained::Offered => DispatchReceipt::Handed,
            Retained::Refused | Retained::Absent => DispatchReceipt::Rejected,
            Retained::AdmittedOnly => DispatchReceipt::Uncertain,
        })
    };
    for command in adoption.commands {
        let receipt = retained(&command.command)?;
        domain.own(scope.clone(), |owner| owner.resolve(&stamp, command.intent_id, owner_generation, receipt)).map_err(failed("resolving a dispatch"))?;
    }
    for control in adoption.controls {
        let receipt = retained(&control.command)?;
        domain.own(scope.clone(), |owner| owner.resolve_control(&stamp, control.id, owner_generation, receipt)).map_err(failed("resolving a control"))?;
    }
    let generation = stamp.generation();
    domain.own(scope.clone(), |owner| {
        let envelope = |name: String, command: Command| CommandEnvelope {
            protocol_version: PROTOCOL_VERSION,
            data_epoch: state.data_epoch.clone(),
            command_id: format!("host-ended:{task}:{generation}:{owner_generation}:{name}").as_str().into(),
            expected_revision: 0,
            command,
        };
        owner.stage_command(&Actor::System, &envelope("ended".into(), Command::RecordHostEnded { task_id: task.clone(), generation, resumes: relaunch }), Attached::None)?;
        let doubted: Vec<InputId> = owner.lifecycle(&task)?.doubts().into_iter().map(|(_, input)| input.clone()).collect();
        for (input, status) in owner.pending_handoffs(&task)? {
            let (id, text) = (input.id().as_str().to_owned(), input.text().to_owned());
            let command = match status.as_str() {
                "rejected" => Command::ResendUntakenInput { task_id: task.clone(), generation, input: id.clone(), text },
                _ if doubted.contains(input.id()) => continue,
                _ => Command::RecordDoubtfulHandoff { task_id: task.clone(), generation, input: id.clone(), text, resumes: relaunch },
            };
            owner.stage_command(&Actor::System, &envelope(id, command), Attached::None)?;
        }
        Ok(())
    })
    .map_err(failed("recording the end of the host"))?;
    if !relaunch {
        return Ok(None);
    }
    let (adapter_state, _) = domain.own(scope.clone(), |owner| owner.adapter_state(&grant)).map_err(failed("reading the provider state"))?;
    let base = adapter_state.checkpoint().map(|checkpoint| checkpoint.cursor()).unwrap_or(0);
    let resume = domain.read().state().lifecycle_of(&task).ok()
        .is_some_and(|lifecycle| lifecycle.resource() == Some(alondra_domain::ResourceEvidence::Available));
    let capability = domain.bind_agent(&agent.0).map_err(|error| Failed(format!("issuing the node's capability: {error}")))?;
    let environment = node_environment(domain.home(), &agent, capability.expose_for_child_environment());
    worker.relaunch(grant, adapter_state, base, resume, environment).map_err(|error| Failed(format!("its new host could not start ({error:?})")))?;
    let mut driver = Driver::new(domain, worker, scope, task, agent);
    driver.replacement = true;
    driver.sealed();
    Ok(Some(driver))
}

/// At startup, takes over every task that was running through the SDK and is not terminal: a live
/// host is adopted with the same session, and one that ended or refuses its adoption is replaced by
/// a host that resumes it.
pub(crate) fn adopt_all(domain: &Arc<DomainRuntime>, backend: &SdkBackend) {
    for (task, taken) in take_all(domain, backend) {
        match taken {
            Ok(driver) => {
                domain.add_sdk_owner(Owner::spawn(driver));
                domain.live.forget_closed_task(&task.0);
                log!("the SDK execution of {task} was taken over: the same Claude session goes on");
            }
            Err(Failed(why)) => lost(domain, &task, &why),
        }
    }
}

/// The task's execution could not be taken over and no host runs it: its card tells the user why
/// and offers to try again or cancel the task. It is never left as it is.
pub(crate) fn lost(domain: &DomainRuntime, task: &TaskId, why: &str) {
    log!("the SDK execution of {task} was not taken over: {why}");
    let Ok(state) = domain.state() else { return };
    let envelope = CommandEnvelope {
        protocol_version: PROTOCOL_VERSION,
        data_epoch: state.data_epoch.clone(),
        command_id: format!("host-lost-{task}-{}", crate::auth::random_hex(8)).into(),
        expected_revision: 0,
        command: Command::RecordHostLost { task_id: task.clone(), reason: why.to_owned() },
    };
    if let Err(error) = domain.execute_on_current(&envelope) {
        log!("could not put the card of {task}, whose execution was not taken over: {}", store_text(&error));
    }
}

/// The user chose «Try again» on the card of a task whose execution was lost: it is taken over
/// again, on a thread of its own, as at the daemon's start; another failure puts a new card.
pub(crate) fn retake(domain: &DomainRuntime, task: TaskId) {
    let Some((backend, runtime)) = domain.sdk() else {
        return lost(domain, &task, NO_HOST);
    };
    let backend = backend.clone();
    let mut retakes = domain.retakes.lock().expect("retakes poisoned");
    retakes.retain(|retake| !retake.is_finished());
    retakes.push(std::thread::spawn(move || {
        if runtime.state().ok().and_then(|state| state.tasks.get(&task).and_then(|task| task.agent_id.clone())).is_some_and(|agent| runtime.owns_sdk_host(&agent)) {
            return;
        }
        let plugin = crate::plugins::node_in(runtime.home());
        let taken = if host_started(&backend, &runtime, &task) {
            adopt(Arc::clone(&runtime), &backend, task.clone(), plugin.as_deref())
        } else {
            plugin.as_deref().ok_or_else(|| Failed("Alondra's node plugin, which its host loads, was not found".into()))
                .and_then(|plugin| launch_admitted(Arc::clone(&runtime), &backend, task.clone(), plugin))
        };
        match taken {
            Ok(driver) => {
                runtime.add_sdk_owner(Owner::spawn(driver));
                log!("the SDK execution of {task} was taken over again: the same Claude session goes on");
            }
            Err(Failed(why)) => lost(&runtime, &task, &why),
        }
    }));
}

/// Takes each execution on a thread of its own, so a host that is slow to stop holds back only
/// its own task.
fn take_all(domain: &Arc<DomainRuntime>, backend: &SdkBackend) -> Vec<(TaskId, Result<Driver, Failed>)> {
    let Ok(state) = domain.state() else { return Vec::new() };
    let tasks: Vec<TaskId> = state
        .tasks
        .values()
        .filter(|task| {
            task.agent_id.as_ref().is_some_and(|agent| !domain.owns_sdk_host(agent))
                && state.lifecycle_of(&task.id).ok().is_some_and(|lifecycle| {
                    lifecycle.execution().is_some_and(|stamp| stamp.host().as_str() == HOST)
                        && !matches!(lifecycle.phase(), alondra_domain::TaskPhase::Completed | alondra_domain::TaskPhase::Cancelled)
                })
        })
        .map(|task| task.id.clone())
        .collect();
    let plugin = crate::plugins::node_in(domain.home());
    std::thread::scope(|scope| {
        let taking: Vec<_> = tasks
            .into_iter()
            .map(|task| {
                let plugin = plugin.as_deref();
                let taking = task.clone();
                let thread = scope.spawn(move || {
                    if host_started(backend, domain, &taking) {
                        adopt(Arc::clone(domain), backend, taking, plugin)
                    } else {
                        plugin.ok_or_else(|| Failed("Alondra's node plugin, which its host loads, was not found".into()))
                            .and_then(|plugin| launch_admitted(Arc::clone(domain), backend, taking, plugin))
                    }
                });
                (task, thread)
            })
            .collect();
        taking.into_iter().map(|(task, thread)| (task, thread.join().unwrap_or_else(|_| Err(Failed("taking it over panicked".into()))))).collect()
    })
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

/// Alondra's node plugin and launch settings, which a task's Claude loads on top of the user's own.
/// Its Claude runs in the default permission mode, so the settings allow editing inside the node's
/// own worktree: an edit there never puts a permission card to the user.
fn worker_profile(plugin: &std::path::Path, options: &LaunchOptions, worktree: &str) -> WorkerProfile {
    let permissions = serde_json::json!({"allow": own_worktree_edits(std::path::Path::new(worktree))});
    WorkerProfile {
        plugins: vec![plugin.to_path_buf()],
        settings: crate::plugins::launch_settings_with(Some(permissions)).as_object().cloned().unwrap_or_default(),
        model: options.model.clone(),
        effort: options.effort(),
        disallowed_tools: options.disallowed_tools.clone(),
    }
}

/// Claude Code's `Edit` rules cover Edit, Write and NotebookEdit; `//` anchors a rule at the root
/// of the disk. The worktree is named as given and as its real path, when they differ. A path a
/// rule cannot spell literally gets no rule, and edits there ask as before.
pub(crate) fn own_worktree_edits(worktree: &std::path::Path) -> Vec<String> {
    let mut paths = vec![worktree.to_path_buf()];
    if let Ok(real) = worktree.canonicalize() {
        if real != worktree {
            paths.push(real);
        }
    }
    paths
        .iter()
        .filter_map(|path| path.to_str())
        .filter(|path| path.starts_with('/') && !path.contains(['*', '?', '[', ']', '{', '}', '(', ')', '!', '\\']))
        .map(|path| format!("Edit(/{}/**)", path.trim_end_matches('/')))
        .collect()
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
    fn new(domain: Arc<DomainRuntime>, worker: ClaudeWorker, scope: AuthorityScope, task: TaskId, agent: AgentId) -> Self {
        Self { domain, worker, scope, task, agent, permits: VecDeque::new(), controls: VecDeque::new(), awaiting: None, replacement: false, results: vec![], host_closed: false, owner_failed: false, #[cfg(test)] fail_next_step: false }
    }

    pub(crate) fn agent(&self) -> &AgentId {
        &self.agent
    }

    /// What the store issued for this task since the last look; its permits wait their turn.
    fn sealed(&mut self) -> Committed {
        let mut sealed = self.domain.seals.take(&self.task);
        self.permits.extend(std::mem::take(&mut sealed.permits));
        self.controls.extend(std::mem::take(&mut sealed.control_permits));
        sealed
    }

    /// Claims the control, records its exact command under the owner, hands it to the host with
    /// no SQL held, and records how the host answered. An interrupt whose turn or work already
    /// ended is revoked by its claim and never sent.
    fn control(&mut self, permit: ControlPermit) -> Result<(), Failed> {
        match self.domain.own(self.scope.clone(), |owner| owner.claim_control(permit)) {
            Ok((true, _)) => {}
            Ok((false, _)) => return Ok(()),
            Err(StoreError::Storage(error)) if stale(&error) => return Ok(()),
            Err(error) => return Err(failed("claiming the control")(error)),
        }
        let attempt = self.sealed().control_attempts.pop().ok_or_else(|| Failed("the claim issued no control attempt".into()))?;
        let generation = self.worker.owner_generation().map_err(adapter("reading the owner generation"))?;
        let worker = &mut self.worker;
        self.domain
            .own(self.scope.clone(), |owner| owner.record_control(attempt, generation, |attempt| worker.prepare_control(attempt)))
            .map_err(failed("recording the control"))?;
        let control = self.sealed().control_commands.pop().ok_or_else(|| Failed("the control left no seal".into()))?;
        let handoff = self.worker.submit_control(&control).unwrap_or(Handoff::Uncertain);
        let awaited = (handoff == Handoff::Offered).then(|| answered_by_event(control.bytes())).flatten();
        let accepted = match (handoff, control.attempt().operation()) {
            (Handoff::Offered, alondra_store::ControlOperation::Control(alondra_domain::Control::Interrupt { turn: Some(turn) })) => {
                Some((control.attempt().execution().clone(), control.attempt().control().clone(), turn.clone()))
            }
            _ => None,
        };
        let epoch = self.scope.epoch().clone();
        let recorded = self.domain
            .own(self.scope.clone(), |owner| {
                owner.record_control_receipt(control, receipt(handoff))?;
                let Some((stamp, id, turn)) = accepted else { return Ok(()) };
                let lifecycle = owner.lifecycle(stamp.task())?;
                if lifecycle.turn() != Some(&turn) || lifecycle.turn_stop() != Some(alondra_domain::TurnStop::Running) {
                    return Ok(());
                }
                let envelope = CommandEnvelope {
                    protocol_version: PROTOCOL_VERSION,
                    data_epoch: epoch,
                    command_id: format!("interrupt-accepted:{}", id.as_str()).as_str().into(),
                    expected_revision: 0,
                    command: Command::RecordInterruptAccepted { task_id: stamp.task().clone(), generation: stamp.generation(), turn: turn.as_str().into() },
                };
                if let Some(refusal) = owner.refusal(&envelope) {
                    log!("the interrupt Claude accepted on {} is left out: {}", stamp.task(), refusal.message);
                    return Ok(());
                }
                owner.stage_command(&Actor::System, &envelope, Attached::None).map(drop)
            });
        match recorded {
            Ok(_) => {}
            Err(StoreError::Storage(error)) if stale(&error) => log!("the handoff of a control to the SDK host of {} is no longer the store's to record", self.task),
            Err(error) => return Err(failed("recording the control's handoff")(error)),
        }
        self.sealed();
        self.results.extend(awaited);
        Ok(())
    }

    /// Claims the dispatch, records the exact command under the owner, hands it to the host
    /// with no SQL held, and records how the handoff went.
    fn dispatch(&mut self, permit: DispatchPermit) -> Result<(), Failed> {
        let input = permit.input().clone();
        match self.domain.own(self.scope.clone(), |owner| owner.claim(permit)) {
            Ok(_) => {}
            Err(StoreError::Storage(error)) if stale(&error) || out_of_turn(&error) => return Ok(()),
            Err(error) => return Err(failed("claiming the dispatch")(error)),
        }
        let attempt = self.sealed().attempts.pop().ok_or_else(|| Failed("the claim issued no attempt".into()))?;
        let generation = self.worker.owner_generation().map_err(adapter("reading the owner generation"))?;
        let worker = &mut self.worker;
        self.domain
            .own(self.scope.clone(), |owner| owner.record_command(attempt, generation, worker))
            .map_err(failed("recording the command"))?;
        let command = self.sealed().commands.pop().ok_or_else(|| Failed("the command left no seal".into()))?;
        self.domain.contexts.handed(&self.task, alondra_core::now_millis());
        let handoff = self.worker.submit(&command).unwrap_or(Handoff::Uncertain);
        if handoff == Handoff::Uncertain && self.worker.host_exited() {
            self.host_closed = true;
        }
        match self.domain.own(self.scope.clone(), |owner| owner.record_receipt(command, receipt(handoff))) {
            Ok(_) => {}
            Err(StoreError::Storage(error)) if stale(&error) => log!("the handoff of an input to the SDK host of {} is no longer the store's to record", self.task),
            Err(error) => return Err(failed("recording the handoff")(error)),
        }
        self.sealed();
        if handoff == Handoff::Offered {
            self.awaiting = Some(input);
            self.replacement = false;
        }
        Ok(())
    }

    /// The next thing the host observed, committed with the domain's decision, and acknowledged
    /// only with the seal of a commit readers already see.
    pub(crate) fn observe(&mut self) -> Result<alondra_store::ObservationAcceptance, Failed> {
        let batch = match self.worker.poll() {
            Ok(Some(batch)) => batch,
            Ok(None) => {
                self.host_closed = true;
                return Err(Failed("the SDK host closed".into()));
            }
            Err(error) => {
                self.host_closed = true;
                return Err(Failed(format!("the connection to the SDK host failed: {error:?}")));
            }
        };
        if native_exited(&batch) {
            self.host_closed = true;
            return Err(Failed("the SDK host's Claude exited".into()));
        }
        let generation = self.worker.owner_generation().map_err(adapter("reading the owner generation"))?;
        let binding = self.worker.binding().clone();
        let (worker, home) = (&self.worker, self.domain.home());
        let (acceptance, unrecorded) = commit_observation(&self.domain, &self.scope, &binding, generation, &batch, worker.normalizer(), home)
            .map_err(failed("committing the observation"))?;
        if let Some(answered) = answered_control(&batch) {
            self.results.retain(|awaited| *awaited != answered);
        }
        if let Some(event) = context_event(&batch) {
            self.domain.contexts.observed(&self.task, event, alondra_core::now_millis());
        }
        for observation in self.sealed().observations {
            #[cfg(test)]
            self.domain.before_acknowledge();
            self.worker.acknowledge(&observation).map_err(adapter("acknowledging the observation"))?;
        }
        if !unrecorded.is_empty() {
            // The task's record no longer follows its Claude: only that turn fails, cut by a new
            // host that resumes the same session, as one whose host ended does.
            self.owner_failed = true;
            return Err(Failed(format!("Alondra could not record what its Claude did ({}): its session goes on in a new host", unrecorded.join("; "))));
        }
        Ok(acceptance)
    }

    /// Whether the node is paused: what it is told waits for the user to resume it, or for their own
    /// words, which end the pause.
    fn paused(&self) -> bool {
        matches!(self.domain.read().state().node_phase(&self.agent), Some(NodePhase::Paused(pause)) if !pause.released)
    }

    /// Whether the task's lifecycle waits for the host: an input it was handed and has not taken
    /// yet, a turn or work of its own still going, or the answer of a control it took. Facts
    /// decide it, never a clock.
    fn expects_host(&mut self) -> Result<bool, Failed> {
        if self.replacement {
            return Ok(false);
        }
        let state = Arc::clone(self.domain.read().state());
        let lifecycle = state.lifecycle_of(&self.task).map_err(|error| Failed(format!("{error:#}")))?;
        if self.awaiting.as_ref().is_some_and(|input| !lifecycle.pending_inputs().contains(&input)) {
            self.awaiting = None;
        }
        Ok(self.awaiting.is_some()
            || !self.results.is_empty()
            || matches!(lifecycle.activity(), ActivityView::Starting | ActivityView::Working | ActivityView::OwnWork | ActivityView::Continuation))
    }

    /// One step: send a waiting control first, then a waiting dispatch unless a pause holds it,
    /// else read the host when the lifecycle waits for it or the host has something, else wait a
    /// moment for what the store issues for this task.
    pub(crate) fn step(&mut self, stopped: &dyn Fn() -> bool) -> Result<(), Failed> {
        let stepped = self.step_once(stopped);
        if stepped.is_err() {
            self.owner_failed = true;
        }
        stepped
    }

    fn step_once(&mut self, stopped: &dyn Fn() -> bool) -> Result<(), Failed> {
        #[cfg(test)]
        if std::mem::take(&mut self.fail_next_step) {
            return Err(Failed("a step of the owner failed".into()));
        }
        self.sealed();
        if let Some(permit) = self.controls.pop_front() {
            return self.control(permit);
        }
        let paused = self.paused();
        if !paused && let Some(permit) = self.permits.pop_front() {
            return self.dispatch(permit);
        }
        if self.expects_host()? {
            if !self.worker.readable(WAKE).map_err(adapter("waiting for the SDK host"))? {
                return Ok(());
            }
            return self.observe().map(drop);
        }
        // Claude goes on by itself while the task rests, as when a subagent hands its report back
        // after the turn ended: what its host says is read as it comes.
        if !self.replacement && self.worker.readable(Duration::ZERO).unwrap_or(true) {
            return self.observe().map(drop);
        }
        let sealed = self.domain.seals.wait_for(&self.task, WAKE, || stopped() || paused && !self.paused());
        self.permits.extend(sealed.permits);
        self.controls.extend(sealed.control_permits);
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

    /// Claims and records the next waiting control but does not send it: the crash state a daemon
    /// leaves when it dies after recording a control and before the host receives it.
    #[cfg(test)]
    pub(crate) fn claim_and_record_control_without_sending(&mut self) -> bool {
        self.sealed();
        let Some(permit) = self.controls.pop_front() else {
            return false;
        };
        assert!(self.domain.own(self.scope.clone(), |owner| owner.claim_control(permit)).unwrap().0, "the control was claimed");
        let attempt = self.sealed().control_attempts.pop().expect("the claim issued a control attempt");
        let generation = self.worker.owner_generation().unwrap();
        let worker = &mut self.worker;
        self.domain.own(self.scope.clone(), |owner| owner.record_control(attempt, generation, |attempt| worker.prepare_control(attempt))).unwrap();
        self.sealed();
        true
    }

    #[cfg(test)]
    pub(crate) fn pending_controls(&mut self) -> usize {
        self.sealed();
        self.controls.len()
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

    /// The host this driver owns closed while the daemon runs, as a crash of Node or of the host
    /// does: what is left of its group ends, and a host that resumes the same Claude session takes
    /// its place, as a daemon that comes back does (`adopt`). The ended host's journal says what it
    /// took; the task asks the user how to go on, and what it is told next reaches that session.
    pub(crate) fn replace_ended_host(mut self, backend: &SdkBackend, plugin: Option<&std::path::Path>) -> Result<Driver, Failed> {
        if let Err(Failed(why)) = self.stop() {
            log!("what was left of the ended SDK host of {} did not end cleanly: {why}", self.task);
        }
        let (domain, task) = (Arc::clone(&self.domain), self.task.clone());
        drop(self);
        adopt(domain, backend, task, plugin)
    }

    /// Whether the owner should put a host that resumes the session in place of its own: its host
    /// closed or its connection to it failed, which a host that is dying shows before its exit, or
    /// a step of the owner itself failed, and it was not itself a replacement handed nothing yet,
    /// which would only end again. The
    /// replacement stops what is left of the host and waits for its exit before it reads the host's
    /// lock, so whether the host ended is a fact, never a guess.
    fn host_to_replace(&self) -> bool {
        (self.host_closed || self.owner_failed) && !self.replacement
    }
}

impl Drop for Driver {
    fn drop(&mut self) {
        self.domain.contexts.forget(&self.task);
    }
}

/// Commits what the host observed. What the domain refuses never stops the node: the observation
/// commits again without it, and the second value says what of the task's lifecycle it left out.
#[allow(clippy::too_many_arguments)]
fn commit_observation<N: alondra_agent_application::ObservationNormalizer>(domain: &DomainRuntime, scope: &AuthorityScope, binding: &ProviderBinding,
    generation: u64, batch: &OrderedObservation, normalizer: &N, home: &std::path::Path) -> Result<(alondra_store::ObservationAcceptance, Vec<String>), StoreError> {
    let stage = |probe: bool| domain
        .own(scope.clone(), |owner| {
            let acceptance = owner.stage_observation(binding, generation, batch, normalizer, home, probe)?;
            Ok((acceptance, owner.unrecorded().to_vec()))
        })
        .map(|(staged, _)| staged);
    match stage(false) {
        Err(StoreError::Domain(refusal)) => {
            log!("the domain refused part of what an SDK host observed ({}): it commits without it", refusal.message);
            stage(true)
        }
        staged => staged,
    }
}

/// Another input goes to the host first: the store issues this one's permit again once it may.
fn out_of_turn(error: &anyhow::Error) -> bool {
    matches!(error.downcast_ref::<alondra_store::Error>(), Some(alondra_store::Error::OfferOrder))
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
    /// The owner ended on its own, its host stopped: it owns nothing any more.
    ended: Arc<AtomicBool>,
    /// The stop it was told is a close: Claude's running turn ends as an interrupted turn first.
    interrupt: Arc<AtomicBool>,
    domain: Arc<DomainRuntime>,
    host: Arc<Mutex<Option<alondra_agent_runtime::owned_host::HostStop>>>,
    agent: AgentId,
    thread: Option<std::thread::JoinHandle<Result<(), Failed>>>,
}

impl Owner {
    pub(crate) fn spawn(mut driver: Driver) -> Self {
        let stopped = Arc::new(AtomicBool::new(false));
        let interrupt = Arc::new(AtomicBool::new(false));
        let interrupting = Arc::clone(&interrupt);
        let domain = Arc::clone(&driver.domain);
        let host = Arc::new(Mutex::new(driver.worker.stopper()));
        let current = Arc::clone(&host);
        let agent = driver.agent.clone();
        let flag = Arc::clone(&stopped);
        let domain_of_owner = Arc::clone(&domain);
        let ended = Arc::new(AtomicBool::new(false));
        let ending = Arc::clone(&ended);
        let thread = std::thread::Builder::new()
            .name(format!("sdk-owner-{}", driver.task))
            .spawn(move || {
                let told = || flag.load(Ordering::SeqCst);
                let mut lost_because = None;
                loop {
                    while !told() {
                        if let Err(error) = driver.step(&told) {
                            log!("the SDK owner of {} stopped: {error}", driver.task);
                            break;
                        }
                    }
                    if told() {
                        break;
                    }
                    let backend = driver.domain.sdk().map(|(backend, _)| backend.clone());
                    let Some(backend) = backend.filter(|_| driver.host_to_replace()) else {
                        lost_because = Some("its new host stopped before it could take the task's next message".to_owned());
                        break;
                    };
                    let task = driver.task.clone();
                    let plugin = crate::plugins::node_in(driver.domain.home());
                    match driver.replace_ended_host(&backend, plugin.as_deref()) {
                        Ok(next) => {
                            *current.lock().expect("host stop poisoned") = next.worker.stopper();
                            driver = next;
                            log!("the SDK host of {task} ended under this daemon: a host that resumes the same Claude session took its place");
                        }
                        Err(Failed(why)) => {
                            ending.store(true, Ordering::SeqCst);
                            lost(&domain_of_owner, &task, &why);
                            return Ok(());
                        }
                    }
                }
                // Told to stop, or ended by its own failure, the owner stops its host and waits for
                // it: no host is left running with no owner. Only a daemon that exits whole leaves
                // its hosts running, for the daemon that comes back to adopt.
                if interrupting.load(Ordering::SeqCst)
                    && let Some(host) = driver.worker.stopper()
                    && let Err(error) = host.request_after_interrupt()
                {
                    log!("could not ask the SDK host of {} to interrupt its turn and stop: {error}", driver.task);
                }
                let stopped = driver.stop();
                if let Err(error) = &stopped {
                    log!("the SDK host of {} did not stop cleanly: {error}", driver.task);
                }
                if let Some(why) = lost_because {
                    ending.store(true, Ordering::SeqCst);
                    lost(&driver.domain, &driver.task, &why);
                }
                stopped
            })
            .expect("spawning the SDK owner");
        Self { stopped, ended, interrupt, domain, host, agent, thread: Some(thread) }
    }

    pub(crate) fn agent(&self) -> &AgentId {
        &self.agent
    }

    pub(crate) fn ended(&self) -> bool {
        self.ended.load(Ordering::SeqCst)
    }

    /// Tells the owner to stop, asks its host to stop so a read blocked on it returns, and waits
    /// until the owner has seen the host exit and its whole process group end. An error says the
    /// group did not empty, or the owner could not see it end.
    pub(crate) fn stop(&mut self) -> Result<(), Failed> {
        self.end()
    }

    /// Stops the host as `stop` does, once a turn Claude is running has ended as an interrupted
    /// turn: the close of a node whose task is over.
    pub(crate) fn close(&mut self) -> Result<(), Failed> {
        self.interrupt.store(true, Ordering::SeqCst);
        self.end()
    }

    fn end(&mut self) -> Result<(), Failed> {
        self.stopped.store(true, Ordering::SeqCst);
        self.domain.seals.wake();
        let interrupt = self.interrupt.load(Ordering::SeqCst);
        if let Some(host) = self.host.lock().expect("host stop poisoned").as_ref()
            && let Err(error) = if interrupt { host.request_after_interrupt() } else { host.request() }
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
pub(crate) mod tests;
