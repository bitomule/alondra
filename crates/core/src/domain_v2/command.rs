use super::*;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CommandEnvelope {
    pub protocol_version: u32,
    pub data_epoch: DataEpoch,
    pub command_id: CommandId,
    pub expected_revision: Revision,
    pub command: Command,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Command {
    CreateRequest {
        request_id: RequestId,
        body: String,
    },
    CloseRequest {
        request_id: RequestId,
    },
    RegisterAgent {
        agent_id: AgentId,
        name: String,
    },
    ObserveAgent {
        agent_id: AgentId,
        connected: bool,
    },
    AdvanceOrchestratorGeneration {
        generation: u64,
    },
    CreateTask {
        task_id: TaskId,
        request_id: RequestId,
        title: String,
        agent_id: Option<AgentId>,
        review_prs: bool,
        dependencies: BTreeSet<TaskId>,
        stack: Option<StackDependency>,
    },
    AssignTask {
        task_id: TaskId,
        agent_id: AgentId,
    },
    SetDependencies {
        task_id: TaskId,
        dependencies: BTreeSet<TaskId>,
        stack: Option<StackDependency>,
    },
    StartTask {
        task_id: TaskId,
    },
    PublishInformation {
        #[serde(default)]
        attachments: Vec<AttachmentRef>,
        interaction_id: InteractionId,
        scope: Scope,
        recipient: Recipient,
        body: String,
    },
    AskQuestion {
        #[serde(default)]
        attachments: Vec<AttachmentRef>,
        interaction_id: InteractionId,
        scope: Scope,
        recipient: Recipient,
        body: String,
        blocking: bool,
        options: Vec<QuestionOption>,
    },
    RequestAction {
        #[serde(default)]
        attachments: Vec<AttachmentRef>,
        interaction_id: InteractionId,
        scope: Scope,
        recipient: Recipient,
        body: String,
        blocking: bool,
        options: Vec<QuestionOption>,
    },
    MarkRead {
        interaction_id: InteractionId,
    },
    AnswerQuestion {
        #[serde(default)]
        attachments: Vec<AttachmentRef>,
        interaction_id: InteractionId,
        option_id: String,
        inputs: BTreeMap<String, InputValue>,
    },
    CreateDocument {
        task_id: TaskId,
        document_id: DocumentId,
        title: String,
        revision_id: DocumentRevisionId,
        content_digest: String,
        content_reference: String,
        role: String,
    },
    ReviseDocument {
        task_id: TaskId,
        document_id: DocumentId,
        revision_id: DocumentRevisionId,
        content_digest: String,
        content_reference: String,
    },
    LinkTaskDocument {
        task_id: TaskId,
        document_id: DocumentId,
        role: String,
    },
    DecideDocumentReview {
        task_id: TaskId,
        document_id: DocumentId,
        revision_id: DocumentRevisionId,
        accepted: bool,
    },
    RegisterPr {
        task_id: TaskId,
        pr_id: PrId,
        host: String,
        repository: String,
        number: u64,
        head: String,
        base: String,
        allowed_methods: Vec<MergeMethod>,
    },
    LinkInteractionSubject {
        interaction_id: InteractionId,
        subject: InteractionSubject,
    },
    PresentResult {
        #[serde(default)]
        attachments: Vec<AttachmentRef>,
        task_id: TaskId,
        interaction_id: InteractionId,
        body: String,
        targets: Vec<DeliveryTarget>,
        is_final: bool,
    },
    SubmitFeedback {
        #[serde(default)]
        attachments: Vec<AttachmentRef>,
        presentation_id: InteractionId,
        targets: Vec<DeliveryTarget>,
        body: String,
    },
    AcceptDelivery {
        presentation_id: InteractionId,
        targets: Vec<DeliveryTarget>,
    },
    AuthorizeMerge {
        authorization_id: AuthorizationId,
        presentation_id: InteractionId,
        pr_id: PrId,
        expected_head: String,
        method: MergeMethod,
    },
    ClaimMergeAuthorization {
        authorization_id: AuthorizationId,
        remote_head: String,
        remote_base: String,
        base_verified: bool,
    },
    ObserveMergeAttempt {
        authorization_id: AuthorizationId,
        state: MergeAttemptState,
    },
    ObservePrState {
        pr_id: PrId,
        head: String,
        base: String,
        state: PrState,
        merged_head: Option<String>,
    },
    PublishStackReadiness {
        task_id: TaskId,
        presentation_id: InteractionId,
        pr_id: PrId,
        head: String,
        ready: bool,
    },
    CompleteTask {
        task_id: TaskId,
        presentation_id: InteractionId,
    },
    CancelTask {
        task_id: TaskId,
    },
    ReopenTask {
        task_id: TaskId,
    },
    ArchiveTask {
        task_id: TaskId,
        archived: bool,
    },
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum DomainEffect {
    NotifyUser {
        task_id: Option<TaskId>,
        interaction_id: InteractionId,
    },
    NotifyAgent {
        agent_id: AgentId,
        task_id: Option<TaskId>,
        interaction_id: Option<InteractionId>,
        message: String,
    },
    NotifyOrchestrator {
        interaction_id: InteractionId,
    },
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommandOutcome {
    pub revision: Revision,
    pub changed: bool,
    pub effects: Vec<DomainEffect>,
    pub created_ids: Vec<String>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DomainError {
    pub code: String,
    pub message: String,
    pub current_revision: Option<Revision>,
    pub subject: Option<String>,
}
impl DomainError {
    pub fn new(code: &str, message: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
            current_revision: None,
            subject: None,
        }
    }
}
impl std::fmt::Display for DomainError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.code, self.message)
    }
}
impl std::error::Error for DomainError {}
