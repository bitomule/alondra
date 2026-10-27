//! What the journal of a host that ended proves, read without changing it. The host appends and
//! fsyncs each command, receipt and event before it acts on it, so the durable journal is the
//! host's own evidence; a frame cut short at its end is a write that never completed.
use alondra_codex_protocol::{Body, Execution, Fence, FramedReader, ReceiptOutcome, ReceiptPhase};
use alondra_provider_contract::AdapterError;
use std::collections::BTreeMap;
use std::path::Path;

/// The last durable receipt the ended host kept for a command.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Retained {
    /// The input reached the native session (or the decision was applied).
    Offered,
    /// Refused before anything was offered.
    Refused,
    /// The host took the command and may have acted on it: its outcome is unknown.
    AdmittedOnly,
    /// The host never took the command: nothing of it reached the native session.
    Absent,
}

/// One unacknowledged event of the ended host, with its exact payload bytes.
#[derive(Debug)]
pub struct RetainedEvent {
    pub cursor: u64,
    pub payload: Vec<u8>,
}

#[derive(Debug, Default)]
pub struct EndedJournal {
    commands: BTreeMap<String, (String, Option<ReceiptOutcome>)>,
    events: Vec<RetainedEvent>,
    last_cursor: u64,
    uncertain: bool,
}

impl EndedJournal {
    /// Reads `state_dir/outbox`, refusing any frame of another fence, execution or host instance.
    /// Events at or below `committed` are already in the store and are left out.
    pub fn read(state_dir: &Path, fence: &Fence, execution: &Execution, host_instance_id: &str, committed: u64) -> Result<Self, AdapterError> {
        let mut journal = Self { last_cursor: committed, ..Self::default() };
        journal.uncertain = state_dir.join("uncertain").try_exists().map_err(|_| AdapterError::Disconnected)?;
        let file = match std::fs::File::open(state_dir.join("outbox")) {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(journal),
            Err(_) => return Err(AdapterError::Disconnected),
        };
        let mut reader = FramedReader::new(std::io::BufReader::new(file));
        loop {
            let received = match reader.read_received() {
                Ok(Some(received)) => received,
                Ok(None) => break,
                Err(alondra_codex_protocol::Error::TruncatedHeader | alondra_codex_protocol::Error::TruncatedPayload) => break,
                Err(_) => {
                    journal.uncertain = true;
                    break;
                }
            };
            let (identity, instance) = match &received.frame().body {
                Body::Command(command) => ((&command.fence, &command.execution), None),
                Body::Receipt(receipt) => ((&receipt.fence, &receipt.execution), None),
                Body::Event(event) => ((&event.fence, &event.execution), None),
                Body::Attached(attached) => ((&attached.fence, &attached.execution), Some(&attached.host_instance_id)),
                Body::Acknowledge(ack) => ((&ack.fence, &ack.execution), Some(&ack.host_instance_id)),
                _ => return Err(AdapterError::InvalidObservation),
            };
            if identity != (fence, execution) || instance.is_some_and(|instance| instance != host_instance_id) {
                return Err(AdapterError::InvalidObservation);
            }
            match &received.frame().body {
                Body::Command(command) => {
                    let id = command.command_id.0.as_str().to_owned();
                    match journal.commands.get(&id) {
                        Some((digest, _)) if digest != received.digest() => return Err(AdapterError::InvalidObservation),
                        Some(_) => {}
                        None => {
                            journal.commands.insert(id, (received.digest().to_owned(), None));
                        }
                    }
                }
                Body::Receipt(receipt) => {
                    let entry = journal.commands.get_mut(receipt.command_id.0.as_str()).ok_or(AdapterError::InvalidObservation)?;
                    if entry.0 != receipt.command_sha256 {
                        return Err(AdapterError::InvalidObservation);
                    }
                    entry.1 = Some(receipt.outcome.clone());
                }
                Body::Event(event) if event.cursor > committed => {
                    if event.cursor != journal.last_cursor + 1 {
                        return Err(AdapterError::InvalidObservation);
                    }
                    journal.last_cursor = event.cursor;
                    journal.events.push(RetainedEvent { cursor: event.cursor, payload: received.payload().to_vec() });
                }
                _ => {}
            }
        }
        Ok(journal)
    }

    /// No journal survives (its folder is gone, as `/tmp` is after a reboot): nothing proves what
    /// the host took, so every unresolved command stays uncertain and the next host continues from
    /// the committed head.
    pub fn without_evidence(committed: u64) -> Self {
        Self { last_cursor: committed, uncertain: true, ..Self::default() }
    }

    /// The host's answer for a command, by its exact recorded bytes. Without trustworthy storage an
    /// absent command may have been lost rather than never taken, so it stays uncertain.
    pub fn retained(&self, command: &[u8]) -> Result<Retained, AdapterError> {
        let received = FramedReader::new(std::io::Cursor::new(command))
            .read_received()
            .map_err(|_| AdapterError::InvalidObservation)?
            .ok_or(AdapterError::InvalidObservation)?;
        let Body::Command(sent) = &received.frame().body else {
            return Err(AdapterError::InvalidObservation);
        };
        let absent = if self.uncertain { Retained::AdmittedOnly } else { Retained::Absent };
        Ok(match self.commands.get(sent.command_id.0.as_str()) {
            None => absent,
            Some((digest, _)) if digest != received.digest() => return Err(AdapterError::InvalidObservation),
            Some((_, Some(ReceiptOutcome::Accepted { phase: ReceiptPhase::Offered | ReceiptPhase::Applied }))) => Retained::Offered,
            Some((_, Some(ReceiptOutcome::Rejected { .. }))) => Retained::Refused,
            Some((_, _)) => Retained::AdmittedOnly,
        })
    }

    pub fn events(&self) -> &[RetainedEvent] {
        &self.events
    }

    /// The cursor the next host's journal continues from.
    pub fn last_cursor(&self) -> u64 {
        self.last_cursor
    }
}

