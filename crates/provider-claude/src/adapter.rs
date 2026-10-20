use crate::normalizer::Normalizer;
use crate::records::{InputRecord, SessionRecord};
use crate::wire::{Connection, ControlIdentity, execution, fence};
use alondra_agent_application::{DurableAdapter, InputMetadata, ObservationNormalizer, invocation};
use alondra_agent_protocol::{
    Action, Body, Command, ControlResult, Decision as WireDecision, DecisionKind as WireKind, FramedReader,
    Observation as WireObservation, PermissionMode as WireMode, SDK_VERSION, SdkEventKind,
};
use alondra_agent_runtime::owned_host::{AdoptedHost, EndedHost, HostStop, LaunchIdentity, OwnedHost, Retained as RetainedHost, VerifiedArtifact};
use crate::ended::EndedJournal;
use alondra_foundation::{CommandId, ExecutionStamp, MessageUuid, RequestId};
use alondra_provider_contract::{
    Adapter, AdapterError, Handoff, Invocation, Operation, OrderedObservation, Placement,
    PreparedCommand, ProviderBinding,
};
use alondra_store::{
    AdapterState, CommittedCommand, CommittedControl, CommittedObservation, Control, ControlAttempt, ControlOperation,
    Decision, DecisionKind, DecisionRequest, DispatchAttempt, OwnerGrant, PermissionMode,
};
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// Configuration carries no dispatch or process-launch authority.
pub struct ClaudeConfiguration {
    pub artifact: VerifiedArtifact,
    pub state_directory: PathBuf,
    pub host_environment: BTreeMap<String, String>,
    pub native_executable: PathBuf,
    pub native_directory: PathBuf,
    pub native_environment: BTreeMap<String, String>,
    pub profile_id: String,
    pub account_id: String,
    pub host_release: String,
    pub io_timeout: Duration,
    /// A Seatbelt profile the whole host runs under, the native process included.
    pub confinement: Option<PathBuf>,
    /// What a task's native Claude loads besides the user's own settings, skills, plugins and MCP.
    pub worker: Option<WorkerProfile>,
}

/// Alondra's plugin directories and launch settings for a task's native Claude, which loads them
/// on top of every setting source of the user's default profile.
#[derive(Clone, Debug, serde::Serialize)]
pub struct WorkerProfile {
    pub plugins: Vec<PathBuf>,
    pub settings: serde_json::Map<String, serde_json::Value>,
    /// The model, effort and denied tools the task was launched with, as `claude --bg` gets them
    /// in its flags; absent, Claude's own defaults apply.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub effort: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub disallowed_tools: Vec<String>,
}

