use alondra_agent_protocol::{
    Attach, Body, Command, CommandRegistry, CommandStatus, Execution, Fence, Frame, FramedReader,
    HostExpectation, MAX_FRAME_BYTES, ReceiptOutcome, ReceiptPhase, ReceivedFrame, SDK_VERSION,
    encode, owner_proof, verify_host_proof,
};
use alondra_agent_runtime::owned_host::OwnedHost;
use alondra_provider_contract::{AdapterError, Handoff, PreparedCommand};
use alondra_store::OwnerGrant;
use std::collections::VecDeque;
use std::io::{Cursor, Read, Write};
use std::os::unix::net::UnixStream;
use std::sync::{Arc, Mutex};
use std::time::Instant;

const MAX_QUEUED_EVENTS: usize = 64;

pub(super) struct ControlIdentity {
    pub profile_id: String,
    pub account_id: String,
    pub host_instance_id: String,
    pub secret: [u8; 32],
    pub host_release: String,
}

/// Owned by the private Claude adapter, never returned by the public controller.
pub(super) struct Connection {
    reader: FramedReader<DeadlineReader>,
    read_deadline: Arc<Mutex<Instant>>,
    writer: UnixStream,
    pending_attach: Attach,
    commands: CommandRegistry,
    queued: VecDeque<ReceivedFrame>,
    queued_bytes: usize,
    cursor: LaneCursor,
    failed: bool,
}
impl Connection {
    pub fn attach(
        host: &mut OwnedHost,
        control: &ControlIdentity,
        committed_cursor: u64,
        deadline: Instant,
    ) -> Result<Self, AdapterError> {
        if committed_cursor > alondra_agent_protocol::MAX_SAFE_INTEGER {
            return Err(invalid("persisted host cursor bound"));
        }
        let fence = fence(host.owner(), control);
        let execution = execution(host.owner());
        fence.validate().map_err(protocol)?;
        execution.validate().map_err(protocol)?;
        let stream = host.wait_for_socket(deadline).map_err(disconnected)?;
        let mut writer = stream.try_clone().map_err(disconnected)?;
        let read_deadline = Arc::new(Mutex::new(deadline));
        let mut reader = FramedReader::new(DeadlineReader {
            stream,
            deadline: read_deadline.clone(),
        });
        let hello = receive(&mut reader, &read_deadline, deadline)?;
        let Body::Hello(hello) = hello.frame().body.clone() else {
            return Err(invalid("expected host hello"));
        };
        hello
            .negotiate(&HostExpectation {
                fence: &fence,
                host_release: &control.host_release,
                host_sha256: host.identity().artifact().artifact_sha256(),
                required_capabilities: &["streaming_input", "owner_attach", "commit_ack"],
            })
            .map_err(protocol)?;
        let challenge = receive(&mut reader, &read_deadline, deadline)?;
        let Body::Challenge(challenge) = challenge.frame().body.clone() else {
            return Err(invalid("expected host challenge"));
        };
        if challenge.fence != fence
            || challenge.execution != execution
            || challenge.host_instance_id != control.host_instance_id
            || challenge.executable_sha256 != host.identity().artifact().executable_sha256()
        {
            return Err(invalid("host challenge identity mismatch"));
        }
        let mut pending_attach = Attach {
            fence: fence.clone(),
            execution: execution.clone(),
            host_instance_id: control.host_instance_id.clone(),
            nonce: challenge.nonce,
            owner_generation: host.owner().generation(),
            host_release: control.host_release.clone(),
            host_sha256: host.identity().artifact().artifact_sha256().into(),
            executable_sha256: host.identity().artifact().executable_sha256().into(),
            sdk_version: SDK_VERSION.into(),
            proof: String::new(),
        };
        pending_attach.proof = owner_proof(&control.secret, &pending_attach).map_err(protocol)?;
        write(
            &mut writer,
            &encode(&Frame::new(Body::Attach(pending_attach.clone()))).map_err(protocol)?,
            deadline,
        )?;
        let attached = receive(&mut reader, &read_deadline, deadline)?;
        let Body::Attached(attached) = &attached.frame().body else {
            return Err(invalid("expected authenticated host response"));
        };
        verify_host_proof(&control.secret, &pending_attach, attached).map_err(protocol)?;
        Ok(Self {
            reader,
            read_deadline,
            writer,
            pending_attach,
            commands: CommandRegistry::new(fence, execution).map_err(protocol)?,
            queued: VecDeque::new(),
            queued_bytes: 0,
            cursor: LaneCursor {
                committed: committed_cursor,
                delivered: None,
            },
            failed: false,
        })
    }

