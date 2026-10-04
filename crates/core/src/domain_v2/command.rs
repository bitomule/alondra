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
    /// Trusted adapter materializes a validated draft and computes `next_run`
    /// with the Mac's local calendar before submitting this command.
    CreateSchedule {
        schedule: Schedule,
    },
    /// The adapter recomputes `next_run`; clients cannot submit this command.
    UpdateSchedule {
        schedule: Schedule,
    },
    /// With `expected_schedule_revision`, staleness is judged against that
    /// Schedule alone (§5), so unrelated commits cannot refuse it.
    DeleteSchedule {
        schedule_id: ScheduleId,
        deleted_at: i64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        expected_schedule_revision: Option<i64>,
    },
    ClaimScheduleRun {
        schedule_id: ScheduleId,
        run_id: ScheduleRunId,
        origin: ScheduleRunOrigin,
        now: i64,
        next_after_now: Option<i64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        expected_schedule_revision: Option<i64>,
    },
    RecordScheduleRunSent {
        run_id: ScheduleRunId,
        request: Request,
        task: Task,
        launch: ScheduleLaunchIntent,
        finished_at: i64,
    },
    ClaimScheduleLaunch {
        run_id: ScheduleRunId,
        now: i64,
    },
    /// `agent_id` is the launched Claude session the daemon registered before
    /// spawning it; the scheduled Task is assigned to it in this same commit.
    RecordScheduleLaunchHanded {
        run_id: ScheduleRunId,
        session_id: Option<String>,
        #[serde(default)]
        agent_id: Option<AgentId>,
        finished_at: i64,
    },
    RecordScheduleRunFailed {
        run_id: ScheduleRunId,
        reason: String,
        finished_at: i64,
    },
    InterruptScheduleRunsOnRestart,
    CreateRequest {
        request_id: RequestId,
        body: String,
        #[serde(default)]
        auto_merge: bool,
    },
    CloseRequest {
        request_id: RequestId,
        #[serde(default)]
        answer: Option<String>,
        #[serde(default)]
        withdrawn: bool,
        #[serde(default)]
        title: Option<String>,
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
    RetitleTask {
        task_id: TaskId,
        title: String,
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
        card: CardSpec,
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
        card: CardSpec,
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
        card: CardSpec,
    },
    /// Alondra turns a Claude Code permission prompt or plan approval of an assigned Agent
    /// into a blocking question for the User; the hook waits for the answer.
    HoldHook {
        interaction_id: InteractionId,
        task_id: TaskId,
        agent_id: AgentId,
        hold: Hold,
        body: String,
        options: Vec<QuestionOption>,
        card: CardSpec,
    },
    /// Alondra or the orchestrator puts a failure in front of the user with its ways out.
    RaiseProblem {
        #[serde(default)]
        attachments: Vec<AttachmentRef>,
        interaction_id: InteractionId,
        scope: Scope,
        body: String,
        options: Vec<QuestionOption>,
        card: CardSpec,
    },
    MarkRead {
        interaction_id: InteractionId,
    },
    /// Any open card of the caller's own stops waiting.
    #[serde(alias = "withdraw_question")]
    Withdraw {
        interaction_id: InteractionId,
    },
    /// The user asks a card's author to say it again; its next card must replace this one.
    RequestClarification {
        interaction_id: InteractionId,
        reason: ClarificationReason,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        text: Option<String>,
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
        card: CardSpec,
    },
    SubmitFeedback {
        #[serde(default)]
        attachments: Vec<AttachmentRef>,
        presentation_id: InteractionId,
        targets: Vec<DeliveryTarget>,
        body: String,
        /// Alondra on its own sends only `conflicts`.
        #[serde(default)]
        instruction: Instruction,
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
/// A wire command written before cards existed gets the card its own fields describe; the
/// optional `need` and `why` fields override what is read from its body.
pub fn derive_wire_card(command: &mut serde_json::Map<String, serde_json::Value>) {
    use serde_json::Value;
    let text = |command: &mut serde_json::Map<String, Value>, key: &str| {
        command.remove(key).and_then(|v| v.as_str().map(str::to_owned)).filter(|v| !v.trim().is_empty())
    };
    let need = text(command, "need");
    let why = text(command, "why");
    if command.contains_key("card") {
        return;
    }
    let body = command.get("body").and_then(Value::as_str).unwrap_or_default().to_owned();
    let field = |key: &str| command.get(key).cloned().unwrap_or(Value::Array(vec![]));
    let mut card = match command.get("kind").and_then(Value::as_str) {
        Some("ask_question") => {
            let options: Vec<QuestionOption> = serde_json::from_value(field("options")).unwrap_or_default();
            CardSpec::question(&body, &options)
        }
        Some("request_action") => CardSpec::derived(CardKind::Do, &body),
        Some("publish_information") => CardSpec::derived(CardKind::Know, &body),
        Some("present_result") => {
            let targets: Vec<DeliveryTarget> = serde_json::from_value(field("targets")).unwrap_or_default();
            let is_final = command.get("is_final").and_then(Value::as_bool).unwrap_or(false);
            CardSpec::delivery(&body, &targets, is_final)
        }
        _ => return,
    };
    if let Some(need) = need {
        card.need = need;
    }
    if let Some(why) = why {
        card.why = why;
    }
    if let Ok(card) = serde_json::to_value(card) {
        command.insert("card".into(), card);
    }
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
    /// Something for the orchestrator's next batch: an interaction addressed to it, or a Request
    /// the user created.
    NotifyOrchestrator {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        interaction_id: Option<InteractionId>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        request_id: Option<RequestId>,
    },
    /// A scheduled Task whose launch failed: it stays not started with no node and nothing
    /// retries it.
    ScheduleLaunchFailed {
        task_id: TaskId,
        title: String,
        reason: String,
    },
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommandOutcome {
    pub revision: Revision,
    pub changed: bool,
    pub effects: Vec<DomainEffect>,
    pub created_ids: Vec<String>,
    /// Valid acceptances this command made obsolete without the user revoking
    /// them (§2.1: a new final, revision, dependency or head supersedes one).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub superseded_acceptances: Vec<SupersededAcceptance>,
}
/// A user acceptance that no longer applies because something other than
/// that user's own feedback or rejection changed the delivery.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SupersededAcceptance {
    pub task_id: TaskId,
    pub presentation_id: InteractionId,
    pub user_id: String,
    pub accepted_at: Revision,
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