/// Opaque technical worker. The application owns SQL, policy and publication.
/// Configuration alone creates no host, SDK query or native process.
/// ```
/// use alondra_provider_claude::ClaudeWorker;
/// let _ = ClaudeWorker::configured;
/// ```
/// The technical facade has no connection-owning constructor:
/// ```compile_fail,E0599
/// use alondra_provider_claude::ClaudeWorker;
/// let _ = ClaudeWorker::open;
/// ```
/// Submission requires actual SQL commit evidence:
/// ```
/// use alondra_provider_claude::ClaudeWorker;
/// use alondra_store::{CommittedCommand, CommittedControl};
/// fn submit(worker: &mut ClaudeWorker, value: &CommittedCommand) { let _ = worker.submit(value); }
/// fn control(worker: &mut ClaudeWorker, value: &CommittedControl) { let _ = worker.submit_control(value); }
/// ```
pub struct ClaudeWorker {
    adapter: ClaudeAdapter,
    prepared: Option<PreparedDispatch>,
    prepared_control: Option<PreparedControl>,
    pending: Option<OrderedObservation>,
}
struct PreparedControl {
    command: PreparedCommand,
    id: i64,
    revision: u64,
    execution: ExecutionStamp,
    binding: ProviderBinding,
    owner_generation: u64,
    control: alondra_foundation::OperationId,
    operation: ControlOperation,
}
impl PreparedControl {
    fn validate(&self, committed: &CommittedControl) -> Result<(), AdapterError> {
        let attempt = committed.attempt();
        if committed.bytes() != self.command.bytes()
            || committed.owner_generation() != self.owner_generation
            || attempt.id() != self.id
            || attempt.revision() != self.revision
            || attempt.execution() != &self.execution
            || attempt.binding() != &self.binding
            || attempt.control() != &self.control
            || attempt.operation() != &self.operation
        {
            return Err(AdapterError::InvalidObservation);
        }
        Ok(())
    }
}
/// What the host is told to do for a control of the domain.
fn control_action(operation: &ControlOperation) -> Result<Action, AdapterError> {
    let kind = match operation {
        ControlOperation::Control(kind) => kind,
        ControlOperation::Respond(response) => return respond_action(response.epoch(), response.request(), response.decision()),
    };
    Ok(match kind {
        Control::Interrupt { .. } => Action::Interrupt,
        Control::SetModel { model } => Action::SetModel { model: model.clone() },
        Control::SetPermissionMode { mode } => Action::SetPermissionMode {
            mode: match mode {
                PermissionMode::Default => WireMode::Default,
                PermissionMode::Plan => WireMode::Plan,
                PermissionMode::DontAsk => WireMode::DontAsk,
            },
        },
        Control::ReadMcpStatus => Action::McpStatus,
    })
}
/// The answer Claude gets for the request the host persisted at the response's epoch, built from
/// that request as it was asked: the tool runs with the input Claude gave it, Claude's questions
/// get their answers by question text, a form its values. Nothing else widens what Claude may do.
fn respond_action(epoch: u64, request: &DecisionRequest, decision: &Decision) -> Result<Action, AdapterError> {
    let payload: serde_json::Value = serde_json::from_slice(request.payload()).map_err(|_| AdapterError::InvalidObservation)?;
    let input = || payload.get("input").and_then(serde_json::Value::as_object).cloned().ok_or(AdapterError::InvalidObservation);
    let kind = request.kind();
    let decision = match (decision, kind) {
        (Decision::Allow, DecisionKind::Permission | DecisionKind::Plan) => WireDecision::Allow { updated_input: input()? },
        (Decision::Answers { answers }, DecisionKind::Question) => {
            let mut updated_input = input()?;
            let questions = updated_input.get("questions").and_then(serde_json::Value::as_array).ok_or(AdapterError::InvalidObservation)?;
            if questions.len() != answers.len() {
                return Err(AdapterError::InvalidObservation);
            }
            let answered = questions
                .iter()
                .zip(answers)
                .map(|(question, answer)| {
                    let text = question.get("question").and_then(serde_json::Value::as_str).ok_or(AdapterError::InvalidObservation)?;
                    Ok((text.to_owned(), serde_json::Value::String(answer.clone())))
                })
                .collect::<Result<serde_json::Map<_, _>, AdapterError>>()?;
            updated_input.insert("answers".into(), serde_json::Value::Object(answered));
            WireDecision::Allow { updated_input }
        }
        (Decision::Deny { message }, DecisionKind::Permission | DecisionKind::Question | DecisionKind::Plan) => WireDecision::Deny { message: message.clone() },
        (Decision::Accept { text }, DecisionKind::Elicitation) => WireDecision::Accept {
            content: Some(crate::elicitation::elicitation_content(payload.get("requestedSchema").unwrap_or(&serde_json::Value::Null), text)),
        },
        (Decision::Decline, DecisionKind::Elicitation) => WireDecision::Decline {},
        _ => return Err(AdapterError::InvalidObservation),
    };
    Ok(Action::Respond {
        request_id: RequestId(request.request_id().to_owned()),
        epoch,
        kind: match kind {
            DecisionKind::Permission => WireKind::Permission,
            DecisionKind::Question => WireKind::Question,
            DecisionKind::Plan => WireKind::Plan,
            DecisionKind::Elicitation => WireKind::Elicitation,
        },
        tool_use_id: request.tool_use_id().map(str::to_owned),
        decision,
        asked: payload.as_object().cloned().ok_or(AdapterError::InvalidObservation)?,
    })
}
fn decoded(bytes: &[u8]) -> Option<alondra_agent_protocol::Frame> {
    let mut reader = FramedReader::new(std::io::Cursor::new(bytes));
    let frame = reader.read().ok()??;
    reader.read().ok()?.is_none().then_some(frame)
}
/// The command id of a recorded control the host answers with an event of its own: a read of the
/// MCP servers' status. The owner reads the host until that event is staged.
pub fn answered_by_event(command: &[u8]) -> Option<CommandId> {
    match decoded(command)?.body {
        Body::Command(Command { command_id, action: Action::McpStatus, .. }) => Some(command_id),
        _ => None,
    }
}
/// The control a host event answers, when it is the result of one.
pub fn answered_control(observation: &OrderedObservation) -> Option<CommandId> {
    let mut framed = u32::try_from(observation.evidence.len()).ok()?.to_be_bytes().to_vec();
    framed.extend_from_slice(&observation.evidence);
    let Body::Event(event) = decoded(&framed)?.body else {
        return None;
    };
    match event.observation {
        WireObservation::Sdk { kind: SdkEventKind::ControlResult, payload, .. } => {
            let ControlResult::McpStatus { command_id, .. } = serde_json::from_value(payload).ok()?;
            Some(command_id)
        }
        _ => None,
    }
}
/// Whether the host observed the exit of its Claude. The host outlives it with no session left to
/// run, so its owner ends the host as one that ended with its Claude.
pub fn native_exited(observation: &OrderedObservation) -> bool {
    let Ok(length) = u32::try_from(observation.evidence.len()) else {
        return false;
    };
    let mut framed = length.to_be_bytes().to_vec();
    framed.extend_from_slice(&observation.evidence);
    matches!(decoded(&framed).map(|frame| frame.body), Some(Body::Event(event)) if matches!(event.observation, WireObservation::ProcessExited { .. }))
}
struct PreparedDispatch {
    command: PreparedCommand,
    intent: i64,
    revision: u64,
    execution: ExecutionStamp,
    binding: ProviderBinding,
    owner_generation: u64,
    input: alondra_foundation::InputId,
    text: String,
    kind: alondra_store::DispatchKind,
    placement: Option<Placement>,
}
impl PreparedDispatch {
    fn new(attempt: &DispatchAttempt, owner_generation: u64, command: PreparedCommand) -> Self {
        Self {
            command,
            intent: attempt.intent_id(),
            revision: attempt.revision(),
            execution: attempt.execution().clone(),
            binding: attempt.binding().clone(),
            owner_generation,
            input: attempt.input().id().clone(),
            text: attempt.input().text().into(),
            kind: attempt.kind(),
            placement: attempt.placement().cloned(),
        }
    }
    fn validate(&self, committed: &CommittedCommand) -> Result<(), AdapterError> {
        let attempt = committed.attempt();
        if committed.bytes() != self.command.bytes()
            || committed.owner_generation() != self.owner_generation
            || attempt.intent_id() != self.intent
            || attempt.revision() != self.revision
            || attempt.execution() != &self.execution
            || attempt.binding() != &self.binding
            || attempt.input().id() != &self.input
            || attempt.input().text() != self.text
            || attempt.kind() != self.kind
            || attempt.placement() != self.placement.as_ref()
        {
            return Err(AdapterError::InvalidObservation);
        }
        Ok(())
    }
}
impl ClaudeWorker {
    pub fn configured(
        binding: ProviderBinding,
        configuration: ClaudeConfiguration,
    ) -> Result<Self, AdapterError> {
        Ok(Self {
            adapter: ClaudeAdapter::new(binding, configuration)?,
            prepared: None,
            prepared_control: None,
            pending: None,
        })
    }
    pub fn binding(&self) -> &ProviderBinding {
        self.adapter.binding()
    }
    pub fn owner_generation(&self) -> Result<u64, AdapterError> {
        self.adapter.owner_generation()
    }
    pub fn install_owner(
        &mut self,
        grant: OwnerGrant,
        state: AdapterState,
    ) -> Result<(), AdapterError> {
        self.adapter.install_owner(grant, state)
    }
    /// Adopts the host a previous daemon left running, with this new owner generation, rather than
    /// launching a fresh one: the same Claude keeps its session.
    pub fn adopt_owner(
        &mut self,
        grant: OwnerGrant,
        state: AdapterState,
    ) -> Result<(), AdapterError> {
        self.adapter.adopt_owner(grant, state)
    }
    /// Whether a host of this execution still runs (adopt it) or ended (what its journal proves).
    pub fn find_host(&mut self, grant: &OwnerGrant, state: &AdapterState) -> Result<Found, AdapterError> {
        self.adapter.find_host(grant, state)
    }
    /// Launches a fresh host for the same execution once the evidence of the ended one is
    /// committed; its first input resumes the stored native session when `resume`.
    pub fn relaunch(&mut self, grant: OwnerGrant, state: AdapterState, base: u64, resume: bool, environment: BTreeMap<String, String>) -> Result<(), AdapterError> {
        self.adapter.configuration.native_environment.extend(environment);
        self.adapter.relaunch(grant, state, base, resume)
    }
    /// Re-sends the exact recorded bytes of a dispatch whose receipt a crash lost, and returns the
    /// host's definitive answer.
    pub fn reconcile(&mut self, command: &[u8]) -> Result<Handoff, AdapterError> {
        self.adapter.reconcile(command)
    }
    /// The conversation this worker stages next continues the existing native session `session`
    /// (a `claude --bg` node's): its first host resumes it, in the same working directory.
    pub fn continue_session(&mut self, session: &str) -> Result<(), AdapterError> {
        self.adapter.continuing = Some(MessageUuid::new(session).map_err(|_| AdapterError::InvalidObservation)?);
        Ok(())
    }
    pub fn stage_input(&mut self, metadata: InputMetadata<'_, '_>) -> Result<(), AdapterError> {
        if metadata.binding().map_err(storage)? != *self.binding() {
            return Err(AdapterError::InvalidObservation);
        }
        self.adapter.stage_input(metadata)
    }
    pub fn prepare(
        &mut self,
        attempt: &DispatchAttempt,
        metadata: InputMetadata<'_, '_>,
    ) -> Result<PreparedCommand, AdapterError> {
        if self.prepared.is_some()
            || metadata.scope() != attempt.scope()
            || metadata.binding().map_err(storage)? != *attempt.binding()
            || metadata.execution() != attempt.execution()
            || metadata.input_id() != attempt.input().id()
        {
            return Err(AdapterError::InvalidObservation);
        }
        let owner = self.adapter.host()?.owner();
        if attempt.scope() != owner.scope()
            || attempt.binding() != owner.binding()
            || attempt.execution() != owner.execution()
        {
            return Err(AdapterError::InvalidObservation);
        }
        let owner_generation = owner.generation();
        let native = self.adapter.load_input(metadata)?;
        let command = self.adapter.prepare(invocation(attempt), native)?;
        let output = PreparedCommand::new(command.bytes().to_vec())?;
        self.prepared = Some(PreparedDispatch::new(attempt, owner_generation, command));
        Ok(output)
    }
    /// Requires the exact preparation persisted under this sealed attempt and owner.
    /// The application must recheck its current SQL fence before calling; this is not a lease.
    pub fn submit(&mut self, committed: &CommittedCommand) -> Result<Handoff, AdapterError> {
        let prepared = self
            .prepared
            .as_ref()
            .ok_or(AdapterError::InvalidObservation)?;
        prepared.validate(committed)?;
        let owner = self.adapter.host()?.owner();
        if committed.owner_generation() != owner.generation()
            || committed.attempt().scope() != owner.scope()
        {
            return Err(AdapterError::InvalidObservation);
        }
        // Consume the local preparation before any possible physical write; failure cannot re-offer it.
        let prepared = self
            .prepared
            .take()
            .ok_or(AdapterError::InvalidObservation)?;
        self.adapter
            .submit(invocation(committed.attempt()), &prepared.command)
    }
    /// Asks this worker's host to stop from another thread; `None` before it has a host.
    pub fn stopper(&self) -> Option<HostStop> {
        self.adapter.host.as_ref().map(Host::stopper)
    }
    /// The process group a launched host leads, named by the host's PID.
    pub fn host_group(&self) -> Option<i32> {
        match self.adapter.host.as_ref()? {
            Host::Launched(host) => Some(host.process_group()),
            Host::Adopted(_) => None,
        }
    }
    /// Whether the host this worker launched has exited; an adopted host is not this daemon's child.
    pub fn host_exited(&mut self) -> bool {
        match self.adapter.host.as_mut() {
            Some(Host::Launched(host)) => host.observed_exit().ok().flatten().is_some(),
            _ => false,
        }
    }
    /// The exact command of a claimed control, under the current owner. One at a time, like
    /// dispatches: it is sent only once its committed evidence matches it.
    pub fn prepare_control(&mut self, attempt: &ControlAttempt) -> Result<PreparedCommand, AdapterError> {
        if self.prepared_control.is_some() {
            return Err(AdapterError::InvalidObservation);
        }
        let owner = self.adapter.host()?.owner();
        if attempt.scope() != owner.scope()
            || attempt.binding() != owner.binding()
            || attempt.binding() != self.binding()
            || attempt.execution() != owner.execution()
        {
            return Err(AdapterError::InvalidObservation);
        }
        let owner_generation = owner.generation();
        let command = self.adapter.command(attempt.command_id(), control_action(attempt.operation())?)?;
        let output = PreparedCommand::new(command.bytes().to_vec())?;
        self.prepared_control = Some(PreparedControl {
            command,
            id: attempt.id(),
            revision: attempt.revision(),
            execution: attempt.execution().clone(),
            binding: attempt.binding().clone(),
            owner_generation,
            control: attempt.control().clone(),
            operation: attempt.operation().clone(),
        });
        Ok(output)
    }
    /// Sends the exact control its committed evidence names. The application must recheck its
    /// current SQL fence before calling; this is not a lease.
    pub fn submit_control(&mut self, committed: &CommittedControl) -> Result<Handoff, AdapterError> {
        self.prepared_control
            .as_ref()
            .ok_or(AdapterError::InvalidObservation)?
            .validate(committed)?;
        let owner = self.adapter.host()?.owner();
        if committed.owner_generation() != owner.generation()
            || committed.attempt().scope() != owner.scope()
        {
            return Err(AdapterError::InvalidObservation);
        }
        let prepared = self
            .prepared_control
            .take()
            .ok_or(AdapterError::InvalidObservation)?;
        self.adapter.send(committed.attempt().execution(), &prepared.command)
    }
    /// Asks the host to stop and waits until it has exited: a launched host with its whole group,
    /// an adopted one by its own exit. A worker without a host has none.
    pub fn stop(&mut self) -> Result<(), AdapterError> {
        self.adapter.connection = None;
        let Some(host) = self.adapter.host.as_mut() else {
            return Ok(());
        };
        host.stop().map_err(|_| AdapterError::Disconnected)
    }
    pub fn poll(&mut self) -> Result<Option<OrderedObservation>, AdapterError> {
        if let Some(pending) = &self.pending {
            return Ok(Some(pending.clone()));
        }
        let received = self.adapter.next()?;
        self.pending = received.clone();
        Ok(received)
    }
    /// Whether a host event waits to be read, within `timeout`, without consuming it.
    pub fn readable(&self, timeout: Duration) -> Result<bool, AdapterError> {
        if self.pending.is_some() {
            return Ok(true);
        }
        self.adapter.connection.as_ref().ok_or(AdapterError::Disconnected)?.readable(timeout)
    }
    pub fn normalizer(&self) -> &impl ObservationNormalizer {
        self.adapter.normalizer()
    }
    /// SQL evidence only; only the owning daemon decides when its real operation is published.
    pub fn acknowledge(&mut self, committed: &CommittedObservation) -> Result<(), AdapterError> {
        let pending = self
            .pending
            .as_ref()
            .ok_or(AdapterError::InvalidObservation)?;
        validate_ack(self.adapter.host()?.owner(), pending, committed)?;
        self.adapter
            .acknowledge(committed.execution(), committed.cursor())?;
        self.pending = None;
        Ok(())
    }
}
fn validate_ack(
    owner: &OwnerGrant,
    pending: &OrderedObservation,
    committed: &CommittedObservation,
) -> Result<(), AdapterError> {
    if committed.scope() != owner.scope()
        || committed.execution() != owner.execution()
        || committed.binding() != owner.binding()
        || committed.owner_generation() != owner.generation()
        || pending.execution != *committed.execution()
        || pending.owner_generation != committed.owner_generation()
        || pending.cursor != committed.cursor()
        || !committed.matches_evidence(&pending.evidence)
    {
        return Err(AdapterError::InvalidObservation);
    }
    Ok(())
}