    /// The caller is the private adapter's submit path, after the controller has
    /// persisted exact command bytes and rechecked its real DispatchAttempt.
    pub fn submit(
        &mut self,
        owner: &OwnerGrant,
        prepared: &PreparedCommand,
        deadline: Instant,
    ) -> Result<Handoff, AdapterError> {
        self.check_owner(owner)?;
        let result = self.submit_inner(prepared, deadline);
        if result.is_err() {
            self.failed = true;
        }
        result
    }
    fn submit_inner(
        &mut self,
        prepared: &PreparedCommand,
        deadline: Instant,
    ) -> Result<Handoff, AdapterError> {
        let mut input = FramedReader::new(Cursor::new(prepared.bytes()));
        let received = input
            .read_received()
            .map_err(protocol)?
            .ok_or_else(|| invalid("missing command"))?;
        if input.read_received().map_err(protocol)?.is_some() {
            return Err(invalid("multiple prepared commands"));
        }
        let Body::Command(command) = &received.frame().body else {
            return Err(invalid("prepared value is not a command"));
        };
        if encode(received.frame()).map_err(protocol)? != prepared.bytes() {
            return Err(invalid("prepared command encoding changed"));
        }
        if self.commands.record(command).map_err(protocol)? == CommandStatus::Duplicate {
            return Err(invalid("command replay requires positive reconciliation"));
        }
        write(&mut self.writer, prepared.bytes(), deadline)?;
        let expected_command = command.command_id.clone();
        loop {
            let received = receive(&mut self.reader, &self.read_deadline, deadline)?;
            match &received.frame().body {
                Body::Receipt(receipt) => {
                    self.commands.receipt(receipt).map_err(protocol)?;
                    if receipt.command_id == expected_command {
                        match &receipt.outcome {
                            ReceiptOutcome::Rejected { .. } => {
                                return Ok(Handoff::RefusedBeforeOffer);
                            }
                            ReceiptOutcome::Accepted {
                                phase: ReceiptPhase::Offered | ReceiptPhase::Applied,
                            } => return Ok(Handoff::Offered),
                            ReceiptOutcome::Accepted {
                                phase: ReceiptPhase::Admitted,
                            } => {}
                        }
                    }
                }
                Body::Event(_) => self.queue(received)?,
                _ => return Err(invalid("unexpected host response after attach")),
            }
        }
    }
    pub fn prepare(
        &self,
        owner: &OwnerGrant,
        command: Command,
    ) -> Result<PreparedCommand, AdapterError> {
        self.check_owner(owner)?;
        if command.fence != self.pending_attach.fence
            || command.execution != self.pending_attach.execution
        {
            return Err(invalid("command owner identity mismatch"));
        }
        PreparedCommand::new(encode(&Frame::new(Body::Command(command))).map_err(protocol)?)
    }
    pub fn next(
        &mut self,
        owner: &OwnerGrant,
        deadline: Instant,
    ) -> Result<ReceivedFrame, AdapterError> {
        self.check_owner(owner)?;
        let result = self.next_inner(deadline);
        if result.is_err() {
            self.failed = true;
        }
        result
    }
    fn next_inner(&mut self, deadline: Instant) -> Result<ReceivedFrame, AdapterError> {
        loop {
            let received = if let Some(frame) = self.queued.pop_front() {
                self.queued_bytes -= frame.payload().len();
                frame
            } else {
                receive(&mut self.reader, &self.read_deadline, deadline)?
            };
            match &received.frame().body {
                Body::Receipt(receipt) => {
                    self.commands.receipt(receipt).map_err(protocol)?;
                }
                Body::Event(event) => {
                    self.check_event(&received)?;
                    self.cursor.deliver(event.cursor)?;
                    return Ok(received);
                }
                _ => return Err(invalid("unexpected host observation")),
            }
        }
    }
    fn check_event(&self, frame: &ReceivedFrame) -> Result<(), AdapterError> {
        let Body::Event(event) = &frame.frame().body else {
            return Err(invalid("not a host event"));
        };
        if event.fence != self.pending_attach.fence
            || event.execution != self.pending_attach.execution
        {
            return Err(invalid("host event identity mismatch"));
        }
        Ok(())
    }
    fn queue(&mut self, frame: ReceivedFrame) -> Result<(), AdapterError> {
        self.check_event(&frame)?;
        let size = self
            .queued_bytes
            .checked_add(frame.payload().len())
            .filter(|size| *size <= MAX_FRAME_BYTES)
            .ok_or_else(|| invalid("host event byte capacity"))?;
        if self.queued.len() >= MAX_QUEUED_EVENTS {
            return Err(invalid("host event count capacity"));
        }
        self.queued.push_back(frame);
        self.queued_bytes = size;
        Ok(())
    }
    /// Only WorkerController invokes this after evidence, checkpoint and facts commit.
    pub fn acknowledge(
        &mut self,
        owner: &OwnerGrant,
        cursor: u64,
        deadline: Instant,
    ) -> Result<(), AdapterError> {
        self.check_owner(owner)?;
        if self.cursor.delivered != Some(cursor) {
            return Err(invalid("ACK exceeds delivered evidence"));
        }
        let frame = Frame::new(Body::Acknowledge(alondra_agent_protocol::Acknowledge {
            fence: self.pending_attach.fence.clone(),
            execution: self.pending_attach.execution.clone(),
            host_instance_id: self.pending_attach.host_instance_id.clone(),
            owner_generation: self.pending_attach.owner_generation,
            cursor,
        }));
        let result = write(
            &mut self.writer,
            &encode(&frame).map_err(protocol)?,
            deadline,
        );
        if result.is_err() {
            self.failed = true;
        } else {
            self.cursor.committed = self.cursor.committed.max(cursor);
        }
        result
    }
    fn check_owner(&self, owner: &OwnerGrant) -> Result<(), AdapterError> {
        if self.failed {
            return Err(AdapterError::Disconnected);
        }
        if owner.generation() != self.pending_attach.owner_generation
            || owner.scope().epoch() != &self.pending_attach.fence.data_epoch
            || execution(owner) != self.pending_attach.execution
            || owner.execution().host().as_str() != self.pending_attach.fence.host_id
        {
            return Err(invalid("stale host owner"));
        }
        Ok(())
    }
}
struct LaneCursor {
    committed: u64,
    delivered: Option<u64>,
}
impl LaneCursor {
    fn deliver(&mut self, cursor: u64) -> Result<(), AdapterError> {
        let valid = cursor > 0
            && match self.delivered {
                Some(previous) => previous.checked_add(1) == Some(cursor),
                None => cursor <= self.committed.saturating_add(1),
            };
        if !valid {
            return Err(invalid("host event cursor gap"));
        }
        self.delivered = Some(cursor);
        Ok(())
    }
}
struct DeadlineReader {
    stream: UnixStream,
    deadline: Arc<Mutex<Instant>>,
}
impl Read for DeadlineReader {
    fn read(&mut self, bytes: &mut [u8]) -> std::io::Result<usize> {
        let deadline = *self
            .deadline
            .lock()
            .map_err(|_| std::io::Error::other("deadline lock"))?;
        read_bounded(&mut self.stream, bytes, deadline, Instant::now())
    }
}
fn receive(
    reader: &mut FramedReader<DeadlineReader>,
    shared_deadline: &Arc<Mutex<Instant>>,
    deadline: Instant,
) -> Result<ReceivedFrame, AdapterError> {
    *shared_deadline
        .lock()
        .map_err(|_| AdapterError::Disconnected)? = deadline;
    reader
        .read_received()
        .map_err(protocol)?
        .ok_or(AdapterError::Disconnected)
}
fn write(writer: &mut UnixStream, bytes: &[u8], deadline: Instant) -> Result<(), AdapterError> {
    write_bounded(writer, bytes, deadline, Instant::now).map_err(disconnected)
}
fn write_bounded<W: Write + TimeoutWriter>(
    writer: &mut W,
    mut bytes: &[u8],
    deadline: Instant,
    mut now: impl FnMut() -> Instant,
) -> std::io::Result<()> {
    while !bytes.is_empty() {
        writer.timeout(budget(deadline, now())?)?;
        match writer.write(bytes) {
            Ok(0) => return Err(std::io::Error::from(std::io::ErrorKind::WriteZero)),
            Ok(count) => bytes = &bytes[count..],
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(error),
        }
    }
    writer.timeout(budget(deadline, now())?)?;
    writer.flush()
}
trait TimeoutWriter {
    fn timeout(&self, duration: std::time::Duration) -> std::io::Result<()>;
}
impl TimeoutWriter for UnixStream {
    fn timeout(&self, duration: std::time::Duration) -> std::io::Result<()> {
        self.set_write_timeout(Some(duration))
    }
}
fn budget(deadline: Instant, now: Instant) -> std::io::Result<std::time::Duration> {
    deadline
        .checked_duration_since(now)
        .filter(|duration| !duration.is_zero())
        .ok_or_else(|| std::io::Error::from(std::io::ErrorKind::TimedOut))
}
fn disconnected(_: std::io::Error) -> AdapterError {
    AdapterError::Disconnected
}
fn invalid(message: &str) -> AdapterError {
    AdapterError::Protocol(message.into())
}
fn protocol(error: alondra_agent_protocol::Error) -> AdapterError {
    AdapterError::Protocol(error.to_string())
}
pub(super) fn fence(owner: &OwnerGrant, control: &ControlIdentity) -> Fence {
    Fence {
        profile_id: control.profile_id.clone(),
        account_id: control.account_id.clone(),
        data_epoch: owner.scope().epoch().clone(),
        host_id: owner.execution().host().as_str().into(),
    }
}
pub(super) fn execution(owner: &OwnerGrant) -> Execution {
    let stamp = owner.execution();
    Execution {
        task_id: stamp.task().clone(),
        agent_id: stamp.node().clone(),
        conversation_id: stamp.conversation().as_str().into(),
        process_instance_id: stamp.instance().as_str().into(),
        generation: stamp.generation(),
    }
}

