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
    /// A task cannot start by itself: Alondra puts why in front of the user with the ways out.
    RaiseStartProblem {
        task_id: TaskId,
        problem: StartProblem,
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
        #[serde(default)]
        maintenance: bool,
        #[serde(default)]
        message: bool,
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
    TitleRequest {
        request_id: RequestId,
        title: String,
    },
    RegisterAgent {
        agent_id: AgentId,
        name: String,
        #[serde(default)]
        at: Millis,
    },
    /// Something happened to a node: the node machine moves its phase or refuses the event.
    MoveNode {
        agent_id: AgentId,
        event: NodeEvent,
        at: Millis,
    },
    /// What the listing of real sessions says of each node Alondra launched, absent included.
    ReconcileNodes {
        listed: BTreeMap<AgentId, Listed>,
        at: Millis,
    },
    AdvanceOrchestratorGeneration {
        generation: u64,
    },
    BindOrchestratorSession {
        session_id: AgentId,
    },
    CreateTask {
        task_id: TaskId,
        request_id: RequestId,
        title: String,
        agent_id: Option<AgentId>,
        review_prs: bool,
        dependencies: BTreeSet<TaskId>,
        stack: Option<StackDependency>,
        /// Optional on the wire only so commands recorded before it read back; the rule refuses
        /// a new Task without it.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        launch: Option<LaunchPlan>,
    },
    AssignTask {
        task_id: TaskId,
        agent_id: AgentId,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        model: Option<TaskModel>,
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
    /// The relations between tasks beyond `SetDependencies`: starts after a final delivery, and
    /// holds back the review of other tasks.
    SetRelations {
        task_id: TaskId,
        #[serde(default)]
        delivered_dependencies: BTreeSet<TaskId>,
        #[serde(default)]
        blocks: BTreeSet<TaskId>,
    },
    /// Where the task's node stands in the queue of nodes waiting to work; the tasks that review it
    /// follow it.
    SetPriority {
        task_id: TaskId,
        priority: TaskPriority,
    },
    /// What the final delivery of a task that blocks others concluded about them.
    RecordVerdict {
        task_id: TaskId,
        presentation_id: InteractionId,
        verdict: Verdict,
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
    /// Several questions asked together: one Decide card per question, linked as a group and in
    /// order. Each is answered on its own; the author gets every answer at once, when the last is in.
    AskQuestions {
        interaction_id: InteractionId,
        scope: Scope,
        recipient: Recipient,
        blocking: bool,
        /// Why the questions are asked, for a question that gives no why of its own.
        why: String,
        questions: Vec<GroupQuestion>,
    },
    /// The same for a Claude Code AskUserQuestion with several questions, held for its answers.
    HoldQuestions {
        interaction_id: InteractionId,
        task_id: TaskId,
        agent_id: AgentId,
        questions: Vec<GroupQuestion>,
    },
    /// The user takes back an answer to a question of a group the author has not been given yet.
    ReopenAnswer {
        interaction_id: InteractionId,
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
        /// A final delivery that found nothing for the user to look at: the task closes in silence.
        #[serde(default)]
        nothing_to_review: bool,
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
    ObservePrMergeMethods {
        pr_id: PrId,
        allowed_methods: Vec<MergeMethod>,
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
    /// The node says it deleted what it generated for its closing Task.
    ConfirmCleanup {
        task_id: TaskId,
    },
    /// The node says it is still working on something after its PR merged (a deploy, a release);
    /// Alondra does not close its session while it says so. No reason ends it.
    DeclarePostMergeWork {
        task_id: TaskId,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reason: Option<String>,
    },
    /// Alondra records what became of a Task's node; a closing Task is done with it.
    RecordNodeEnd {
        task_id: TaskId,
        end: NodeEnd,
        /// The node the end is of; refused when the task has another by now.
        #[serde(default)]
        agent_id: Option<AgentId>,
    },
    ArchiveTask {
        task_id: TaskId,
        archived: bool,
    },
    /// The Agent working on the Task was closed by the user: the Task goes back to not started with
    /// no Agent, and stays open for a new one.
    ReleaseTask {
        task_id: TaskId,
        reason: String,
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
/// A PR host as a node tends to get it slightly wrong: a scheme, a path, a
/// trailing slash, capitals, or the bare name "github" for "github.com".
pub fn normalize_pr_host(host: &str) -> String {
    let host = host.trim().to_ascii_lowercase();
    let host = host.split_once("://").map_or(host.as_str(), |(_, rest)| rest);
    let host = host.split('/').next().unwrap_or_default();
    match host {
        "github" => "github.com".to_owned(),
        other => other.to_owned(),
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DomainError {
    pub code: String,
    pub message: String,
    pub current_revision: Option<Revision>,
    pub subject: Option<String>,
    /// Why, for a person: what the user's action ran into. Absent when only a node can get here.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub refusal: Option<Refusal>,
}
impl DomainError {
    pub fn new(code: &str, message: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
            current_revision: None,
            subject: None,
            refusal: None,
        }
    }
    pub fn because(mut self, refusal: Refusal) -> Self {
        self.refusal = Some(refusal);
        self
    }
    /// What to tell the person whose action this stopped.
    pub fn why(&self) -> Refusal {
        self.refusal.clone().unwrap_or_else(|| Refusal::for_code(&self.code))
    }
}
impl std::fmt::Display for DomainError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.code, self.message)
    }
}
impl std::error::Error for DomainError {}