/// The transport's host: one this daemon launched, or one a previous daemon left running that
/// this one adopted. An adopted host is stopped by the process at its endpoint, never reaped here.
enum Host {
    Launched(OwnedHost),
    Adopted(AdoptedHost),
}
impl Host {
    fn owner(&self) -> &OwnerGrant {
        match self {
            Host::Launched(host) => host.owner(),
            Host::Adopted(host) => host.owner(),
        }
    }
    fn artifact(&self) -> &VerifiedArtifact {
        match self {
            Host::Launched(host) => host.identity().artifact(),
            Host::Adopted(host) => host.identity().artifact(),
        }
    }
    fn connect(&mut self, deadline: Instant) -> std::io::Result<std::os::unix::net::UnixStream> {
        match self {
            Host::Launched(host) => host.wait_for_socket(deadline),
            Host::Adopted(host) => host.connect(deadline),
        }
    }
    fn request_stop(&mut self) -> std::io::Result<()> {
        match self {
            Host::Launched(host) => host.request_stop(),
            Host::Adopted(_) => Ok(()),
        }
    }
    fn wait(&mut self) -> std::io::Result<std::process::ExitStatus> {
        match self {
            Host::Launched(host) => host.wait(),
            Host::Adopted(_) => Err(std::io::Error::other("an adopted host is not ours to reap")),
        }
    }
    fn stopper(&self) -> HostStop {
        match self {
            Host::Launched(host) => host.stopper(),
            Host::Adopted(host) => host.stopper(),
        }
    }
    fn stop(&mut self) -> std::io::Result<()> {
        match self {
            Host::Launched(host) => {
                host.request_stop()?;
                host.wait().map(|_| ())
            }
            Host::Adopted(host) => host.stop(),
        }
    }
}

