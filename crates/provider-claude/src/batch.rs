//! One orchestrator batch through the Agent SDK host: one prompt, one turn, and every SDK message
//! the turn produced, in order, until its native process exits. The batch's own claim owns the
//! host. What the batch achieved is decided by the caller from these messages, never here.
use crate::adapter::{bootstrap, random_uuid};
use alondra_engine_ports::agent_workers::WorkerConfiguration;
use crate::records::SessionRecord;
use crate::wire::{Connection, ControlIdentity, HostOwner, execution, fence};
use alondra_agent_protocol::{Action, Body, Command, FailureSurface, Observation, SdkEventKind, SdkFailure};
use alondra_engine_ports::agent_host::{HostLaunch, HostStop, OwnedHost};
use alondra_engine_ports::agent_workers::Provider;
use alondra_engine_ports::batch_hosts::{self as port, BatchProvider};
use alondra_engine_ports::journal::BatchOwner;
use alondra_foundation::{CommandId, MessageUuid};
use alondra_provider_contract::{AdapterError, Handoff};
use serde_json::Value;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

pub use alondra_engine_ports::batch_hosts::{DiagnosticSource, HostEvent, McpServer, OrchestratorProfile, PendingHostEvent};

fn diagnostic(payload: &Value, failures: &[SdkFailure]) -> HostEvent {
    if payload["source"] == "stderr" && let Some(text) = payload["text"].as_str() {
        return HostEvent::Diagnostic { source: DiagnosticSource::Stderr, text: text.to_owned() };
    }
    match failures.first() {
        Some(failure) => HostEvent::Diagnostic {
            source: DiagnosticSource::Failure { surface: surface_name(failure.surface), code: failure.code.clone() },
            text: payload["text"].as_str().unwrap_or(&failure.code).to_owned(),
        },
        None => HostEvent::Diagnostic { source: DiagnosticSource::Unknown, text: payload["text"].as_str().unwrap_or("diagnostic").to_owned() },
    }
}

/// The name the protocol gives `surface` on the wire.
fn surface_name(surface: FailureSurface) -> String {
    serde_json::to_value(surface).ok().and_then(|name| name.as_str().map(str::to_owned)).unwrap_or_default()
}

fn host_event(observation: &Observation) -> HostEvent {
    match observation {
        Observation::Sdk { kind: SdkEventKind::Diagnostic, payload, failures, .. } => diagnostic(payload, failures),
        Observation::Sdk { payload, .. } => HostEvent::Sdk(payload.clone()),
        Observation::DecisionRequested { .. } | Observation::DecisionSettled { .. } => HostEvent::UnexpectedDecision,
        Observation::ProcessExited { exit_code, signal } => HostEvent::Exited { code: *exit_code, signal: signal.clone() },
    }
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
    pending: Option<u64>,
}

