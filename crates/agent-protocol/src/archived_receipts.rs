//! Definitive receipts from completed host archives, without attaching to a host or sending.
use crate::{Body, Error, Execution, Fence, FramedReader, ReceiptOutcome, ReceiptPhase, ReceivedFrame};
use std::collections::{BTreeMap, BTreeSet};
use std::io::{BufReader, Cursor, Read};
use std::path::Path;

pub struct ArchivedReceipt {
    pub command: Vec<u8>,
    pub receipt: Vec<u8>,
    pub outcome: ReceiptOutcome,
}

/// `validate` checks the archive's bootstrap against the provider's stored native session and
/// configuration and returns its host instance. No absent or admitted-only command is evidence.
pub fn archived_receipts(
    state_dir: &Path, fence: &Fence, execution: &Execution, commands: &[&[u8]],
    validate: impl Fn(&[u8]) -> Result<String, Error>,
) -> Result<Vec<ArchivedReceipt>, Error> {
    if commands.is_empty() { return Ok(Vec::new()); }
    let mut expected = BTreeMap::new();
    for &bytes in commands {
        let mut reader = FramedReader::new(Cursor::new(bytes));
        let frame = reader.read_received()?.ok_or(Error::Invalid("missing archived command"))?;
        let Body::Command(command) = &frame.frame().body else { return Err(Error::Invalid("archived command type")); };
        if &command.fence != fence || &command.execution != execution || reader.read_received()?.is_some() {
            return Err(Error::Invalid("archived command scope"));
        }
        if expected.insert(command.command_id.0.clone(), (bytes, frame)).is_some() {
            return Err(Error::Invalid("duplicate archived command"));
        }
    }
    let parent = state_dir.parent().ok_or(Error::Invalid("archive parent"))?;
    let prefix = format!("{}.ended-", state_dir.file_name().and_then(|name| name.to_str()).ok_or(Error::Invalid("archive name"))?);
    let entries = match std::fs::read_dir(parent) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(Error::Io(error)),
    };
    let mut paths = Vec::new();
    for entry in entries {
        let entry = entry.map_err(Error::Io)?;
        if entry.file_name().to_str().is_some_and(|name| name.starts_with(&prefix) && name.len() > prefix.len())
            && entry.file_type().map_err(Error::Io)?.is_dir()
        { paths.push(entry.path()); }
    }
    paths.sort();
    let mut proofs = Vec::new();
    for path in paths {
        if path.join("uncertain").try_exists().map_err(Error::Io)? { continue; }
        let Some(bootstrap) = regular_file(&path.join("host.lock"))? else { continue };
        let mut bytes = Vec::new();
        bootstrap.take(65_537).read_to_end(&mut bytes).map_err(Error::Io)?;
        if bytes.len() > 65_536 { return Err(Error::Invalid("archive bootstrap capacity")); }
        let instance = validate(&bytes)?;
        let Some(outbox) = regular_file(&path.join("outbox"))? else { continue };
        proofs.extend(read_archive(BufReader::new(outbox), fence, execution, &instance, &expected)?);
    }
    Ok(proofs)
}

fn regular_file(path: &Path) -> Result<Option<std::fs::File>, Error> {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) if metadata.is_file() => std::fs::File::open(path).map(Some).map_err(Error::Io),
        Ok(_) => Err(Error::Invalid("archive file type")),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(Error::Io(error)),
    }
}

fn read_archive(
    source: impl Read, fence: &Fence, execution: &Execution, instance: &str,
    expected: &BTreeMap<String, (&[u8], ReceivedFrame)>,
) -> Result<Vec<ArchivedReceipt>, Error> {
    let mut reader = FramedReader::new(source);
    let mut seen = BTreeSet::new();
    let mut outcomes = BTreeSet::new();
    let mut proofs = Vec::new();
    loop {
        let frame = match reader.read_received() {
            Ok(Some(frame)) => frame,
            Ok(None) => return Ok(proofs),
            Err(Error::TruncatedHeader | Error::TruncatedPayload) => return Ok(Vec::new()),
            Err(error) => return Err(error),
        };
        let (identity, host) = match &frame.frame().body {
            Body::Command(value) => ((&value.fence, &value.execution), None),
            Body::Receipt(value) => ((&value.fence, &value.execution), None),
            Body::Event(value) => ((&value.fence, &value.execution), None),
            Body::Attached(value) => ((&value.fence, &value.execution), Some(value.host_instance_id.as_str())),
            Body::Acknowledge(value) => ((&value.fence, &value.execution), Some(value.host_instance_id.as_str())),
            _ => return Err(Error::Invalid("archive frame type")),
        };
        if identity != (fence, execution) || host.is_some_and(|host| host != instance) {
            return Err(Error::Invalid("archive frame scope"));
        }
        match &frame.frame().body {
            Body::Command(command) => {
                if let Some((_, expected)) = expected.get(&command.command_id.0) {
                    if expected.payload() != frame.payload() { return Err(Error::CommandConflict); }
                    seen.insert(command.command_id.0.clone());
                }
            }
            Body::Receipt(receipt) => {
                let Some((bytes, command)) = expected.get(&receipt.command_id.0) else { continue };
                if !seen.contains(&receipt.command_id.0) || receipt.command_sha256 != command.digest() {
                    return Err(Error::CommandConflict);
                }
                let offered = match receipt.outcome {
                    ReceiptOutcome::Accepted { phase: ReceiptPhase::Offered | ReceiptPhase::Applied } => true,
                    ReceiptOutcome::Rejected { .. } => false,
                    _ => continue,
                };
                if outcomes.insert((receipt.command_id.0.clone(), offered)) {
                    proofs.push(ArchivedReceipt { command: bytes.to_vec(), receipt: frame.payload().to_vec(), outcome: receipt.outcome.clone() });
                }
            }
            _ => {}
        }
    }
}