struct ClaudeAdapter {
    binding: ProviderBinding,
    configuration: ClaudeConfiguration,
    control: ControlIdentity,
    host: Option<Host>,
    connection: Option<Connection>,
    normalizer: Normalizer,
    ended: Option<(EndedHost, String)>,
    /// The existing native session the conversation this worker stages continues.
    continuing: Option<MessageUuid>,
}
impl ClaudeConfiguration {
    /// Checks the configuration and makes the private control material of one host.
    pub(super) fn control(&self) -> Result<ControlIdentity, AdapterError> {
        if self.io_timeout.is_zero()
            || self.io_timeout > Duration::from_secs(300)
            || !self.native_executable.is_absolute()
            || !self.native_directory.is_absolute()
            || self.profile_id.trim().is_empty()
            || self.account_id.trim().is_empty()
            || self.host_release.trim().is_empty()
        {
            return Err(AdapterError::InvalidObservation);
        }
        let mut secret = [0; 32];
        getrandom::fill(&mut secret).map_err(|_| AdapterError::Disconnected)?;
        Ok(ControlIdentity {
            profile_id: self.profile_id.clone(),
            account_id: self.account_id.clone(),
            host_instance_id: random_uuid()?.as_str().into(),
            secret,
            host_release: self.host_release.clone(),
        })
    }
}

