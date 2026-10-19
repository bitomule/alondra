#![forbid(unsafe_code)]
//! Technical worker execution contract. Business decisions and dispatch authority stay in the application.
use alondra_foundation::{CommandId, ContinuationId, ExecutionStamp, InputId, TurnId, WorkId};
use std::collections::BTreeSet;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InvalidBinding;
fn text(value: &str) -> Result<(), InvalidBinding> {
    if value.trim().is_empty() || value.len() > 256 || value.contains('\0') {
        Err(InvalidBinding)
    } else {
        Ok(())
    }
}
fn number(value: u64) -> Result<(), InvalidBinding> {
    if value == 0 || value > alondra_foundation::MAX_REVISION {
        Err(InvalidBinding)
    } else {
        Ok(())
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Capability {
    OfferInput,
    Interrupt,
    BackgroundWork,
    DurableResume,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProviderBinding {
    id: String,
    driver: String,
    instance: String,
    configuration: String,
    configuration_revision: u64,
    capability_revision: u64,
    capabilities: BTreeSet<Capability>,
    credential: String,
    credential_generation: u64,
}
impl ProviderBinding {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        id: String,
        driver: String,
        instance: String,
        configuration: String,
        configuration_revision: u64,
        capability_revision: u64,
        capabilities: BTreeSet<Capability>,
        credential: String,
        credential_generation: u64,
    ) -> Result<Self, InvalidBinding> {
        for value in [&id, &driver, &instance, &configuration, &credential] {
            text(value)?;
        }
        for value in [
            configuration_revision,
            capability_revision,
            credential_generation,
        ] {
            number(value)?;
        }
        Ok(Self {
            id,
            driver,
            instance,
            configuration,
            configuration_revision,
            capability_revision,
            capabilities,
            credential,
            credential_generation,
        })
    }
    pub fn id(&self) -> &str {
        &self.id
    }
    pub fn driver(&self) -> &str {
        &self.driver
    }
    pub fn instance(&self) -> &str {
        &self.instance
    }
    pub fn configuration(&self) -> &str {
        &self.configuration
    }
    pub fn configuration_revision(&self) -> u64 {
        self.configuration_revision
    }
    pub fn capability_revision(&self) -> u64 {
        self.capability_revision
    }
    pub fn capabilities(&self) -> &BTreeSet<Capability> {
        &self.capabilities
    }
    pub fn credential(&self) -> &str {
        &self.credential
    }
    pub fn credential_generation(&self) -> u64 {
        self.credential_generation
    }
    pub fn permits(&self, capability: Capability) -> bool {
        self.capabilities.contains(&capability)
    }
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Placement {
    LocalDirectory(String),
    RemoteWorkspace(String),
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Operation {
    Launch,
    Input,
}
/// Correlation data is not dispatch authority. The application privately owns the submitting adapter.
pub struct Invocation<'a> {
    pub binding: &'a ProviderBinding,
    pub execution: &'a ExecutionStamp,
    pub command: CommandId,
    pub input: &'a InputId,
    pub text: &'a str,
    pub operation: Operation,
    pub placement: Option<&'a Placement>,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Handoff {
    Offered,
    RefusedBeforeOffer,
    Uncertain,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Observation {
    Available,
    TurnStarted {
        input: InputId,
        turn: TurnId,
    },
    InputConsumed {
        input: InputId,
    },
    WorkStarted {
        turn: TurnId,
        work: WorkId,
    },
    WorkEnded {
        work: WorkId,
        continuation: ContinuationId,
    },
    /// The root turn ended; `failure` when a Claude API error ended it.
    TurnEnded {
        turn: TurnId,
        failure: Option<ApiFailure>,
    },
    /// The model answered the root turn: the API and the account Claude Code uses work.
    ModelAnswered,
    TransportLost,
    ResourceReleased,
}
/// The Claude API error a root turn ended with, as Claude Code reports it after its own retries.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ApiFailure {
    /// Claude Code's error category, such as `authentication_failed` or `rate_limit`.
    pub error: String,
    /// The HTTP status of the failed request, when one was answered.
    pub status: Option<u16>,
    /// When a rejected usage limit resets, in Unix seconds.
    pub resets_at: Option<u64>,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OrderedObservation {
    pub owner_generation: u64,
    pub execution: ExecutionStamp,
    pub cursor: u64,
    pub evidence: Vec<u8>,
}
#[derive(Debug)]
pub struct PreparedCommand {
    bytes: Vec<u8>,
}
impl PreparedCommand {
    pub fn new(bytes: Vec<u8>) -> Result<Self, AdapterError> {
        if bytes.is_empty() || bytes.len() > 1_048_576 {
            return Err(AdapterError::Protocol("command capacity".into()));
        }
        Ok(Self { bytes })
    }
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }
}
#[derive(Debug)]
pub enum AdapterError {
    Disconnected,
    InvalidObservation,
    Unsupported,
    Storage(String),
    Protocol(String),
}
/// Production implementations keep their endpoint and launch/ownership credentials private.
pub trait Adapter {
    type NativeInput;
    fn binding(&self) -> &ProviderBinding;
    fn prepare(
        &mut self,
        invocation: Invocation<'_>,
        native: Self::NativeInput,
    ) -> Result<PreparedCommand, AdapterError>;
    fn submit(
        &mut self,
        invocation: Invocation<'_>,
        command: &PreparedCommand,
    ) -> Result<Handoff, AdapterError>;
    fn next(&mut self) -> Result<Option<OrderedObservation>, AdapterError>;
    fn acknowledge(&mut self, execution: &ExecutionStamp, cursor: u64) -> Result<(), AdapterError>;
}
