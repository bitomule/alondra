//! One orchestrator batch through the Agent SDK host: one prompt, one turn, and every SDK message
//! the turn produced, in order, until its native process exits. The batch's own claim owns the
//! host. What the batch achieved is decided by the caller from these messages, never here.
use crate::adapter::{ClaudeConfiguration, bootstrap, random_uuid};
use crate::records::SessionRecord;
use crate::wire::{Connection, ControlIdentity, HostOwner, execution, fence};
use alondra_agent_protocol::{Action, Body, Command, Observation, SdkEventKind};
use alondra_agent_runtime::owned_host::OwnedHost;
use alondra_foundation::{CommandId, MessageUuid};
use alondra_provider_contract::{AdapterError, Handoff};
use serde::Serialize;
use serde_json::Value;
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// The Claude an orchestrator batch runs: its model, its tools and MCP server and nothing else,
/// its plugins and settings, and whether it resumes the conversation of the previous batch.
#[derive(Clone, Debug, Serialize)]
pub struct OrchestratorProfile {
    pub resume: bool,
    pub model: String,
    pub tools: Vec<String>,
    pub disallowed_tools: Vec<String>,
    pub mcp_servers: BTreeMap<String, McpServer>,
    pub plugins: Vec<PathBuf>,
    pub settings: serde_json::Map<String, Value>,
}

#[derive(Clone, Debug, Serialize)]
pub struct McpServer {
    pub command: PathBuf,
    pub args: Vec<String>,
}

/// What the host saw of the batch's turn, in its order.
#[derive(Clone, Debug, PartialEq)]
pub enum HostEvent {
    /// A message of the SDK, exactly as it came.
    Sdk(Value),
    /// What Claude wrote to stderr, or why the host could not read the SDK any further.
    Diagnostic(String),
    /// Claude's process ended.
    Exited { code: Option<i32>, signal: Option<String> },
}

/// The host of one batch. Its input takes one prompt: the host refuses any later one, so a
/// message that arrives during the turn waits for the next batch and its own host.
pub struct BatchHost<O: HostOwner> {
    host: OwnedHost<O>,
    connection: Connection,
    control: ControlIdentity,
    io_timeout: Duration,
    cwd: String,
    input: MessageUuid,
    commands: u32,
}

impl<O: HostOwner> BatchHost<O> {
    /// Starts the host under `owner` and hands it the batch's one prompt. Once this returns the
    /// prompt was offered to the SDK; any failure before it means Claude never saw it.
    pub fn start(
        owner: O,
        configuration: &ClaudeConfiguration,
        session: &str,
        profile: &OrchestratorProfile,
        prompt: &str,
    ) -> Result<Self, AdapterError> {
        let control = configuration.control()?;
        let session = SessionRecord {
            session_id: MessageUuid::new(session).map_err(|_| AdapterError::InvalidObservation)?,
        };
        let mut host = OwnedHost::launch_confined(
            owner,
            configuration.artifact.clone(),
            &configuration.state_directory,
            &configuration.host_environment,
            configuration.confinement.as_deref(),
            |owner, identity| bootstrap(owner, identity, &control, configuration, &session, Some(profile), None),
        )
        .map_err(|_| AdapterError::Disconnected)?;
        let deadline = deadline(configuration.io_timeout)?;
        let stream = host.wait_for_socket(deadline).map_err(|_| AdapterError::Disconnected)?;
        let connection = Connection::attach(stream, host.owner(), host.identity().artifact(), &control, 0, deadline)?;
        let mut batch = Self {
            host,
            connection,
            control,
            io_timeout: configuration.io_timeout,
            cwd: configuration.native_directory.to_string_lossy().into_owned(),
            input: random_uuid()?,
            commands: 0,
        };
        let start = Action::Start { cwd: batch.cwd.clone(), input_id: batch.input.clone(), prompt: prompt.into() };
        match batch.send(start)? {
            Handoff::Offered => Ok(batch),
            Handoff::RefusedBeforeOffer => Err(AdapterError::Unsupported),
            Handoff::Uncertain => Err(AdapterError::Disconnected),
        }
    }

    /// The id the SDK reports back for the batch's prompt: a result that names another is not
    /// this batch's.
    pub fn input(&self) -> &str {
        self.input.as_str()
    }

    /// Hands the host another prompt. An orchestrator host refuses it: one batch, one turn.
    pub fn offer(&mut self, prompt: &str) -> Result<Handoff, AdapterError> {
        self.send(Action::Offer { input_id: random_uuid()?, prompt: prompt.into() })
    }

    /// The host's process id: it leads the process group Claude and its MCP server run in.
    pub fn process_id(&self) -> u32 {
        self.host.process_id()
    }

    /// Asks the SDK to stop the turn.
    pub fn interrupt(&mut self) -> Result<Handoff, AdapterError> {
        self.send(Action::Interrupt)
    }

    /// Whether the host has something to read within `timeout`. Waiting decides nothing: it only
    /// lets the caller look at a cancellation between reads.
    pub fn readable(&self, timeout: Duration) -> Result<bool, AdapterError> {
        self.connection.readable(timeout)
    }

    /// The next thing the host saw, acknowledged once read: the batch keeps no host evidence of
    /// its own, its claim is the record.
    pub fn next(&mut self) -> Result<HostEvent, AdapterError> {
        let deadline = deadline(self.io_timeout)?;
        let received = self.connection.next(self.host.owner(), deadline)?;
        let Body::Event(event) = &received.frame().body else {
            return Err(AdapterError::InvalidObservation);
        };
        let seen = match &event.observation {
            Observation::Sdk { kind: SdkEventKind::Diagnostic, payload, failures, .. } => HostEvent::Diagnostic(
                payload["text"].as_str().map(str::to_owned)
                    .or_else(|| failures.first().map(|failure| failure.code.clone()))
                    .unwrap_or_else(|| "diagnostic".into()),
            ),
            Observation::Sdk { payload, .. } => HostEvent::Sdk(payload.clone()),
            Observation::DecisionRequested { .. } | Observation::DecisionSettled { .. } => return Err(AdapterError::Unsupported),
            Observation::ProcessExited { exit_code, signal } => HostEvent::Exited { code: *exit_code, signal: signal.clone() },
        };
        let cursor = event.cursor;
        self.connection.acknowledge(self.host.owner(), cursor, deadline)?;
        Ok(seen)
    }

    /// Stops the host and waits until it has exited.
    pub fn stop(mut self) -> Result<(), AdapterError> {
        self.host.request_stop().map_err(|_| AdapterError::Disconnected)?;
        self.host.wait().map_err(|_| AdapterError::Disconnected)?;
        Ok(())
    }

    fn send(&mut self, action: Action) -> Result<Handoff, AdapterError> {
        self.commands += 1;
        let owner = self.host.owner();
        let command = Command {
            fence: fence(owner, &self.control),
            execution: execution(owner),
            command_id: CommandId(format!("{}-{}", self.control.host_instance_id, self.commands)),
            deadline_ms: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_err(|_| AdapterError::Disconnected)?
                .checked_add(self.io_timeout)
                .and_then(|duration| u64::try_from(duration.as_millis()).ok())
                .ok_or(AdapterError::Disconnected)?,
            action,
        };
        let prepared = self.connection.prepare(owner, command)?;
        self.connection.submit(owner, &prepared, deadline(self.io_timeout)?)
    }
}

fn deadline(timeout: Duration) -> Result<Instant, AdapterError> {
    Instant::now().checked_add(timeout).ok_or(AdapterError::Disconnected)
}