impl ClaudeAdapter {
    fn new(
        binding: ProviderBinding,
        configuration: ClaudeConfiguration,
    ) -> Result<Self, AdapterError> {
        let control = configuration.control()?;
        Ok(Self {
            binding,
            configuration,
            control,
            host: None,
            connection: None,
            normalizer: Normalizer {
                session: None,
                fence: None,
                first_cursor: None,
                host_ended: false,
            },
            ended: None,
            continuing: None,
        })
    }
    fn deadline(&self) -> Result<Instant, AdapterError> {
        Instant::now()
            .checked_add(self.configuration.io_timeout)
            .ok_or(AdapterError::Disconnected)
    }
    fn host(&self) -> Result<&Host, AdapterError> {
        self.host.as_ref().ok_or(AdapterError::Disconnected)
    }
    /// The exact command the host is told under this owner's fence, with its deadline.
    fn command(&self, command_id: CommandId, action: Action) -> Result<PreparedCommand, AdapterError> {
        let owner = self.host()?.owner();
        let deadline_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|_| AdapterError::Disconnected)?
            .checked_add(self.configuration.io_timeout)
            .and_then(|duration| u64::try_from(duration.as_millis()).ok())
            .ok_or(AdapterError::Disconnected)?;
        self.connection
            .as_ref()
            .ok_or(AdapterError::Disconnected)?
            .prepare(
                owner,
                Command {
                    fence: fence(owner, &self.control),
                    execution: execution(owner),
                    command_id,
                    deadline_ms,
                    action,
                },
            )
    }
    fn send(&mut self, stamp: &ExecutionStamp, command: &PreparedCommand) -> Result<Handoff, AdapterError> {
        let deadline = self.deadline()?;
        let host = self.host.as_ref().ok_or(AdapterError::Disconnected)?;
        if stamp != host.owner().execution() {
            return Err(AdapterError::InvalidObservation);
        }
        self.connection
            .as_mut()
            .ok_or(AdapterError::Disconnected)?
            .submit(host.owner(), command, deadline)
    }
    /// Re-sends the exact recorded command bytes of an unresolved dispatch and reads the host's
    /// definitive answer. The owner is rechecked; the lane is the adopted one.
    fn reconcile(&mut self, command: &[u8]) -> Result<Handoff, AdapterError> {
        let deadline = self.deadline()?;
        let host = self.host.as_ref().ok_or(AdapterError::Disconnected)?;
        let prepared = PreparedCommand::new(command.to_vec())?;
        self.connection.as_mut().ok_or(AdapterError::Disconnected)?.reconcile(host.owner(), &prepared, deadline)
    }
    /// Opens the owner lane to the current host, from the cursor already committed in SQL.
    fn connect(&mut self, committed_cursor: u64) -> Result<(), AdapterError> {
        let deadline = self.deadline()?;
        let host = self.host.as_mut().ok_or(AdapterError::Disconnected)?;
        let stream = host.connect(deadline).map_err(|_| AdapterError::Disconnected)?;
        self.connection = Some(Connection::attach(stream, host.owner(), host.artifact(), &self.control, committed_cursor, deadline)?);
        Ok(())
    }
    /// The retained bootstrap of a host of this execution, checked against this grant and
    /// configuration and against the stored conversation. A live host must run this daemon's
    /// artifact; an ended one may have run the release before an upgrade.
    fn retained(&self, grant: &OwnerGrant, artifact: Option<&VerifiedArtifact>, bytes: &[u8], stored: &SessionRecord) -> std::io::Result<Retained> {
        let retained: RetainedBootstrap = serde_json::from_slice(bytes).map_err(std::io::Error::other)?;
        let refuse = |why: &str| std::io::Error::other(format!("retained host bootstrap {why}"));
        if artifact.is_some_and(|artifact| {
            retained.manifest.host_sha256 != artifact.artifact_sha256() || retained.manifest.executable_sha256 != artifact.executable_sha256()
        }) {
            return Err(refuse("artifact hashes differ"));
        }
        let control = |secret| ControlIdentity {
            profile_id: self.configuration.profile_id.clone(),
            account_id: self.configuration.account_id.clone(),
            host_instance_id: retained.host_instance_id.clone(),
            secret,
            host_release: retained.manifest.host_release.clone(),
        };
        if retained.execution != execution(grant) || retained.fence != fence(grant, &control([0; 32])) {
            return Err(refuse("fence or execution differs"));
        }
        if std::path::Path::new(&retained.native.cwd) != self.configuration.native_directory {
            return Err(refuse("native working directory differs"));
        }
        let session = SessionRecord { session_id: MessageUuid::new(&retained.native.session_id).map_err(|_| refuse("native session id"))?, continued: stored.continued };
        if session.session_id != stored.session_id {
            return Err(refuse("native session differs from the stored conversation"));
        }
        let secret = decode_secret(&retained.control_secret).ok_or_else(|| refuse("control secret"))?;
        Ok(Retained { control: control(secret), session, journal_base: retained.journal_base })
    }
    /// Adopts the host a previous daemon left running: it reads the retained bootstrap, verifies it
    /// against this grant and configuration, rebuilds the control identity and session from it, and
    /// attaches with the new owner generation from the committed checkpoint. It launches nothing
    /// and spawns no Claude; a missing or changed endpoint is a dead host (`Disconnected`).
    fn adopt_owner(&mut self, owner: OwnerGrant, state: AdapterState) -> Result<(), AdapterError> {
        if owner.binding() != &self.binding || state.validate_owner(&owner).is_err() || self.host.is_some() {
            return Err(AdapterError::InvalidObservation);
        }
        let stored: SessionRecord = serde_json::from_slice(state.conversation()).map_err(|_| AdapterError::InvalidObservation)?;
        let expected = fence(&owner, &self.control);
        let (host, retained) = AdoptedHost::adopt(
            owner,
            self.configuration.artifact.clone(),
            &self.configuration.state_directory,
            |grant, identity, bytes| self.retained(grant, Some(identity.artifact()), bytes, &stored),
        )
        .map_err(|_| AdapterError::Disconnected)?;
        self.control = retained.control;
        self.host = Some(Host::Adopted(host));
        self.normalizer.session = Some(retained.session);
        self.normalizer.fence = Some(expected);
        self.normalizer.first_cursor = retained.journal_base.map(|base| base + 1);
        self.connect(state.checkpoint().map(|checkpoint| checkpoint.cursor()).unwrap_or(0))
    }
    /// Whether a host of this execution still runs. When none does, the evidence its journal kept
    /// beyond `state`'s checkpoint; the ended host's folder stays locked until `relaunch`.
    fn find_host(&mut self, owner: &OwnerGrant, state: &AdapterState) -> Result<Found, AdapterError> {
        if owner.binding() != &self.binding || state.validate_owner(owner).is_err() || self.host.is_some() || self.ended.is_some() {
            return Err(AdapterError::InvalidObservation);
        }
        let stored: SessionRecord = serde_json::from_slice(state.conversation()).map_err(|_| AdapterError::InvalidObservation)?;
        let committed = state.checkpoint().map(|checkpoint| checkpoint.cursor()).unwrap_or(0);
        let wire_fence = fence(owner, &self.control);
        let journal = match EndedHost::claim(&self.configuration.state_directory).map_err(|_| AdapterError::Disconnected)? {
            RetainedHost::Held => return Ok(Found::Live),
            RetainedHost::Absent => EndedJournal::without_evidence(committed),
            RetainedHost::Ended(ended) => {
                let retained = self.retained(owner, None, ended.bootstrap(), &stored).map_err(|_| AdapterError::InvalidObservation)?;
                let journal = EndedJournal::read(ended.state_dir(), &wire_fence, &execution(owner), &retained.control.host_instance_id, committed)?;
                self.ended = Some((ended, retained.control.host_instance_id));
                journal
            }
        };
        self.normalizer.session = Some(stored);
        self.normalizer.fence = Some(wire_fence);
        self.normalizer.host_ended = true;
        Ok(Found::Ended(journal))
    }
    /// Launches a fresh host for the same execution after its host ended: the ended host's folder
    /// is kept as evidence, the new host's journal continues from `base`, and its first input
    /// resumes the stored native session when `resume`, or starts it with the same id.
    fn relaunch(&mut self, owner: OwnerGrant, state: AdapterState, base: u64, resume: bool) -> Result<(), AdapterError> {
        if owner.binding() != &self.binding || state.validate_owner(&owner).is_err() || self.host.is_some() || !self.normalizer.host_ended {
            return Err(AdapterError::InvalidObservation);
        }
        if state.checkpoint().map(|checkpoint| checkpoint.cursor()).unwrap_or(0) != base {
            return Err(AdapterError::InvalidObservation);
        }
        let session: SessionRecord = serde_json::from_slice(state.conversation()).map_err(|_| AdapterError::InvalidObservation)?;
        let directory = self.configuration.state_directory.clone();
        if let Some((ended, instance)) = self.ended.take() {
            let mut archive = directory.clone().into_os_string();
            archive.push(format!(".ended-{instance}"));
            ended.retire(std::path::Path::new(&archive)).map_err(|_| AdapterError::Disconnected)?;
        }
        if !directory.try_exists().map_err(|_| AdapterError::Disconnected)? {
            use std::os::unix::fs::DirBuilderExt;
            std::fs::DirBuilder::new().mode(0o700).create(&directory).map_err(|_| AdapterError::Disconnected)?;
        }
        let expected = fence(&owner, &self.control);
        let continued = Continued { journal_base: base, resume };
        let host = OwnedHost::launch_confined(
            owner,
            self.configuration.artifact.clone(),
            &directory,
            &self.configuration.host_environment,
            self.configuration.confinement.as_deref(),
            |grant, identity| bootstrap(grant, identity, &self.control, &self.configuration, &session, None, Some(continued)),
        )
        .map_err(|_| AdapterError::Disconnected)?;
        self.host = Some(Host::Launched(host));
        self.normalizer.session = Some(session);
        self.normalizer.fence = Some(expected);
        self.normalizer.first_cursor = Some(base + 1);
        self.normalizer.host_ended = false;
        self.connect(base)
    }
}

