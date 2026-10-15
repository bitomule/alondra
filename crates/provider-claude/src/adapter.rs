use crate::normalizer::Normalizer;
use crate::records::{InputRecord, SessionRecord};
use crate::wire::{Connection, ControlIdentity, execution, fence};
use alondra_agent_application::{DurableAdapter, InputMetadata, WorkerController};
use alondra_agent_protocol::{Action, Body, Command, SDK_VERSION};
use alondra_agent_runtime::owned_host::{LaunchIdentity, OwnedHost, VerifiedArtifact};
use alondra_foundation::{ExecutionStamp, MessageUuid, TaskId};
use alondra_provider_contract::{
    Adapter, AdapterError, Handoff, Invocation, Operation, OrderedObservation, Placement,
    PreparedCommand, ProviderBinding,
};
use alondra_store::{AdapterState, AuthorityScope, InputPayload, OwnerGrant};
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

/// The submitting implementation and its control credentials never escape this wrapper.
pub struct ClaudeWorker {
    controller: WorkerController<ClaudeAdapter>,
}
impl ClaudeWorker {
    pub fn open(
        connection: rusqlite::Connection,
        scope: AuthorityScope,
        binding: ProviderBinding,
        configuration: ClaudeConfiguration,
    ) -> Result<Self, alondra_agent_application::Error> {
        let adapter = ClaudeAdapter::new(binding, configuration)?;
        Ok(Self {
            controller: WorkerController::open(connection, scope, adapter)?,
        })
    }
    pub fn launch(
        &mut self,
        task: &TaskId,
        input: InputPayload,
        admission: alondra_domain::Event,
        placement: Placement,
    ) -> Result<(), alondra_agent_application::Error> {
        self.controller.launch(task, input, admission, placement)
    }
    pub fn dispatch_next(&mut self) -> Result<Option<Handoff>, alondra_agent_application::Error> {
        let Some(permit) = self.controller.next_permit() else {
            return Ok(None);
        };
        let attempt = self.controller.claim(permit)?;
        self.controller.dispatch(attempt).map(Some)
    }
    pub fn observe(&mut self) -> Result<bool, alondra_agent_application::Error> {
        self.controller.observe()
    }
    pub fn load(
        &mut self,
        task: &TaskId,
    ) -> Result<alondra_domain::TaskLifecycle, alondra_agent_application::Error> {
        self.controller.load(task)
    }
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
            Ok(bytes) => {
                let _: SessionRecord =
                    serde_json::from_slice(&bytes).map_err(|_| AdapterError::InvalidObservation)?;
            }
            Err(alondra_store::Error::MissingAdapterInput) => metadata
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
            Ok(bytes) => {
                let _: InputRecord =
                    serde_json::from_slice(&bytes).map_err(|_| AdapterError::InvalidObservation)?;
            }
            Err(alondra_store::Error::MissingAdapterInput) => metadata
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
        serde_json::from_slice(&metadata.input().map_err(storage)?)
            .map_err(|_| AdapterError::InvalidObservation)
    }
    fn install_owner(
        &mut self,
        owner: OwnerGrant,
        state: AdapterState,
    ) -> Result<(), AdapterError> {
        if owner.binding() != &self.binding {
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
