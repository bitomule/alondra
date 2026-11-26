use super::*;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
/// What came of a Request the orchestrator closes, one of a closed set the domain checks.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CloseOutcome {
    /// It needed no Task: `answer` is what the user asked for.
    Answered,
    /// Tasks that exist already did or do its work: `covered_by` names each.
    Covered,
    /// The open Task `folded_into` names carries it.
    Folded,
    /// It no longer applies: `withdrawn`.
    Withdrawn,
    /// Every Task of its own ended: `answer` is what came of them.
    TasksEnded,
}

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
    /// `now` and `until` are Unix milliseconds supplied by the trusted adapter.
    SnoozeCard { interaction_id: InteractionId, until: i64, now: i64 },
    UnsnoozeCard { interaction_id: InteractionId },
    ResurfaceDueCards { now: i64 },
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
    /// Every task the store keeps waiting whose waits are over goes on, saved and with its node
    /// told: the daemon runs it when it opens its domain, since opening it tells no node.
    ReviewWaits,
    /// Every open request of the user tells them, on a card of its own, the tasks no card of it
    /// named yet: the daemon runs it when a turn of the orchestrator ends.
    AnnounceRequestTasks,
    /// The tasks `AnnounceRequestTasks` would tell count as told, with no card shown: the daemon
    /// runs it when it opens its domain, so tasks made before an upgrade or a crash are not
    /// announced as new.
    AcknowledgeRequestTasks,
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
    /// A user's message to the orchestrator with its card context captured in the same commit. A
    /// reply to information is also its reading: the card leaves the inbox with it.
    CreateCardReply {
        request_id: RequestId,
        interaction_id: InteractionId,
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
        /// The answer's headline the user reads on its card, the answer below it.
        #[serde(default)]
        headline: Option<String>,
        /// The open Task that already covers this Request: it gets no Task of its own.
        #[serde(default)]
        folded_into: Option<TaskId>,
        /// The Tasks, of any Request and even over or archived, whose work already answers this
        /// Request: each must exist. Only an outcome `covered` names them, and it must.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        covered_by: Vec<TaskId>,
        /// What came of the Request. The orchestrator always says it, and the domain checks it
        /// against the Request and the other fields; the user and Alondra may leave it out.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        outcome: Option<CloseOutcome>,
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
    },
    /// What a node said last as a turn of it ended.
    RecordLastWords {
        agent_id: AgentId,
        text: String,
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
        #[serde(default, skip_serializing_if = "Option::is_none")]
        plan_items: Option<PlanItems>,
        /// The task of the same request this one carries on, taking its worktrees and its plan.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        continues: Option<TaskId>,
        /// Where its node stands in the queue from the start, so a free slot never launches it
        /// before its priority arrives.
        #[serde(default)]
        priority: TaskPriority,
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
    /// The assigned node waits for these tasks to end, without asking for a card.
    WaitForTasks {
        task_id: TaskId,
        tasks: BTreeSet<TaskId>,
    },
    SetDependencies {
        task_id: TaskId,
        dependencies: BTreeSet<TaskId>,
        stack: Option<StackDependency>,
        /// Why the dependencies change, when the orchestrator re-plans a task that waited for a
        /// cancelled one: the task shows it and its node reads it first.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reason: Option<String>,
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
    /// The user paused a task that waits in the queue to start: nothing starts it until they resume
    /// it, and it keeps its place. Ending the task ends the pause.
    PauseQueuedTask {
        task_id: TaskId,
        at: Millis,
    },
    ResumeQueuedTask {
        task_id: TaskId,
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
        /// The language the author declares every question is written in.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        language: Option<alondra_foundation::Lang>,
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
        #[serde(default)]
        inputs: BTreeMap<String, InputValue>,
    },
    /// The orchestrator's answer to a node's suggestion: one of the three decisions, never words alone.
    SettleSuggestion {
        interaction_id: InteractionId,
        decision: SuggestionDecision,
    },
    /// The orchestrator's settlement of a node that stopped with nothing explaining its next step:
    /// one closed outcome, never words alone. Alondra settles it itself, by closing, when the
    /// orchestrator leaves it unsettled.
    SettleStall {
        case_id: InteractionId,
        task_id: TaskId,
        outcome: StallOutcome,
    },
    /// The orchestrator's review of one dependent's plan after the user's decision held it: it
    /// stays as it is, gets a new plan, or is cancelled. The user can only keep the plans.
    SettlePlanReview {
        review_id: InteractionId,
        task_id: TaskId,
        outcome: PlanReviewOutcome,
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
    /// GitHub has this PR open for the branch of one of the task's worktrees: Alondra keeps it
    /// until GitHub says what it is. A command's text or output never notes a PR.
    NotePrOpened {
        task_id: TaskId,
        host: String,
        repository: String,
        number: u64,
    },
    /// A successful close command makes the task's registered PRs await a fresh GitHub read.
    NotePrChanges {
        task_id: TaskId,
    },
    /// What GitHub reports of a PR noted for the task: the PR is now the task's.
    RecordOpenedPr {
        task_id: TaskId,
        host: String,
        repository: String,
        number: u64,
        head: String,
        base: String,
        state: PrState,
        merged_head: Option<String>,
        allowed_methods: Vec<MergeMethod>,
    },
    /// Kept so the commands already recorded still read: nothing sends it, and it changes nothing,
    /// since running `gh pr create` no longer tells Alondra anything about a task's PRs.
    AskToRegisterPr {
        task_id: TaskId,
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
        /// Every point of the task's approved plan, marked done with its evidence; required on the
        /// final delivery of a task whose plan was approved.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        checklist: Vec<ChecklistMark>,
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
        /// Only Alondra says the feedback is a merge it could not make.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        merge_failure: Option<MergeFailure>,
    },
    AcceptDelivery {
        presentation_id: InteractionId,
        targets: Vec<DeliveryTarget>,
    },
    /// Accepts a final result that proposes a plan, saying whether its plan is implemented.
    DecideProposedPlan {
        presentation_id: InteractionId,
        targets: Vec<DeliveryTarget>,
        choice: PlanChoice,
    },
    AuthorizeMerge {
        authorization_id: AuthorizationId,
        presentation_id: InteractionId,
        pr_id: PrId,
        expected_head: String,
        method: MergeMethod,
        /// The user confirmed merging while a message of theirs still waits for its answer.
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        despite_pending_message: bool,
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
    /// What the provider says of merging a pull request at `head`.
    ObserveMergeVerdict {
        pr_id: PrId,
        head: String,
        verdict: crate::MergeVerdict,
    },
    ObservePrMergeMethods {
        pr_id: PrId,
        allowed_methods: Vec<MergeMethod>,
    },
    /// What the provider's adapter decided a read of a registered pull request says: which pull
    /// request it is and where it lives now.
    ObservePrAddress {
        pr_id: PrId,
        read: PrRead,
        read_started_at: Millis,
        verdict: PrAddressVerdict,
    },
    /// The user takes a pull request Alondra cannot check out of Alondra, without closing or
    /// merging it.
    DetachPr {
        pr_id: PrId,
        at: Millis,
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
        /// Why the task is cancelled, shown beside it and told to its node.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reason: Option<String>,
    },
    /// What GitHub reports of the open pull request of a task someone closed without delivering
    /// it: Alondra delivers it, records its merge, or lets the task be cancelled once it is closed.
    SettleOpenPr {
        task_id: TaskId,
        status: OpenPrStatus,
    },
    AdoptOpenPr {
        task_id: TaskId,
        pr_id: PrId,
        branch: String,
        status: OpenPrStatus,
    },
    /// A cancelled task's pull request is open on GitHub: Alondra closes it with the cancel's
    /// reason, as the cancel itself would have.
    ClosePrOfCancelledTask {
        task_id: TaskId,
        pr_id: PrId,
        branch: String,
        status: OpenPrStatus,
    },
    ReopenTask {
        task_id: TaskId,
    },
    /// The node says it deleted what it generated for its closing Task, and which of the task's
    /// worktrees still hold work it gives up: Alondra deletes those as they are.
    ConfirmCleanup {
        task_id: TaskId,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        discard: Vec<String>,
    },
    /// Someone tells the node of an SDK task `text` outside a card: the orchestrator, the user, or
    /// Alondra itself. It reaches the node's Claude inside the turn that runs, or as a turn of its own;
    /// told to a task no node runs, it waits in order for the node that starts it.
    TellNode {
        task_id: TaskId,
        text: String,
        /// The user wrote it: their word on the task moves it on.
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        from_user: bool,
        /// The node's own answered card whose answer `text` carries.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        answers: Option<InteractionId>,
    },
    /// Alondra saw the task's node, or a subagent it launched, make or take the git worktree at
    /// `path`.
    RecordTaskWorktree {
        task_id: TaskId,
        path: String,
        origin: WorktreeOrigin,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        branch: Option<String>,
    },
    /// A worktree of the task ended. An ended task's launch folder that was never recorded can
    /// only end `Removed`, which records it so nothing settles it again.
    ReleaseTaskWorktree {
        task_id: TaskId,
        path: String,
        end: WorktreeEnd,
    },
    /// The node says it is still working on something after its PR merged (a deploy, a release);
    /// Alondra does not close its session while it says so. No reason ends it. `waits_for` names
    /// what that work waits on: Alondra wakes the node when it lands.
    DeclarePostMergeWork {
        task_id: TaskId,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reason: Option<String>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        waits_for: Vec<WaitTarget>,
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
    /// The user asks for a crash report of what failed in the task. Nothing in the domain changes:
    /// it only judges that the task had such a failure; the report is built and sent outside it.
    ReportFailure {
        task_id: TaskId,
    },
    /// The user takes back what they allowed in the task from the permission card `card`: the
    /// next prompt it covered asks again.
    RevokeTaskGrant {
        task_id: TaskId,
        card: InteractionId,
    },
    /// The user clears the completed work off the lists: every ended Task the user saw at
    /// revision `seen`, and every closed Request that made none.
    ClearCompleted {
        seen: Revision,
    },
    /// The user picks how this Mac merges PRs from now on. Merges already authorized keep theirs.
    SetMergeMethod {
        method: MergeMethod,
    },
    /// The user picks how nodes bring their PR's base into their branch from now on.
    SetBranchUpdate {
        update: BranchUpdate,
    },
    /// The Agent working on the Task was closed, by the user or by Alondra: the Task goes back to
    /// not started with no Agent, and stays open for a new one, which `relaunch` names who starts.
    ReleaseTask {
        task_id: TaskId,
        reason: String,
        /// Who starts the task's next node.
        #[serde(default)]
        relaunch: Relaunch,
        /// The folder the closed node worked in, where its next node starts.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        cwd: Option<String>,
    },
    /// Alondra left nothing of a Task that ends without a node: not in the launch queue, no
    /// worktree. The receipt its ending waited for.
    RecordTaskCleanup {
        task_id: TaskId,
    },
    /// The SDK host took the Task with an execution of its own, running as `conversation`,
    /// `instance` and `host`, with `model` when the launch chose one. Never the `claude --bg`
    /// backend, whose node is assigned.
    AdmitExecution {
        #[serde(default, skip_serializing_if = "alondra_core::accounts::Provider::is_claude")]
        provider: alondra_core::accounts::Provider,
        task_id: TaskId,
        agent_id: AgentId,
        conversation: String,
        instance: String,
        host: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        model: Option<TaskModel>,
        /// The provider account the execution runs on; none for the provider's own sign-in.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        account: Option<alondra_core::accounts::AccountId>,
        /// The card of the usage limit the Task waits on between turns, which this execution, on
        /// another account, leaves behind: it resumes the same session there.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        leaves_limit: Option<InteractionId>,
    },
    /// What the SDK observed of the Task's execution of `generation`, in order, normalized from
    /// one accepted observation.
    RecordRuntimeFacts {
        task_id: TaskId,
        generation: u64,
        facts: Vec<RuntimeObservation>,
    },
    /// The root turn `turn` of the Task's SDK execution of `generation` ended. Evidence, never
    /// success: when nothing explains the Task's next step, its card asks the user how it goes on.
    RecordTurnBoundary {
        task_id: TaskId,
        generation: u64,
        turn: String,
        /// The Claude API error that ended the turn, past Claude Code's own retries.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        failure: Option<SdkApiFailure>,
        /// The code Claude Code ended the turn as failed with, such as `error_max_turns`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        failed: Option<String>,
    },
    /// The API error `failure` ended a turn of the Task's SDK execution of `generation`, and the
    /// lifecycle refused to record that end: the wait the error asks for (a sign-in, the API, the
    /// usage limit) stands all the same. An error no wait answers, and a Task already waiting,
    /// change nothing.
    RecordClaudeHold {
        task_id: TaskId,
        generation: u64,
        failure: SdkApiFailure,
    },
    /// The Task's SDK host of `generation` ended: no process holds its lock any more. What it had
    /// running ended with it. When a host that resumes the same Claude session takes its place
    /// (`resumes`), what was cut goes on by itself; otherwise, when nothing explains the Task's next
    /// step, its card asks the user. `ended_work` names, as Claude knows it, each background task the
    /// host left running or whose notification it had not yet read: the resumed session is told they
    /// ended and will not report back.
    RecordHostEnded {
        task_id: TaskId,
        generation: u64,
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        resumes: bool,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        ended_work: Vec<String>,
        /// Alondra ended the host itself (the daemon stopped it, or the host's owner failed): its
        /// end says nothing about whether the session can go on.
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        by_alondra: bool,
    },
    /// `input`, told `text`, was handed to the Task's SDK host of `generation`, which ended before
    /// it showed the input was read. When a host that resumes the same Claude session takes its
    /// place (`resumes`), the same text goes to it, flagged as possibly received already; otherwise
    /// a card asks the user to send it again or take it as received. `by_alondra` is kept for the
    /// commands already recorded: the doubt no longer depends on it.
    RecordDoubtfulHandoff {
        task_id: TaskId,
        generation: u64,
        input: String,
        text: String,
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        resumes: bool,
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        by_alondra: bool,
    },
    /// The Task's SDK execution could not be taken over and has no host to run on: `reason` says
    /// why. Its card offers to try again or to cancel the Task. `quiet`: the user was already told
    /// of this outage, so the card opens without a push.
    ReconcileRecoveryCards { task_id: TaskId },
    RecordHostLost {
        task_id: TaskId,
        reason: String,
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        quiet: bool,
        /// The provider refused the task's account before taking its input, for want of a sign-in
        /// or a key: the account is signed out and the card asks to sign in.
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        signed_out: bool,
    },
    /// Claude will never read `input`, told `text`, handed to the Task's SDK host of `generation`:
    /// it leaves what the Task waits on and a card asks the user whether to send it again.
    RecordDroppedInput {
        task_id: TaskId,
        generation: u64,
        input: String,
        text: String,
    },
    /// `input`, told `text`, never reached the Task's ended SDK host of `generation`: the same
    /// text goes to its next host in its place.
    ResendUntakenInput {
        task_id: TaskId,
        generation: u64,
        input: String,
        text: String,
    },
    /// The provider accounts as the daemon sees them now, in the user's order: each one's sign-in,
    /// email and plan, and how much of the session window each may use. It replaces what was
    /// recorded before.
    BeginAccountLogin { account: alondra_core::accounts::AccountKey, attempt: String, started_at: Millis },
    CancelAccountLogin { account: alondra_core::accounts::AccountKey, attempt: String },
    RecordAccountLogin { login: super::AccountLogin },
    RecordAccounts {
        accounts: Vec<alondra_core::accounts::AccountStatus>,
        #[serde(default = "alondra_core::usage::default_reserve")]
        reserve: alondra_core::usage::Share,
    },
    /// How new work is spread over the accounts, and whether a node moves to another account
    /// when its own reaches its limit, as `options.toml` says it now.
    RecordDistribution {
        policy: alondra_core::accounts::Policy,
        move_when_exhausted: bool,
    },
    /// The user changes the accounts from Settings: the daemon writes it to `options.toml`, then
    /// records it here.
    ChangeAccounts {
        provider: alondra_core::accounts::Provider,
        change: alondra_core::accounts::AccountChange,
    },
    SetAccountOrder {
        order: Vec<alondra_core::accounts::AccountKey>,
        expected_config_version: String,
    },
    RecordAccountConfiguration {
        order: Vec<alondra_core::accounts::AccountKey>,
        version: String,
    },
    /// The user set (`set`) or cleared the key of a named Antigravity account. The key itself went
    /// to the account's folder and is never part of a command.
    RecordAccountCredential {
        account: alondra_core::accounts::AccountKey,
        set: bool,
    },
    ReplaceAccountConfiguration {
        accounts: String,
        expected_config_version: String,
    },
    RecordModelCatalog {
        catalog: alondra_core::model_selection::AccountModelCatalog,
    },
    RecordTaskSelection {
        task_id: TaskId,
        selection: alondra_core::model_selection::TaskSelection,
        pending: Option<alondra_core::model_selection::PendingTaskSelection>,
    },
    RecordModelProfiles {
        profiles: alondra_core::model_selection::ModelProfiles,
        version: String,
    },
    SetModelProfile {
        provider: alondra_core::accounts::Provider,
        profile: alondra_core::model_selection::ModelProfile,
        policy: alondra_core::model_selection::RoleModelPolicy,
        expected_config_version: String,
    },
    RequestTaskSelection {
        task_id: TaskId,
        expected_selection: String,
        selection: alondra_core::model_selection::TaskSelection,
        pending: alondra_core::model_selection::PendingTaskSelection,
    },
    /// The task goes on on `account`: its execution ends and a new node there takes it over, with
    /// the same task, plan, worktree and branch. A task that never ran just starts there.
    MoveTask {
        task_id: TaskId,
        expected_selection: String,
        account: alondra_core::accounts::AccountKey,
    },
    /// The task's move to the account its pending selection names ends before any node started
    /// there: its execution is released, the selection it moves with stays, and nothing is pending.
    /// A pause the gate put on the node it leaves ends with it; the user's own pause stays.
    DeferTaskMove {
        task_id: TaskId,
        account: alondra_core::accounts::AccountKey,
    },
    /// The orchestrator's choice of stages for the task, recorded as its first node starts: with
    /// stages, the selection each role starts with, apart from the task's own.
    RecordTaskStages {
        task_id: TaskId,
        staged: bool,
        reason: String,
        #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
        selections: std::collections::BTreeMap<crate::ExecutionRole, alondra_core::model_selection::TaskSelection>,
    },
    /// Alondra starts, as `transition`, the change of role the staged task owes: its selection
    /// becomes the next role's, and `pending` names the account and model the new execution
    /// starts on.
    BeginStageHandover {
        task_id: TaskId,
        expected_selection: String,
        transition: String,
        pending: alondra_core::model_selection::PendingTaskSelection,
    },
    /// The author proposes the specs that go with its plan. Alondra fills `check` with what the
    /// engine reads of the task's worktree with them; whatever the agent sends there is replaced.
    ProposeSpecs {
        task_id: TaskId,
        level: crate::SpecLevel,
        #[serde(default)]
        files: Vec<crate::SpecFile>,
        #[serde(default)]
        check: crate::SpecCheck,
    },
    /// The user edits the proposed specs on the plan's card; `expected` is the proposal's revision.
    ReviseSpecs {
        task_id: TaskId,
        expected: Revision,
        level: crate::SpecLevel,
        #[serde(default)]
        files: Vec<crate::SpecFile>,
        #[serde(default)]
        check: crate::SpecCheck,
    },
    /// What Alondra last read of the settings of the task's worktree: why they cannot be read, or
    /// nothing when they can.
    RecordSpecSettings {
        task_id: TaskId,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        problem: Option<crate::SpecProblem>,
    },
    /// Alondra wrote these approved spec files into the task's worktree: path and fingerprint of
    /// what it wrote.
    RecordSpecsWritten {
        task_id: TaskId,
        written: std::collections::BTreeMap<String, String>,
    },
    /// The implementer asks for a change of the approved specs, and why.
    RequestSpecChange {
        task_id: TaskId,
        reason: String,
    },
    /// The author drafts the change of specs the implementer asked for; Alondra fills `check`.
    ProposeAmendment {
        task_id: TaskId,
        amendment_id: String,
        #[serde(default)]
        files: Vec<crate::SpecFile>,
        #[serde(default)]
        check: crate::SpecCheck,
    },
    /// The author proposes a lasting decision on a card of its own. Alondra fills `files` (the
    /// decision and those it supersedes) and `check`.
    ProposeDecision {
        task_id: TaskId,
        decision: crate::DecisionFields,
        #[serde(default)]
        files: crate::SpecFiles,
        #[serde(default)]
        check: crate::SpecCheck,
    },
    /// The change of role `transition` could not start the next execution: `refused` when the
    /// same start is refused however often it is tried, which waits for the user. No `transition`
    /// names a change that could not even start.
    StageHandoverFailed {
        task_id: TaskId,
        transition: Option<String>,
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        refused: bool,
    },
    /// A selection change could not finish: `selection` is the one the task had before it, nothing
    /// is pending, and a move says why on the task.
    AbortTaskSelection {
        task_id: TaskId,
        selection: Option<alondra_core::model_selection::TaskSelection>,
        refused: Option<alondra_core::model_selection::MoveRefused>,
    },
    /// What `account` said of its usage at `at`, on the Mac's local `day`, from a provider event
    /// or a probe.
    RecordAccountUsage {
        provider: alondra_core::accounts::Provider,
        account: alondra_core::accounts::AccountId,
        reading: alondra_core::usage::UsageReading,
        at: Millis,
        day: String,
    },
    /// The provider refused to run work on `account` because of its native account policy: it
    /// takes no new work until it changes (a new sign-in, enabling it again, another identity or billing).
    RecordAccountPolicyRefused {
        provider: alondra_core::accounts::Provider,
        account: alondra_core::accounts::AccountId,
    },
    /// The daemon's schedule: one task held by `hold` goes on, to probe whether Claude works again;
    /// for a limit, one of `account`'s when it names one.
    ProbeClaudeAccess {
        #[serde(default, skip_serializing_if = "alondra_core::accounts::Provider::is_claude")]
        provider: alondra_core::accounts::Provider,
        hold: ClaudeHold,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        account: Option<alondra_core::accounts::AccountId>,
    },
    /// Stops what the node's SDK task runs now: its root turn, or the work it runs on its own.
    InterruptTurn {
        agent_id: AgentId,
    },
    /// Ends Claude's process for the node's SDK task, whose root turn stayed open after Claude
    /// accepted its interrupt. The turn ends as interrupted when the process exits.
    ForceStopTurn {
        agent_id: AgentId,
    },
    /// Claude accepted the interrupt of the root `turn` of the Task's SDK execution of
    /// `generation`, and the turn is still open.
    /// The Task's resting SDK execution of `generation` closed its host, keeping its session.
    RecordHostReleased {
        task_id: TaskId,
        generation: u64,
    },
    /// A host that resumes the released session of `generation` takes over.
    RecordHostRetaken {
        task_id: TaskId,
        generation: u64,
    },
    RecordInterruptAccepted {
        task_id: TaskId,
        generation: u64,
        turn: String,
    },
    /// The model the Task's SDK execution answers with from its next turn on.
    SetModel {
        task_id: TaskId,
        model: String,
    },
    SetPermissionMode {
        task_id: TaskId,
        mode: PermissionModeRecord,
    },
    /// Asks the Task's SDK execution which MCP servers it has and how they are.
    ReadMcpStatus {
        task_id: TaskId,
    },
    /// Claude connects the Task's MCP server again, reading its stored sign-in anew.
    ReconnectMcp {
        task_id: TaskId,
        server: String,
    },
    /// Claude opens a sign-in to the Task's MCP server; the user finishes it in a browser.
    StartMcpSignIn {
        task_id: TaskId,
        server: String,
    },
    /// Claude forgets and revokes its stored sign-in to the Task's MCP server.
    ClearMcpSignIn {
        task_id: TaskId,
        server: String,
    },
    /// The MCP servers the Task's SDK execution of `generation` reported, and how they are.
    RecordMcpServers {
        task_id: TaskId,
        generation: u64,
        servers: Vec<McpServerState>,
    },
    /// How the sign-in to `server` the Task's SDK execution of `generation` opened goes on.
    RecordMcpSignIn {
        task_id: TaskId,
        generation: u64,
        server: String,
        sign_in: McpSignIn,
    },
    /// The browser's return from the open sign-in to `server`, under `state`, reached Claude's
    /// listener. The return itself is never recorded.
    CompleteMcpSignIn {
        task_id: TaskId,
        server: String,
        state: String,
    },
    /// The Task's node needs an MCP server its Claude reported and that is not connected: the
    /// Task waits for it on a card of its own until it connects or the user goes on without it.
    WaitForMcp {
        task_id: TaskId,
        server: String,
    },
    /// Alondra lets go of what nobody reads any more of the cards of done or cancelled Tasks.
    CompactEndedTasks {
        task_ids: Vec<TaskId>,
    },
    /// Claude asks, through the Task's SDK execution of `generation`, for the decision its host
    /// persisted at `epoch`. Alondra's own tools are allowed, what nodes may not do and a plan
    /// without its checklist are denied, all without a card; anything else is put to the user.
    RecordDecisionRequest {
        task_id: TaskId,
        generation: u64,
        epoch: u64,
        hold: Hold,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        tool: Option<String>,
        /// The tool's input as Claude asked to run it.
        #[serde(default)]
        input: serde_json::Value,
        /// The MCP server that serves the tool, as the SDK reports it.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        mcp_server: Option<McpServerOrigin>,
        prompt: DecisionPrompt,
    },
    /// Alondra could not put the request its host persisted at `epoch` in front of the user, for
    /// `reason`: Claude is denied it with that reason and goes on.
    RefuseDecisionRequest {
        task_id: TaskId,
        generation: u64,
        epoch: u64,
        reason: String,
    },
    /// The decision of the request at `epoch` reached Claude.
    RecordDecisionSettled {
        task_id: TaskId,
        generation: u64,
        epoch: u64,
    },
    /// The request at `epoch` ended without a decision reaching Claude: aborted, closed, or its
    /// decision could not be delivered.
    RecordDecisionAborted {
        task_id: TaskId,
        generation: u64,
        epoch: u64,
    },
}
/// The MCP server that serves a tool, as the Agent SDK reports it: `source` says where its
/// definition came from, and only it is trusted, never the name alone.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct McpServerOrigin {
    pub name: String,
    pub source: String,
}
/// What the user is asked for a decision Claude waits on.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum DecisionPrompt {
    /// The same native plan request already decided in this execution, observed at a new epoch.
    RecordedPlan { epoch: u64 },
    /// One card, as a held hook asks it.
    Card { body: String, options: Vec<QuestionOption>, card: CardSpec },
    /// Claude's questions asked together: a card each, decided together once the last is answered.
    Questions { questions: Vec<GroupQuestion> },
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
    /// The last open Task of an open Request ended: the orchestrator closes the Request or creates
    /// its next Tasks.
    RequestTasksOver {
        request_id: RequestId,
        task_id: TaskId,
    },
    /// The user decided on a Task that others wait for: the orchestrator reviews the plans of
    /// the dependents this decision holds.
    ReviewDependentPlans {
        review_id: InteractionId,
        task_id: TaskId,
    },
    /// A scheduled Task whose launch failed: it stays not started with no node and nothing
    /// retries it.
    ScheduleLaunchFailed {
        task_id: TaskId,
        title: String,
        reason: String,
    },
    /// The pull request's address no longer leads to it: the adapter finds it again by its
    /// locator and reports what it found with `ObservePrAddress`.
    Relocate {
        pr_id: PrId,
        locator: ProviderLocator,
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
    /// What the command changed, one line each, for the caller to read.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub effect: Vec<String>,
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
/// The id Alondra gives a PR: its host, repository and number, so two repositories with
/// the same PR number never share one.
pub fn hosted_pr_id(host: &str, repository: &str, number: u64) -> PrId {
    PrId(format!("{host}/{repository}#{number}"))
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
/// A PR repository as its owner/name, lowercase: GitHub does not tell `Parcel` from `parcel`,
/// so both spellings name one PR and get one id.
pub fn normalize_pr_repository(repository: &str) -> String {
    repository.trim().to_ascii_lowercase()
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
    /// The rule of a card's contract the card broke, and in which field.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub contract: Option<ContractBreach>,
}
impl DomainError {
    pub fn new(code: &str, message: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
            current_revision: None,
            subject: None,
            refusal: None,
            contract: None,
        }
    }
    /// The refusal of a card that broke its type's contract.
    pub fn of_breach(breach: ContractBreach) -> Self {
        let mut error = Self::new("card_contract", format!("{}: {}", breach.field, breach.fix()));
        error.subject = Some(breach.field.clone());
        error.contract = Some(breach);
        error
    }
    /// The same refusal, naming the rule of the card's contract it applied.
    pub fn breaking(mut self, card_type: Option<CardType>, rule: ContractRule) -> Self {
        if self.contract.is_none() {
            let field = self.subject.clone().unwrap_or_else(|| "card".into());
            self.contract = Some(ContractBreach { card_type, rule, field });
        }
        self
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
/// Work of another Task a node waits on: the Task itself, or the pull request it registered.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum WaitTarget {
    Task { task_id: TaskId },
    PullRequest { repository: String, number: u64 },
}
impl WaitTarget {
    /// What a node names: `owner/repo#12`, a pull request's URL, or else a task id.
    pub fn parse(text: &str) -> Self {
        let text = text.trim();
        let url = text.split_once("://").map_or(text, |(_, rest)| rest);
        let pull = url.split_once('/').and_then(|(_, path)| {
            let mut parts = path.trim_end_matches('/').splitn(4, '/');
            let (owner, repo, pull, number) = (parts.next()?, parts.next()?, parts.next()?, parts.next()?);
            (pull == "pull").then(|| (format!("{owner}/{repo}"), number.parse().ok())).and_then(|(r, n)| Some((r, n?)))
        });
        let short = text.rsplit_once('#').filter(|(repo, _)| repo.contains('/'))
            .and_then(|(repo, number)| Some((repo.to_owned(), number.parse().ok()?)));
        match pull.or(short) {
            Some((repository, number)) => Self::PullRequest { repository: normalize_pr_repository(&repository), number },
            None => Self::Task { task_id: text.into() },
        }
    }
}