/// What a daemon that takes an execution over finds of its host.
pub enum Found {
    /// A host still runs: adopt it.
    Live,
    /// No host runs any more: what its journal proves, for a relaunch.
    Ended(EndedJournal),
}
struct Retained {
    control: ControlIdentity,
    session: SessionRecord,
    journal_base: Option<u64>,
}
/// A host launched for an execution whose previous host ended.
#[derive(Clone, Copy)]
pub(super) struct Continued {
    journal_base: u64,
    resume: bool,
}

#[derive(serde::Deserialize)]
struct RetainedBootstrap {
    host_instance_id: String,
    control_secret: String,
    fence: alondra_agent_protocol::Fence,
    execution: alondra_agent_protocol::Execution,
    manifest: RetainedManifest,
    native: RetainedNative,
    #[serde(default)]
    journal_base: Option<u64>,
}
#[derive(serde::Deserialize)]
struct RetainedManifest {
    host_release: String,
    host_sha256: String,
    executable_sha256: String,
}
#[derive(serde::Deserialize)]
struct RetainedNative {
    session_id: String,
    cwd: String,
}
fn decode_secret(hex: &str) -> Option<[u8; 32]> {
    if hex.len() != 64 {
        return None;
    }
    let mut secret = [0; 32];
    for (index, byte) in secret.iter_mut().enumerate() {
        *byte = u8::from_str_radix(hex.get(index * 2..index * 2 + 2)?, 16).ok()?;
    }
    Some(secret)
}
impl Adapter for ClaudeAdapter {
    type NativeInput = InputRecord;
    fn binding(&self) -> &ProviderBinding {
        &self.binding
    }
    fn prepare(
        &mut self,
        invocation: Invocation<'_>,
        native: InputRecord,
    ) -> Result<PreparedCommand, AdapterError> {
        let owner = self.host()?.owner();
        if invocation.binding != &self.binding || invocation.execution != owner.execution() {
            return Err(AdapterError::InvalidObservation);
        }
        let action = match invocation.operation {
            Operation::Launch => {
                let Some(Placement::LocalDirectory(directory)) = invocation.placement else {
                    return Err(AdapterError::Unsupported);
                };
                if std::path::Path::new(directory) != self.configuration.native_directory {
                    return Err(AdapterError::Unsupported);
                }
                Action::Start {
                    cwd: directory.clone(),
                    input_id: native.message_id,
                    prompt: invocation.text.into(),
                }
            }
            Operation::Input => Action::Offer {
                input_id: native.message_id,
                prompt: invocation.text.into(),
            },
        };
        self.command(invocation.command, action)
    }
    fn submit(
        &mut self,
        invocation: Invocation<'_>,
        command: &PreparedCommand,
    ) -> Result<Handoff, AdapterError> {
        if invocation.binding != &self.binding {
            return Err(AdapterError::InvalidObservation);
        }
        self.send(invocation.execution, command)
    }
    fn next(&mut self) -> Result<Option<OrderedObservation>, AdapterError> {
        let deadline = self.deadline()?;
        let host = self.host.as_ref().ok_or(AdapterError::Disconnected)?;
        let received = self
            .connection
            .as_mut()
            .ok_or(AdapterError::Disconnected)?
            .next(host.owner(), deadline)?;
        let Body::Event(event) = &received.frame().body else {
            return Err(AdapterError::InvalidObservation);
        };
        Ok(Some(OrderedObservation {
            owner_generation: host.owner().generation(),
            execution: host.owner().execution().clone(),
            cursor: event.cursor,
            evidence: received.payload().to_vec(),
        }))
    }
    fn acknowledge(&mut self, stamp: &ExecutionStamp, cursor: u64) -> Result<(), AdapterError> {
        let deadline = self.deadline()?;
        let host = self.host.as_ref().ok_or(AdapterError::Disconnected)?;
        if stamp != host.owner().execution() {
            return Err(AdapterError::InvalidObservation);
        }
        self.connection
            .as_mut()
            .ok_or(AdapterError::Disconnected)?
            .acknowledge(host.owner(), cursor, deadline)
    }
}
impl DurableAdapter for ClaudeAdapter {
    type Normalizer = Normalizer;
    fn normalizer(&self) -> &Normalizer {
        &self.normalizer
    }
    fn owner_generation(&self) -> Result<u64, AdapterError> {
        Ok(self.host()?.owner().generation())
    }
    fn stage_input(&mut self, mut metadata: InputMetadata<'_, '_>) -> Result<(), AdapterError> {
        match metadata.conversation() {
            Ok(Some(bytes)) => {
                let _: SessionRecord =
                    serde_json::from_slice(&bytes).map_err(|_| AdapterError::InvalidObservation)?;
            }
            Ok(None) => metadata
                .put_conversation(
                    serde_json::to_vec(&match &self.continuing {
                        Some(session) => SessionRecord { session_id: session.clone(), continued: true },
                        None => SessionRecord { session_id: random_uuid()?, continued: false },
                    })
                    .map_err(|_| AdapterError::InvalidObservation)?,
                )
                .map_err(storage)?,
            Err(error) => return Err(storage(error)),
        }
        match metadata.input() {
            Ok(Some(bytes)) => {
                let _: InputRecord =
                    serde_json::from_slice(&bytes).map_err(|_| AdapterError::InvalidObservation)?;
            }
            Ok(None) => metadata
                .put_input(
                    serde_json::to_vec(&InputRecord {
                        message_id: random_uuid()?,
                    })
                    .map_err(|_| AdapterError::InvalidObservation)?,
                )
                .map_err(storage)?,
            Err(error) => return Err(storage(error)),
        }
        Ok(())
    }
    fn load_input(&mut self, metadata: InputMetadata<'_, '_>) -> Result<InputRecord, AdapterError> {
        let bytes = metadata.input().map_err(storage)?.ok_or(AdapterError::InvalidObservation)?;
        serde_json::from_slice(&bytes).map_err(|_| AdapterError::InvalidObservation)
    }
    fn install_owner(
        &mut self,
        owner: OwnerGrant,
        state: AdapterState,
    ) -> Result<(), AdapterError> {
        if owner.binding() != &self.binding || state.validate_owner(&owner).is_err() {
            return Err(AdapterError::InvalidObservation);
        }
        // Historical command receipts need durable exact-byte reconstruction before reattach.
        // Never replace an uncertain live host with a fresh process or a new secret.
        if self.host.is_some()
            || state.checkpoint().is_some()
            || self
                .configuration
                .state_directory
                .join("host.lock")
                .try_exists()
                .map_err(|_| AdapterError::Disconnected)?
        {
            return Err(AdapterError::Unsupported);
        }
        let session: SessionRecord = serde_json::from_slice(state.conversation())
            .map_err(|_| AdapterError::InvalidObservation)?;
        let expected = fence(&owner, &self.control);
        let host = OwnedHost::launch_confined(
            owner,
            self.configuration.artifact.clone(),
            &self.configuration.state_directory,
            &self.configuration.host_environment,
            self.configuration.confinement.as_deref(),
            |grant, identity| {
                bootstrap(
                    grant,
                    identity,
                    &self.control,
                    &self.configuration,
                    &session,
                    None,
                    None,
                )
            },
        )
        .map_err(|_| AdapterError::Disconnected)?;
        self.host = Some(Host::Launched(host));
        self.normalizer.session = Some(session);
        self.normalizer.fence = Some(expected);
        self.connect(0)
    }
}
pub(super) fn bootstrap(
    owner: &impl crate::wire::HostOwner,
    identity: &LaunchIdentity,
    control: &ControlIdentity,
    configuration: &ClaudeConfiguration,
    session: &SessionRecord,
    orchestrator: Option<&crate::batch::OrchestratorProfile>,
    continued: Option<Continued>,
) -> std::io::Result<Vec<u8>> {
    let secret = control
        .secret
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    if configuration
        .native_environment
        .values()
        .any(|value| value.contains(&secret))
    {
        return Err(std::io::Error::other(
            "private control material in native environment",
        ));
    }
    let frame =
        alondra_agent_protocol::Frame::new(Body::Challenge(alondra_agent_protocol::Challenge {
            fence: fence(owner, control),
            execution: execution(owner),
            host_instance_id: control.host_instance_id.clone(),
            nonce: "0".repeat(64),
            executable_sha256: identity.artifact().executable_sha256().into(),
        }));
    alondra_agent_protocol::encode(&frame).map_err(std::io::Error::other)?;
    let mut bootstrap = serde_json::json!({
        "state_dir": identity.state_dir(), "ownership_path": identity.ownership_path(), "socket_path": identity.socket_path(), "host_instance_id": control.host_instance_id, "control_secret": secret,
        "fence": fence(owner, control), "execution": execution(owner),
        "manifest": { "mode": "node_bundle", "artifact_path": identity.artifact().artifact(), "host_release": control.host_release, "host_sha256": identity.artifact().artifact_sha256(), "executable_sha256": identity.artifact().executable_sha256(), "sdk_version": SDK_VERSION },
        "native": { "session_id": session.session_id, "executable": configuration.native_executable, "cwd": configuration.native_directory, "env": configuration.native_environment }
    });
    if let Some(continued) = continued {
        bootstrap["journal_base"] = continued.journal_base.into();
        bootstrap["native"]["resume"] = continued.resume.into();
    }
    if session.continued {
        bootstrap["native"]["resume"] = true.into();
    }
    match (orchestrator, &configuration.worker) {
        (Some(_), Some(_)) => return Err(std::io::Error::other("a host runs the orchestrator or a task, never both")),
        (Some(profile), None) => bootstrap["orchestrator"] = serde_json::to_value(profile).map_err(std::io::Error::other)?,
        (None, Some(profile)) => bootstrap["worker"] = serde_json::to_value(profile).map_err(std::io::Error::other)?,
        (None, None) => {}
    }
    serde_json::to_vec(&bootstrap).map_err(std::io::Error::other)
}
pub(super) fn random_uuid() -> Result<MessageUuid, AdapterError> {
    let mut bytes = [0; 16];
    getrandom::fill(&mut bytes).map_err(|_| AdapterError::Disconnected)?;
    bytes[6] = (bytes[6] & 15) | 64;
    bytes[8] = (bytes[8] & 63) | 128;
    let hex = bytes
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    MessageUuid::new(format!(
        "{}-{}-{}-{}-{}",
        &hex[..8],
        &hex[8..12],
        &hex[12..16],
        &hex[16..20],
        &hex[20..]
    ))
    .map_err(|_| AdapterError::InvalidObservation)
}
fn storage(error: alondra_store::Error) -> AdapterError {
    AdapterError::Storage(error.to_string())
}