trait TimeoutReader {
    fn timeout(&self, duration: std::time::Duration) -> std::io::Result<()>;
}
impl TimeoutReader for UnixStream {
    fn timeout(&self, duration: std::time::Duration) -> std::io::Result<()> {
        self.set_read_timeout(Some(duration))
    }
}
fn read_bounded<R: Read + TimeoutReader>(
    reader: &mut R,
    bytes: &mut [u8],
    deadline: Instant,
    now: Instant,
) -> std::io::Result<usize> {
    reader.timeout(budget(deadline, now)?)?;
    reader.read(bytes)
}
#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;
    #[test]
    fn lane_replay_precedes_durable_commit_without_skipping_wire_evidence() {
        let mut cursor = LaneCursor {
            committed: 12,
            delivered: None,
        };
        cursor.deliver(11).unwrap();
        assert_eq!(cursor.delivered, Some(11));
        cursor.deliver(12).unwrap();
        assert!(cursor.deliver(14).is_err());
        assert_eq!(cursor.delivered, Some(12));
        cursor.deliver(13).unwrap();
        assert!(cursor.deliver(13).is_err());
        let mut fresh = LaneCursor {
            committed: 0,
            delivered: None,
        };
        assert!(fresh.deliver(0).is_err());
        assert!(fresh.deliver(2).is_err());
        assert_eq!(fresh.delivered, None);
        fresh.deliver(1).unwrap();
    }
    struct Fragmented {
        bytes: Cursor<Vec<u8>>,
        deadline: Instant,
        clock: std::vec::IntoIter<Instant>,
    }
    impl TimeoutReader for Cursor<Vec<u8>> {
        fn timeout(&self, _: Duration) -> std::io::Result<()> {
            Ok(())
        }
    }
    impl Read for Fragmented {
        fn read(&mut self, bytes: &mut [u8]) -> std::io::Result<usize> {
            let count = bytes.len().min(1);
            read_bounded(
                &mut self.bytes,
                &mut bytes[..count],
                self.deadline,
                self.clock.next().unwrap_or(self.deadline),
            )
        }
    }
    #[test]
    fn fragmented_frame_progress_cannot_reset_absolute_deadline() {
        let now = Instant::now();
        let deadline = now + Duration::from_secs(1);
        let bytes = include_bytes!("../../agent-protocol/fixtures/hello.json");
        let mut framed = (bytes.len() as u32).to_be_bytes().to_vec();
        framed.extend_from_slice(bytes);
        let reader = Fragmented {
            bytes: Cursor::new(framed),
            deadline,
            clock: vec![now, now, deadline].into_iter(),
        };
        let mut reader = FramedReader::new(reader);
        let error = reader
            .read_received()
            .err()
            .expect("fragmented frame must time out");
        assert!(
            matches!(error, alondra_agent_protocol::Error::Io(ref error) if error.kind() == std::io::ErrorKind::TimedOut)
        );
        assert!(matches!(
            reader.read_received(),
            Err(alondra_agent_protocol::Error::Poisoned)
        ));
    }
    struct PartialWriter(Vec<u8>);
    impl TimeoutWriter for PartialWriter {
        fn timeout(&self, _: Duration) -> std::io::Result<()> {
            Ok(())
        }
    }
    impl Write for PartialWriter {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0.push(bytes[0]);
            Ok(1)
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    #[test]
    fn fragmented_command_write_cannot_reset_absolute_deadline() {
        let now = Instant::now();
        let deadline = now + Duration::from_secs(1);
        let mut clock = vec![now, deadline].into_iter();
        let mut writer = PartialWriter(Vec::new());
        let error = write_bounded(&mut writer, b"command", deadline, || {
            clock.next().unwrap_or(deadline)
        })
        .unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::TimedOut);
        assert_eq!(writer.0, b"c");
    }
}
