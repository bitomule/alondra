use crate::normalizer::Normalizer;
use crate::records::{InputRecord, SessionRecord};
use crate::wire::{Connection, ControlIdentity, execution, fence};
use alondra_agent_application::{DurableAdapter, InputMetadata, ObservationNormalizer, invocation};
use alondra_agent_protocol::{Action, Body, Command, SDK_VERSION};
use alondra_agent_runtime::owned_host::{LaunchIdentity, OwnedHost, VerifiedArtifact};
use alondra_foundation::{ExecutionStamp, MessageUuid};
use alondra_provider_contract::{
    Adapter, AdapterError, Handoff, Invocation, Operation, OrderedObservation, Placement,
    PreparedCommand, ProviderBinding,
};
use alondra_store::{
    AdapterState, CommittedCommand, CommittedObservation, DispatchAttempt, OwnerGrant,
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
/// use alondra_store::CommittedCommand;
/// fn submit(worker: &mut ClaudeWorker, value: &CommittedCommand) { let _ = worker.submit(value); }
/// ```
pub struct ClaudeWorker {
    adapter: ClaudeAdapter,
    prepared: Option<PreparedDispatch>,
    pending: Option<OrderedObservation>,
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
    pub fn poll(&mut self) -> Result<Option<OrderedObservation>, AdapterError> {
        if let Some(pending) = &self.pending {
            return Ok(Some(pending.clone()));
        }
        let received = self.adapter.next()?;
        self.pending = received.clone();
        Ok(received)
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

struct ClaudeAdapter {
    binding: ProviderBinding,
    configuration: ClaudeConfiguration,
    control: ControlIdentity,
    host: Option<OwnedHost>,
    connection: Option<Connection>,
    normalizer: Normalizer,
}
impl ClaudeAdapter {
    fn new(
        binding: ProviderBinding,
        configuration: ClaudeConfiguration,
    ) -> Result<Self, AdapterError> {
        if configuration.io_timeout.is_zero()
            || configuration.io_timeout > Duration::from_secs(300)
            || !configuration.native_executable.is_absolute()
            || !configuration.native_directory.is_absolute()
            || configuration.profile_id.trim().is_empty()
            || configuration.account_id.trim().is_empty()
            || configuration.host_release.trim().is_empty()
        {
            return Err(AdapterError::InvalidObservation);
        }
        let mut secret = [0; 32];
        getrandom::fill(&mut secret).map_err(|_| AdapterError::Disconnected)?;
        let control = ControlIdentity {
            profile_id: configuration.profile_id.clone(),
            account_id: configuration.account_id.clone(),
            host_instance_id: random_uuid()?.as_str().into(),
            secret,
            host_release: configuration.host_release.clone(),
        };
        Ok(Self {
            binding,
            configuration,
            control,
            host: None,
            connection: None,
            normalizer: Normalizer {
                session: None,
                fence: None,
            },
        })
    }
    fn deadline(&self) -> Result<Instant, AdapterError> {
        Instant::now()
            .checked_add(self.configuration.io_timeout)
            .ok_or(AdapterError::Disconnected)
    }
    fn host(&self) -> Result<&OwnedHost, AdapterError> {
        self.host.as_ref().ok_or(AdapterError::Disconnected)
    }
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
                    command_id: invocation.command,
                    deadline_ms,
                    action,
                },
            )
    }
    fn submit(
        &mut self,
        invocation: Invocation<'_>,
        command: &PreparedCommand,
    ) -> Result<Handoff, AdapterError> {
        let deadline = self.deadline()?;
        let host = self.host.as_ref().ok_or(AdapterError::Disconnected)?;
        if invocation.binding != &self.binding || invocation.execution != host.owner().execution() {
            return Err(AdapterError::InvalidObservation);
        }
        self.connection
            .as_mut()
            .ok_or(AdapterError::Disconnected)?
            .submit(host.owner(), command, deadline)
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
                    serde_json::to_vec(&SessionRecord {
                        session_id: random_uuid()?,
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
        let host = OwnedHost::launch(
            owner,
            self.configuration.artifact.clone(),
            &self.configuration.state_directory,
            &self.configuration.host_environment,
            |grant, identity| {
                bootstrap(
                    grant,
                    identity,
                    &self.control,
                    &self.configuration,
                    &session,
                )
            },
        )
        .map_err(|_| AdapterError::Disconnected)?;
        self.host = Some(host);
        self.normalizer.session = Some(session);
        self.normalizer.fence = Some(expected);
        let deadline = self.deadline()?;
        self.connection = Some(Connection::attach(
            self.host.as_mut().ok_or(AdapterError::Disconnected)?,
            &self.control,
            0,
            deadline,
        )?);
        Ok(())
    }
}
fn bootstrap(
    owner: &OwnerGrant,
    identity: &LaunchIdentity,
    control: &ControlIdentity,
    configuration: &ClaudeConfiguration,
    session: &SessionRecord,
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
    serde_json::to_vec(&serde_json::json!({
        "state_dir": identity.state_dir(), "ownership_path": identity.ownership_path(), "socket_path": identity.socket_path(), "host_instance_id": control.host_instance_id, "control_secret": secret,
        "fence": fence(owner, control), "execution": execution(owner),
        "manifest": { "mode": "node_bundle", "artifact_path": identity.artifact().artifact(), "host_release": control.host_release, "host_sha256": identity.artifact().artifact_sha256(), "executable_sha256": identity.artifact().executable_sha256(), "sdk_version": SDK_VERSION },
        "native": { "session_id": session.session_id, "executable": configuration.native_executable, "cwd": configuration.native_directory, "env": configuration.native_environment }
    })).map_err(std::io::Error::other)
}
fn random_uuid() -> Result<MessageUuid, AdapterError> {
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