#[cfg(all(test, feature = "sdk_fixture"))]
#[path = "sdk_tests.rs"]
mod sdk_tests;

#[cfg(test)]
#[path = "facade_tests.rs"]
mod facade_tests;

#[cfg(test)]
mod respond_tests {
    use super::*;
    use serde_json::json;

    fn request(kind: DecisionKind, payload: serde_json::Value) -> DecisionRequest {
        let tool_use = (kind != DecisionKind::Elicitation).then(|| "toolu_1".to_owned());
        DecisionRequest::new("decision-1".into(), tool_use, kind, serde_json::to_vec(&payload).unwrap()).unwrap()
    }
    fn decided(action: Action) -> (u64, WireKind, Option<String>, WireDecision) {
        let Action::Respond { request_id, epoch, kind, tool_use_id, decision, .. } = action else { panic!("{action:?}") };
        assert_eq!(request_id.0, "decision-1");
        (epoch, kind, tool_use_id, decision)
    }

    #[test]
    fn an_allowed_tool_runs_with_the_input_claude_asked_for_and_a_denial_carries_its_message() {
        let asked = request(DecisionKind::Permission, json!({"tool": "Bash", "input": {"command": "make test", "timeout": 5}, "request": {}}));
        let (epoch, kind, tool_use, decision) = decided(respond_action(9, &asked, &Decision::Allow).unwrap());
        assert_eq!((epoch, kind, tool_use.as_deref()), (9, WireKind::Permission, Some("toolu_1")));
        assert_eq!(decision, WireDecision::Allow { updated_input: json!({"command": "make test", "timeout": 5}).as_object().unwrap().clone() });
        let denied = decided(respond_action(9, &asked, &Decision::Deny { message: "The user denied it".into() }).unwrap()).3;
        assert_eq!(denied, WireDecision::Deny { message: "The user denied it".into() });
        assert!(matches!(respond_action(9, &asked, &Decision::Decline), Err(AdapterError::InvalidObservation)), "a form answer does not answer a tool");
    }

