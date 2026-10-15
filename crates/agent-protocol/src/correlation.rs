use crate::codec::event_digest;
use crate::{
    Command, Error, Event, Execution, Fence, MAX_TRACKED_COMMANDS, Receipt, ReceiptOutcome,
    command_digest,
};
use alondra_foundation::CommandId;
use std::collections::HashMap;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CommandStatus {
    New,
    Duplicate,
}

struct TrackedCommand {
    digest: String,
    receipt: Option<ReceiptOutcome>,
}

/// Bounded outgoing command correlation. Commands must be sent with encode(), then
/// retried using identical bytes. This is not a durable command journal or a peer
/// command admission service; a receiver hashes the original read_with_digest bytes.
pub struct CommandRegistry {
    fence: Fence,
    execution: Execution,
    commands: HashMap<CommandId, TrackedCommand>,
}
impl CommandRegistry {
    pub fn new(fence: Fence, execution: Execution) -> Result<Self, Error> {
        fence.validate()?;
        execution.validate()?;
        Ok(Self {
            fence,
            execution,
            commands: HashMap::new(),
        })
    }
    pub fn record(&mut self, command: &Command) -> Result<CommandStatus, Error> {
        command.validate()?;
        matches_binding(
            &self.fence,
            &self.execution,
            &command.fence,
            &command.execution,
        )?;
        let digest = command_digest(command)?;
        if let Some(previous) = self.commands.get(&command.command_id) {
            return if previous.digest == digest {
                Ok(CommandStatus::Duplicate)
            } else {
                Err(Error::CommandConflict)
            };
        }
        if self.commands.len() == MAX_TRACKED_COMMANDS {
            return Err(Error::CommandCapacity);
        }
        self.commands.insert(
            command.command_id.clone(),
            TrackedCommand {
                digest,
                receipt: None,
            },
        );
        Ok(CommandStatus::New)
    }
    pub fn receipt(&mut self, receipt: &Receipt) -> Result<CommandStatus, Error> {
        receipt.validate()?;
        matches_binding(
            &self.fence,
            &self.execution,
            &receipt.fence,
            &receipt.execution,
        )?;
        let previous = self
            .commands
            .get_mut(&receipt.command_id)
            .ok_or(Error::UnknownCommand)?;
        if previous.digest != receipt.command_sha256 {
            return Err(Error::CommandConflict);
        }
        if let Some(outcome) = &previous.receipt {
            if outcome == &receipt.outcome {
                return Ok(CommandStatus::Duplicate);
            }
            match (outcome, &receipt.outcome) {
                (
                    ReceiptOutcome::Accepted { phase: earlier },
                    ReceiptOutcome::Accepted { phase: later },
                ) if later > earlier => {}
                (
                    ReceiptOutcome::Accepted {
                        phase: crate::ReceiptPhase::Admitted,
                    },
                    ReceiptOutcome::Rejected { .. },
                ) => {}
                _ => return Err(Error::ReceiptConflict),
            }
        }
        previous.receipt = Some(receipt.outcome.clone());
        Ok(CommandStatus::New)
    }
    pub fn len(&self) -> usize {
        self.commands.len()
    }
    pub fn is_empty(&self) -> bool {
        self.commands.is_empty()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SequenceStatus {
    Next,
    Duplicate,
}

/// Cursor validation does not apply a domain event. Commit only after evidence is
/// persisted. Only the most recent duplicate is retained in this minimum slice;
/// older replay requires the durable journal introduced by the ownership phase.
pub struct EventSequence {
    fence: Fence,
    execution: Execution,
    cursor: u64,
    last_digest: Option<String>,
}
impl EventSequence {
    pub fn new(fence: Fence, execution: Execution) -> Result<Self, Error> {
        fence.validate()?;
        execution.validate()?;
        Ok(Self {
            fence,
            execution,
            cursor: 0,
            last_digest: None,
        })
    }
    pub fn cursor(&self) -> u64 {
        self.cursor
    }
    pub fn check(&self, event: &Event) -> Result<SequenceStatus, Error> {
        event.validate()?;
        matches_binding(&self.fence, &self.execution, &event.fence, &event.execution)?;
        let digest = event_digest(event)?;
        if event.cursor == self.cursor {
            return if self.last_digest.as_ref() == Some(&digest) {
                Ok(SequenceStatus::Duplicate)
            } else {
                Err(Error::CursorConflict)
            };
        }
        if event.cursor < self.cursor {
            return Err(Error::StaleCursor);
        }
        if event.cursor != self.cursor + 1 {
            return Err(Error::CursorGap);
        }
        Ok(SequenceStatus::Next)
    }
    pub fn commit(&mut self, event: &Event) -> Result<SequenceStatus, Error> {
        let status = self.check(event)?;
        if status == SequenceStatus::Next {
            self.last_digest = Some(event_digest(event)?);
            self.cursor = event.cursor;
        }
        Ok(status)
    }
}

fn matches_binding(
    expected_fence: &Fence,
    expected_execution: &Execution,
    fence: &Fence,
    execution: &Execution,
) -> Result<(), Error> {
    if fence != expected_fence {
        return Err(Error::FenceMismatch);
    }
    if execution != expected_execution {
        return Err(Error::ExecutionMismatch);
    }
    Ok(())
}
