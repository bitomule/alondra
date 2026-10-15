use crate::{AgentId, TaskId};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum IdentityError {
    EmptyIdentity,
    InvalidGeneration,
}
impl std::fmt::Display for IdentityError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::EmptyIdentity => formatter.write_str("identity must not be empty"),
            Self::InvalidGeneration => {
                formatter.write_str("generation is outside the supported range")
            }
        }
    }
}
impl std::error::Error for IdentityError {}

fn validate_identity(value: &str) -> Result<(), IdentityError> {
    if value.trim().is_empty() {
        Err(IdentityError::EmptyIdentity)
    } else {
        Ok(())
    }
}

macro_rules! identities {
    ($($name:ident),+ $(,)?) => {$ (
        #[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
        pub struct $name(String);
        impl $name {
            pub fn new(value: impl Into<String>) -> Result<Self, IdentityError> {
                let value = value.into();
                validate_identity(&value)?;
                Ok(Self(value))
            }
            pub fn as_str(&self) -> &str { &self.0 }
        }
    )+};
}
identities!(
    ConversationId,
    InstanceId,
    HostId,
    InputId,
    TurnId,
    WorkId,
    ContinuationId,
    CallbackId,
    OperationId
);

/// A correlation value, never a permission to launch or dispatch an input.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExecutionStamp {
    task: TaskId,
    node: AgentId,
    conversation: ConversationId,
    instance: InstanceId,
    host: HostId,
    generation: u64,
}
impl ExecutionStamp {
    pub fn new(
        task: TaskId,
        node: AgentId,
        conversation: ConversationId,
        instance: InstanceId,
        host: HostId,
        generation: u64,
    ) -> Result<Self, IdentityError> {
        validate_identity(&task.0)?;
        validate_identity(&node.0)?;
        if generation == 0 || generation > crate::MAX_REVISION {
            return Err(IdentityError::InvalidGeneration);
        }
        Ok(Self {
            task,
            node,
            conversation,
            instance,
            host,
            generation,
        })
    }

    pub fn task(&self) -> &TaskId {
        &self.task
    }
    pub fn node(&self) -> &AgentId {
        &self.node
    }
    pub fn conversation(&self) -> &ConversationId {
        &self.conversation
    }
    pub fn instance(&self) -> &InstanceId {
        &self.instance
    }
    pub fn host(&self) -> &HostId {
        &self.host
    }
    pub fn generation(&self) -> u64 {
        self.generation
    }
}