    #[test]
    fn claudes_questions_get_their_answers_by_question_text() {
        let questions = json!([{"question": "Which color?", "options": [{"label": "Red"}]}, {"question": "Which sizes?", "multiSelect": true, "options": []}]);
        let asked = request(DecisionKind::Question, json!({"tool": "AskUserQuestion", "input": {"questions": questions}, "request": {}}));
        let answers = Decision::Answers { answers: vec!["Red".into(), "Small, Large".into()] };
        let decision = decided(respond_action(4, &asked, &answers).unwrap()).3;
        let expected = json!({"questions": questions, "answers": {"Which color?": "Red", "Which sizes?": "Small, Large"}});
        assert_eq!(decision, WireDecision::Allow { updated_input: expected.as_object().unwrap().clone() });
        let short = Decision::Answers { answers: vec!["Red".into()] };
        assert!(matches!(respond_action(4, &asked, &short), Err(AdapterError::InvalidObservation)), "every question needs its answer");
    }

    #[test]
    fn an_accepted_form_carries_the_values_its_schema_types_and_a_declined_one_nothing() {
        let schema = json!({"type": "object", "properties": {"name": {"type": "string", "title": "Name"}, "count": {"type": "integer"}}});
        let asked = request(DecisionKind::Elicitation, json!({"serverName": "fixture", "message": "Which?", "requestedSchema": schema}));
        let (_, kind, tool_use, decision) = decided(respond_action(2, &asked, &Decision::Accept { text: "name: Ada\ncount: 3".into() }).unwrap());
        assert_eq!((kind, tool_use), (WireKind::Elicitation, None));
        assert_eq!(decision, WireDecision::Accept { content: Some(json!({"name": "Ada", "count": 3}).as_object().unwrap().clone()) });
        assert_eq!(decided(respond_action(2, &asked, &Decision::Decline).unwrap()).3, WireDecision::Decline {});
        assert!(matches!(respond_action(2, &asked, &Decision::Allow), Err(AdapterError::InvalidObservation)));
    }
}