impl<O: HostOwner> BatchHost<O> {
    /// Starts the host under `owner` and hands it the batch's one prompt, as a new turn of
    /// `session` when `resume`, or as the first turn of a new conversation with that id. Once this
    /// returns the prompt was offered to the SDK; any failure before it means Claude never saw it.
    pub fn start(
        owner: O,
        configuration: &WorkerConfiguration,
        session: &str,
        resume: bool,
        profile: &OrchestratorProfile,
        prompt: &str,
    ) -> Result<Self, AdapterError> {
        let control = crate::adapter::control_of(configuration)?;
        let session = SessionRecord {
            session_id: MessageUuid::new(session).map_err(|_| AdapterError::InvalidObservation)?,
            continued: resume,
        };
        let mut host = OwnedHost::launch(
            &*configuration.hosts,
            owner,
            HostLaunch { artifact: configuration.artifact.clone(), state_dir: &configuration.state_directory, environment: &configuration.host_environment, confinement: configuration.confinement.as_deref() },
            |owner, identity| bootstrap(owner, identity, &control, configuration, &session, Some(profile), None),
        )
        .map_err(|error| AdapterError::Unreachable(format!("launching the host: {error}")))?;
        let deadline = deadline(configuration.io_timeout)?;
        let stream = host.wait_for_socket(deadline).map_err(|error| AdapterError::Unreachable(format!("connecting to the host: {error}")))?;
        let connection = Connection::attach(stream, host.owner(), &crate::wire::LaunchedAs::this_release(host.identity().artifact()), &control, 0, deadline)?;
        let mut batch = Self {
            host,
            connection,
            control,
            io_timeout: configuration.io_timeout,
            cwd: configuration.native_directory.to_string_lossy().into_owned(),
            input: random_uuid()?,
            commands: 0,
            pending: None,
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

    /// Compatibility reader for callers that do not retain a durable batch report.
    pub fn next(&mut self)->Result<HostEvent,AdapterError>{
        let pending=self.next_pending()?;self.acknowledge(pending.cursor)?;Ok(pending.event)
    }

    /// Leaves this exact validated frame unacknowledged until the caller commits its report.
    pub fn next_pending(&mut self)->Result<PendingHostEvent,AdapterError>{
        if self.pending.is_some(){return Err(AdapterError::InvalidObservation)}
        let deadline = deadline(self.io_timeout)?;
        let received = self.connection.next(self.host.owner(), deadline)?;
        let Body::Event(event) = &received.frame().body else {
            return Err(AdapterError::InvalidObservation);
        };
        let seen = host_event(&event.observation);
        let cursor = event.cursor;
        self.pending = Some(cursor);
        Ok(PendingHostEvent { cursor,event:seen,evidence:received.payload().to_vec() })
    }

    pub fn acknowledge(&mut self,cursor:u64)->Result<(),AdapterError>{
        if self.pending!=Some(cursor){return Err(AdapterError::InvalidObservation)}
        self.connection.acknowledge(self.host.owner(),cursor,deadline(self.io_timeout)?)?;
        self.pending=None;Ok(())
    }

    pub fn stopper(&self) -> HostStop {
        self.host.stopper()
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
    alondra_foundation::clock::instant().into_std().checked_add(timeout).ok_or(AdapterError::Disconnected)
}

/// The orchestrator's batches on Claude, which `wire` adds to the registry.
pub struct Batches;

impl BatchProvider for Batches {
    fn provider(&self) -> Provider { Provider::Claude }
    fn start(&self, owner: BatchOwner, configuration: &WorkerConfiguration, session: &str, resume: bool,
        profile: &OrchestratorProfile, prompt: &str) -> Result<Box<dyn port::BatchHost>, AdapterError> {
        Ok(Box::new(BatchHost::start(owner, configuration, session, resume, profile, prompt)?))
    }
}

impl port::BatchHost for BatchHost<BatchOwner> {
    fn input(&self) -> &str { BatchHost::input(self) }
    fn process_id(&self) -> u32 { BatchHost::process_id(self) }
    fn readable(&self, timeout: Duration) -> Result<bool, AdapterError> { BatchHost::readable(self, timeout) }
    fn next_pending(&mut self) -> Result<PendingHostEvent, AdapterError> { BatchHost::next_pending(self) }
    fn acknowledge(&mut self, cursor: u64) -> Result<(), AdapterError> { BatchHost::acknowledge(self, cursor) }
    fn stopper(&self) -> HostStop { BatchHost::stopper(self) }
    fn stop(self: Box<Self>) -> Result<(), AdapterError> { BatchHost::stop(*self) }
}

/// A batch's claim owns its host: the orchestrator's run and conversation, at the one generation
/// a claim has.
impl HostOwner for BatchOwner {
    fn owner_generation(&self) -> u64 { 1 }
    fn epoch(&self) -> &alondra_foundation::DataEpoch { BatchOwner::epoch(self) }
    fn host(&self) -> &str { self.host_name() }
    fn wire_execution(&self) -> alondra_agent_protocol::Execution {
        alondra_agent_protocol::Execution::Orchestrator(alondra_agent_protocol::OrchestratorExecution {
            lane: alondra_agent_protocol::OrchestratorLane::Orchestrator,
            run_id: self.run().to_owned(), conversation_id: self.session().to_owned(), generation: 1,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn observed(observation: Value) -> HostEvent {
        host_event(&serde_json::from_value(observation).unwrap())
    }

    #[test]
    fn a_native_decision_is_its_own_event_and_a_diagnostic_says_where_it_came_from() {
        let phrase = "Unexpected native decision in the Alondra orchestrator";
        assert_eq!(observed(json!({"type": "decision_requested", "request_id": "r-1", "kind": "question", "payload": {}})), HostEvent::UnexpectedDecision);
        assert_eq!(observed(json!({"type": "sdk", "kind": "diagnostic", "native": {}, "payload": {"source": "stderr", "text": phrase}, "failures": []})),
            HostEvent::Diagnostic { source: DiagnosticSource::Stderr, text: phrase.into() });
        assert_eq!(observed(json!({"type": "sdk", "kind": "diagnostic", "native": {}, "payload": {}, "failures": [{"surface": "transport", "code": "decision_too_large", "details": {}}]})),
            HostEvent::Diagnostic { source: DiagnosticSource::Failure { surface: "transport".into(), code: "decision_too_large".into() }, text: "decision_too_large".into() });
        assert_eq!(observed(json!({"type": "sdk", "kind": "diagnostic", "native": {}, "payload": {}, "failures": []})),
            HostEvent::Diagnostic { source: DiagnosticSource::Unknown, text: "diagnostic".into() });
    }
}
