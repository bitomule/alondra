use super::ScheduleState;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

macro_rules! ids { ($($name:ident),*) => {$ (
    #[derive(Clone,Debug,Default,PartialEq,Eq,PartialOrd,Ord,Hash,Serialize,Deserialize)]
    #[serde(transparent)] pub struct $name(pub String);
    impl From<&str> for $name { fn from(v:&str)->Self {Self(v.into())} }
    impl From<String> for $name { fn from(v:String)->Self {Self(v)} }
    impl fmt::Display for $name {fn fmt(&self,f:&mut fmt::Formatter<'_>)->fmt::Result {self.0.fmt(f)}}
)*}; }
ids!(
    TaskId,
    AgentId,
    InteractionId,
    DocumentId,
    DocumentRevisionId,
    PrId,
    RequestId,
    AuthorizationId,
    CommandId,
    DataEpoch,
    ScheduleId,
    ScheduleRunId
);
pub type Revision = u64;
pub const MAX_REVISION: Revision = 9_007_199_254_740_991;
pub const PROTOCOL_VERSION: u32 = 2;

/// Never deserialize an Actor from a client payload. Only authenticated adapters construct it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Actor {
    User { user_id: String },
    Agent { agent_id: AgentId },
    Orchestrator { generation: u64 },
    System,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Author {
    Agent { agent_id: AgentId },
    Orchestrator { generation: u64 },
    System,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Recipient {
    User,
    Agent { agent_id: AgentId },
    Orchestrator,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Scope {
    Task { task_id: TaskId },
    Orchestrator { request_id: Option<RequestId> },
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Request {
    pub id: RequestId,
    pub body: String,
    pub closed: bool,
    /// A short name the user reads in place of the whole text.
    #[serde(default)]
    pub title: Option<String>,
    /// What the orchestrator told the user when it closed a Request that needed no Task.
    #[serde(default)]
    pub answer: Option<String>,
    /// Closed because the user withdrew it.
    #[serde(default)]
    pub withdrawn: bool,
    /// The user chose that Alondra merges the PR of the final delivery once its checks pass,
    /// without waiting for their acceptance.
    #[serde(default)]
    pub auto_merge: bool,
}
impl Request {
    pub fn new(id: RequestId, body: String) -> Self {
        Request { id, body, closed: false, title: None, answer: None, withdrawn: false, auto_merge: false }
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Agent {
    pub id: AgentId,
    pub name: String,
    pub connected: bool,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskState {
    NotStarted,
    Running,
    NeedsYou,
    /// Delivered, but an open task blocks it: nothing for the user to do yet.
    AwaitingBlocker,
    /// Delivered and the user can act on it now.
    #[serde(alias = "in_review")]
    ForYourReview,
    WaitingDependency,
    Closing,
    Done,
    Cancelled,
}
impl TaskState {
    pub fn terminal(self) -> bool {
        matches!(self, Self::Done | Self::Cancelled)
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Task {
    pub id: TaskId,
    pub request_id: RequestId,
    pub agent_id: Option<AgentId>,
    pub title: String,
    pub state: TaskState,
    pub revision: Revision,
    pub started: bool,
    pub review_prs: bool,
    pub final_presentation: Option<InteractionId>,
    pub pr_id: Option<PrId>,
    pub dependencies: BTreeSet<TaskId>,
    /// Tasks this one starts after, as soon as they make their final delivery.
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    pub delivered_dependencies: BTreeSet<TaskId>,
    /// Tasks that cannot reach the user's review, or have a card, while this one is open.
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    pub blocks: BTreeSet<TaskId>,
    /// What this task's final delivery said about the tasks it blocks.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verdict: Option<Verdict>,
    /// How many times its verdict sent the blocked tasks back to work.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub review_rounds: u8,
    pub stack: Option<StackDependency>,
    pub documents: BTreeMap<DocumentId, String>,
    pub archived: bool,
    /// The model its node was last launched with; absent for Tasks launched before it was recorded
    /// and for launches that left Claude's default model.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<TaskModel>,
    /// How the task's end reaches its node: the cleanup request, the node's confirmation and what
    /// became of the node.
    #[serde(default, skip_serializing_if = "WrapUp::is_default")]
    pub wrap_up: WrapUp,
}
fn is_zero(n: &u8) -> bool {
    *n == 0
}
/// The most rounds a blocking task sends the work back before its last word closes it.
pub const MAX_REVIEW_ROUNDS: u8 = 3;
/// What a task that blocks others concluded about the tasks it blocks.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Verdict {
    /// `open_issues` is set when the rounds ran out with failures still open: `summary` then
    /// holds them.
    Approved {
        summary: String,
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        open_issues: bool,
        /// Serious failures found in this round: broken behaviour, lost data, or a security or
        /// session hole. Verdicts recorded before the count read as 0.
        #[serde(default, skip_serializing_if = "is_zero_u32")]
        serious_findings: u32,
    },
    /// `serious_findings` is required from the reviewer; only verdicts recorded before the count
    /// lack it.
    ChangesRequested {
        text: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        serious_findings: Option<u32>,
    },
}
impl Verdict {
    pub fn serious_findings(&self) -> u32 {
        match self {
            Self::Approved { serious_findings, .. } => *serious_findings,
            Self::ChangesRequested { serious_findings, .. } => serious_findings.unwrap_or(0),
        }
    }
}
fn is_zero_u32(n: &u32) -> bool {
    *n == 0
}
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct WrapUp {
    /// The revision at which Alondra asked the node to clean up.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub asked_at: Option<Revision>,
    /// The revision of the node's last `cleanup_done`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cleaned_at: Option<Revision>,
    /// The task was cancelled while it had a node, so Alondra closes that node.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub close_cancelled: bool,
    /// What became of the node.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub node: Option<NodeEnd>,
}
impl WrapUp {
    pub fn is_default(&self) -> bool {
        *self == Self::default()
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NodeEnd {
    /// Alondra closed it with `claude rm` and its worktree is gone.
    Closed,
    /// It was already gone from `claude agents` when Alondra came to close it.
    Gone,
    /// Alondra could not close it and the user chose to leave it open.
    KeptByUser,
    /// It is not a session Alondra launched (the user's own, or the orchestrator): never closed.
    UsersOwn,
}
impl Task {
    /// Still under way or waiting on something: what every client counts as a Task in progress.
    pub fn active(&self) -> bool {
        !self.state.terminal() && !self.archived
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ModelChooser {
    Orchestrator,
    User,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskModel {
    pub model: String,
    pub chosen_by: ModelChooser,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct StackDependency {
    pub upstream_task: TaskId,
    pub presentation_id: InteractionId,
    pub pr_id: PrId,
    pub head: String,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct StackReadiness {
    pub task_id: TaskId,
    pub presentation_id: InteractionId,
    pub pr_id: PrId,
    pub head: String,
    pub ready: bool,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Document {
    pub id: DocumentId,
    pub title: String,
    pub current_revision: DocumentRevisionId,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DocumentRevision {
    pub id: DocumentRevisionId,
    pub document_id: DocumentId,
    pub number: Revision,
    pub content_digest: String,
    pub content_reference: String,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DocumentReview {
    pub task_id: TaskId,
    pub document_id: DocumentId,
    pub decisions: Vec<DocumentDecision>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DocumentDecision {
    pub revision_id: DocumentRevisionId,
    pub presentation_id: Option<InteractionId>,
    pub accepted: bool,
    pub user_id: String,
    pub at_revision: Revision,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PrState {
    Open,
    Closed,
    Merged,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MergeMethod {
    Merge,
    Squash,
    Rebase,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PullRequest {
    pub id: PrId,
    pub task_id: TaskId,
    pub host: String,
    pub repository: String,
    pub number: u64,
    pub head: String,
    pub base: String,
    pub state: PrState,
    pub merged_head: Option<String>,
    pub allowed_methods: Vec<MergeMethod>,
    pub review: Option<PullRequestReview>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PullRequestReview {
    pub decisions: Vec<PrDecision>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PrDecision {
    pub presentation_id: InteractionId,
    pub head: String,
    pub accepted: bool,
    pub user_id: String,
    pub at_revision: Revision,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum DeliveryTarget {
    Text {
        text: String,
    },
    DocumentRevision {
        document_id: DocumentId,
        revision_id: DocumentRevisionId,
    },
    PullRequest {
        pr_id: PrId,
        head: String,
    },
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum InteractionSubject {
    PullRequest {
        pr_id: PrId,
    },
    DocumentReview {
        document_id: DocumentId,
        revision_id: DocumentRevisionId,
    },
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InputKind {
    Text,
    Boolean,
    Integer,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct InputSpec {
    pub id: String,
    pub kind: InputKind,
    pub required: bool,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "value", rename_all = "snake_case")]
pub enum InputValue {
    Text(String),
    Boolean(bool),
    Integer(i64),
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct QuestionOption {
    pub id: String,
    pub label: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub inputs: Vec<InputSpec>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AttachmentRef {
    pub id: String,
    pub name: String,
    pub content_digest: String,
    pub content_reference: String,
    pub bytes: u64,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Answer {
    #[serde(default)]
    pub attachments: Vec<AttachmentRef>,
    pub option_id: String,
    pub inputs: BTreeMap<String, InputValue>,
    pub responder: String,
    pub at_revision: Revision,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Acceptance {
    pub user_id: String,
    pub at_revision: Revision,
    pub valid: bool,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Feedback {
    #[serde(default)]
    pub attachments: Vec<AttachmentRef>,
    pub user_id: String,
    pub body: String,
    pub at_revision: Revision,
    #[serde(default)]
    pub instruction: Instruction,
}
impl Feedback {
    pub const ALONDRA: &'static str = "alondra";
}
/// What Alondra told the Agent to do with its delivery. The Task shows it until the Agent
/// presents again, which every instruction ends by asking for.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Instruction {
    #[default]
    Changes,
    Question,
    Reply,
    Conflicts,
}
impl Instruction {
    /// The words the Agent receives: the user's text, then what to do once it is done.
    pub fn message(self, body: &str) -> String {
        match self {
            Self::Changes => format!("The user requested changes on your result: {body}\n\nWhen they are done, present the result again with present_result, for a PR with its new head."),
            Self::Question => format!("The user asks about your result: {body}\n\nPut the answer in the report, then present the result again with present_result, for a PR with its new head."),
            Self::Reply => format!("The user replied to your result: {body}\n\nWhen you have acted on it, present the result again with present_result, for a PR with its new head."),
            Self::Conflicts => body.to_owned(),
        }
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum InteractionPayload {
    Information {
        read_by: BTreeSet<String>,
    },
    Question {
        options: Vec<QuestionOption>,
        answer: Option<Answer>,
    },
    ActionRequest {
        options: Vec<QuestionOption>,
        answer: Option<Answer>,
    },
    Delivery {
        targets: Vec<DeliveryTarget>,
        is_final: bool,
        acceptance: Option<Acceptance>,
        feedback: Vec<Feedback>,
    },
}
/// What the card asks of the user: the kind comes from what the user contributes, not from the
/// object behind it (a PR or a plan is what the card carries).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CardKind {
    Decide,
    Review,
    Authorize,
    Know,
    Provide,
    Test,
    Do,
    Problem,
    Propose,
}
impl CardKind {
    /// A notice or a proposal never stops a node.
    pub fn never_blocks(self) -> bool {
        matches!(self, Self::Know | Self::Propose)
    }
    /// The asks a node opens one at a time.
    pub fn one_at_a_time(self) -> bool {
        matches!(self, Self::Decide | Self::Do | Self::Provide)
    }
    pub fn needs_why(self) -> bool {
        matches!(self, Self::Decide | Self::Do | Self::Provide | Self::Propose | Self::Problem)
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContentKind {
    Plan,
    Document,
    Diff,
    Command,
    Report,
    Steps,
    Options,
    Diagnosis,
    Attachments,
}
/// The long part of a card, shown only in its detail. Without `summary` Alondra computes one.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CardContent {
    pub kind: ContentKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    #[serde(default)]
    pub body: String,
}
/// What a card holds back until it is answered.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum CardTarget {
    PullRequest { pr_id: PrId },
    Document { document_id: DocumentId },
    Delivery { interaction_id: InteractionId },
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CardSpec {
    pub kind: CardKind,
    /// What the user must do, in the imperative: at most 90 characters.
    pub need: String,
    /// Why, and whom it holds back: at most 240 characters.
    #[serde(default)]
    pub why: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content: Option<CardContent>,
    /// The option id the author recommends.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recommended: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub blocks: Vec<CardTarget>,
    /// The open card of the same author this one replaces; it is withdrawn in the same step.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub replaces: Option<InteractionId>,
    /// How the node will check the user did it (Do only).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verify: Option<String>,
    /// The card is one question of several asked together: each is its own card, and the node
    /// gets every answer at once when the last one is answered.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub group: Option<QuestionGroup>,
}
/// Where a card stands among the questions asked together.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct QuestionGroup {
    /// The same on every question of the group.
    pub id: String,
    /// From 1.
    pub index: u8,
    pub total: u8,
}
/// One question of several asked together; it becomes a Decide card of its own.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GroupQuestion {
    /// The question, clear on its own: at most 90 characters.
    pub need: String,
    /// What the user needs to know to answer it: at most 240 characters.
    #[serde(default)]
    pub why: String,
    pub options: Vec<QuestionOption>,
    /// The id of the option the author recommends.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recommended: Option<String>,
}
impl QuestionGroup {
    pub const MAX: usize = 4;
    /// The id of the card for question `index` (from 1) of the group `id`.
    pub fn card_id(id: &InteractionId, index: usize) -> InteractionId {
        InteractionId(format!("{id}-q{index}"))
    }
}
/// A label an author marked with "(Recommended)", as Claude Code's own questions do: the label
/// without the suffix, when it had one. A recommendation belongs in `recommended`, not the label.
pub fn without_recommended_suffix(label: &str) -> Option<String> {
    const SUFFIX: &str = "(recommended)";
    let trimmed = label.trim_end();
    let cut = trimmed.len().checked_sub(SUFFIX.len())?;
    if !trimmed.is_char_boundary(cut) || !trimmed[cut..].eq_ignore_ascii_case(SUFFIX) {
        return None;
    }
    let bare = trimmed[..cut].trim_end();
    (!bare.is_empty()).then(|| bare.to_owned())
}
impl CardSpec {
    pub const NEED_MAX: usize = 90;
    pub const WHY_MAX: usize = 240;
    pub const SUMMARY_MAX: usize = 60;

    pub fn new(kind: CardKind, need: impl Into<String>) -> Self {
        CardSpec {
            kind,
            need: need.into(),
            why: String::new(),
            content: None,
            recommended: None,
            blocks: vec![],
            replaces: None,
            verify: None,
            group: None,
        }
    }
    pub fn why(mut self, why: impl Into<String>) -> Self {
        self.why = why.into();
        self
    }
    pub fn content(mut self, kind: ContentKind, body: impl Into<String>) -> Self {
        self.content = Some(CardContent { kind, summary: None, body: body.into() });
        self
    }
    /// A card read from a body written before cards existed: its first line is what is needed,
    /// the next paragraph why, and a longer body goes to the content.
    pub fn derived(kind: CardKind, body: &str) -> Self {
        let mut lines = body.lines().map(str::trim).filter(|line| !line.is_empty());
        let first = lines.next().unwrap_or_default().trim_start_matches('#').trim();
        let need = if first.is_empty() { fallback_need(kind).to_owned() } else { clip(first, Self::NEED_MAX) };
        let rest: Vec<&str> = lines.collect();
        let why = match rest.first() {
            Some(paragraph) => clip(paragraph, Self::WHY_MAX),
            None if kind.needs_why() => need.clone(),
            None => String::new(),
        };
        let mut card = CardSpec::new(kind, need).why(why);
        if rest.len() > 1 || first.chars().count() > Self::NEED_MAX || rest.first().is_some_and(|p| p.chars().count() > Self::WHY_MAX) {
            card = card.content(ContentKind::Report, body.trim());
        }
        if kind == CardKind::Do {
            card.verify = Some("The node checks it once you answer.".into());
        }
        card
    }
    /// A question with two or more choices is a decision; one the user answers in words or with
    /// a file is something only they can provide.
    pub fn question(body: &str, options: &[QuestionOption]) -> Self {
        Self::derived(if options.len() >= 2 { CardKind::Decide } else { CardKind::Provide }, body)
    }
    /// A delivery: a final one is reviewed, a progress one only read.
    pub fn delivery(body: &str, targets: &[DeliveryTarget], is_final: bool) -> Self {
        let mut card = Self::derived(if is_final { CardKind::Review } else { CardKind::Know }, body);
        let kind = if targets.iter().any(|t| matches!(t, DeliveryTarget::PullRequest { .. })) {
            ContentKind::Diff
        } else if targets.iter().any(|t| matches!(t, DeliveryTarget::DocumentRevision { .. })) {
            ContentKind::Document
        } else {
            ContentKind::Report
        };
        card.content = Some(CardContent { kind, summary: None, body: body.trim().to_owned() });
        card
    }
}
fn fallback_need(kind: CardKind) -> &'static str {
    match kind {
        CardKind::Decide => "Choose an option",
        CardKind::Review => "Review the result",
        CardKind::Authorize => "Allow this action",
        CardKind::Know => "Read this update",
        CardKind::Provide => "Provide what the node asks for",
        CardKind::Test => "Test the result",
        CardKind::Do => "Do what the node asks",
        CardKind::Problem => "Fix the problem",
        CardKind::Propose => "Consider this proposal",
    }
}
/// At most `max` characters, cut at a character boundary and marked with an ellipsis.
pub fn clip(text: &str, max: usize) -> String {
    let text = text.trim();
    if text.chars().count() <= max {
        return text.to_owned();
    }
    let mut cut: String = text.chars().take(max.saturating_sub(1)).collect();
    cut.truncate(cut.trim_end().len());
    cut.push('…');
    cut
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ClarificationReason {
    Unclear,
    Contradicts,
    MissingContext,
}
/// The user asked the author to say it again: its next card replaces this one.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Clarification {
    pub reason: ClarificationReason,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    pub user_id: String,
    pub at_revision: Revision,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Interaction {
    #[serde(default)]
    pub attachments: Vec<AttachmentRef>,
    pub id: InteractionId,
    pub scope: Scope,
    pub author: Author,
    pub recipient: Recipient,
    pub body: String,
    pub blocking: bool,
    pub subject: Option<InteractionSubject>,
    pub payload: InteractionPayload,
    pub revision: Revision,
    pub order: Revision,
    /// A card its author withdrew or replaced: it no longer waits for anything.
    #[serde(default)]
    pub withdrawn: bool,
    pub card: CardSpec,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub clarification: Option<Clarification>,
}
impl Interaction {
    /// The long part the reader sees: the card's content, or else the body its author wrote, so a
    /// notice sent with only `need` and `body` never loses its text. Alondra's own cards keep data
    /// in their body and only show their content.
    pub fn shown_content(&self) -> Option<std::borrow::Cow<'_, CardContent>> {
        use std::borrow::Cow;
        if let Some(content) = &self.card.content {
            return Some(Cow::Borrowed(content));
        }
        let body = self.body.trim();
        let (need, why) = (self.card.need.trim(), self.card.why.trim());
        let only_the_card = body
            .lines()
            .map(|line| line.trim().trim_start_matches('#').trim())
            .filter(|line| !line.is_empty())
            .all(|line| line == need || line == why);
        if self.author == Author::System || only_the_card {
            return None;
        }
        Some(Cow::Owned(CardContent { kind: ContentKind::Report, summary: None, body: body.to_owned() }))
    }
}
/// What a held question stands for: a tool permission prompt, a plan to approve, Claude's
/// AskUserQuestion, or an MCP server asking for input.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Hold {
    Permission,
    Plan,
    Question,
    Elicitation,
}
impl Hold {
    pub const ALLOW: &'static str = "allow";
    pub const DENY: &'static str = "deny";
    pub const ALWAYS: &'static str = "always";
    pub const APPROVE: &'static str = "approve";
    pub const REQUEST_CHANGES: &'static str = "request_changes";
    pub const ACCEPT: &'static str = "accept";
    pub const DECLINE: &'static str = "decline";
    pub const NOTE: &'static str = "text";

    /// The fixed yes/no options of a permission, plan or elicitation; a question brings its own.
    /// A permission's or plan's no, and an elicitation's accept, take optional text.
    pub fn options(self) -> Vec<QuestionOption> {
        self.options_offering(false)
    }

    /// `always` adds a permission's "allow and remember" between its yes and its no.
    pub fn options_offering(self, always: bool) -> Vec<QuestionOption> {
        let note = || vec![InputSpec { id: Self::NOTE.into(), kind: InputKind::Text, required: false }];
        let option = |id: &str, label: &str, description: Option<&str>, inputs: Vec<InputSpec>| QuestionOption {
            id: id.into(), label: label.into(), description: description.map(Into::into), inputs,
        };
        match self {
            Hold::Permission => {
                let mut options = vec![option(Self::ALLOW, "Allow", None, vec![])];
                if always {
                    options.push(option(Self::ALWAYS, "Always for this", None, vec![]));
                }
                options.push(option(Self::DENY, "Deny", Some("Write the reason"), note()));
                options
            }
            Hold::Plan => vec![option(Self::APPROVE, "Approve", None, vec![]),
                option(Self::REQUEST_CHANGES, "Request changes", Some("Write what to change"), note())],
            Hold::Elicitation => vec![option(Self::ACCEPT, "Accept", None, note()),
                option(Self::DECLINE, "Decline", None, vec![])],
            Hold::Question => vec![],
        }
    }

    /// The permission update Claude Code suggested that "Always for this" applies: the first rule
    /// that allows the call and is kept for the project, the narrowest place that outlives the
    /// session (this checkout's local settings, then the project's shared ones, then the session
    /// alone). Never the user's global settings, a mode change or a directory grant, which reach
    /// further than the one call the user is answering.
    pub fn always_suggestion(suggestions: &serde_json::Value) -> Option<serde_json::Value> {
        const SCOPES: [&str; 3] = ["localSettings", "projectSettings", "session"];
        let text = |entry: &serde_json::Value, key: &str| entry.get(key).and_then(serde_json::Value::as_str).map(str::to_owned);
        let entries = suggestions.as_array()?;
        SCOPES.iter().find_map(|scope| entries.iter().find(|entry| {
            text(entry, "type").as_deref() == Some("addRules")
                && text(entry, "behavior").as_deref() == Some("allow")
                && text(entry, "destination").as_deref() == Some(scope)
                && entry.get("rules").and_then(serde_json::Value::as_array).is_some_and(|rules| !rules.is_empty())
        })).cloned()
    }

    pub fn approves(self, option_id: &str) -> bool {
        option_id == match self {
            Hold::Permission => Self::ALLOW,
            Hold::Plan => Self::APPROVE,
            Hold::Elicitation => Self::ACCEPT,
            Hold::Question => return false,
        }
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MergeAttemptState {
    Unclaimed,
    Claimed,
    Uncertain,
    Confirmed,
    Failed,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct MergeAuthorization {
    pub id: AuthorizationId,
    pub task_id: TaskId,
    pub pr_id: PrId,
    pub presentation_id: InteractionId,
    pub head: String,
    pub method: MergeMethod,
    pub user_id: String,
    pub valid: bool,
    pub attempt: MergeAttemptState,
    pub claimed_by: Option<AgentId>,
    pub at_revision: Revision,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DomainState {
    pub data_epoch: DataEpoch,
    pub revision: Revision,
    pub orchestrator_generation: u64,
    pub requests: BTreeMap<RequestId, Request>,
    pub agents: BTreeMap<AgentId, Agent>,
    pub tasks: BTreeMap<TaskId, Task>,
    pub documents: BTreeMap<DocumentId, Document>,
    pub document_revisions: BTreeMap<DocumentRevisionId, DocumentRevision>,
    pub document_reviews: Vec<DocumentReview>,
    pub pull_requests: BTreeMap<PrId, PullRequest>,
    pub interactions: BTreeMap<InteractionId, Interaction>,
    pub merge_authorizations: BTreeMap<AuthorizationId, MergeAuthorization>,
    pub stack_readiness: BTreeMap<TaskId, StackReadiness>,
    #[serde(default)]
    pub schedule: ScheduleState,
    /// Questions a Claude Code hook is waiting on: the hook, not the outbox, hands the answer.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub holds: BTreeMap<InteractionId, Hold>,
}
impl DomainState {
    pub fn new(epoch: impl Into<DataEpoch>) -> Self {
        Self {
            data_epoch: epoch.into(),
            revision: 0,
            orchestrator_generation: 0,
            requests: BTreeMap::new(),
            agents: BTreeMap::new(),
            tasks: BTreeMap::new(),
            documents: BTreeMap::new(),
            document_revisions: BTreeMap::new(),
            document_reviews: vec![],
            pull_requests: BTreeMap::new(),
            interactions: BTreeMap::new(),
            merge_authorizations: BTreeMap::new(),
            stack_readiness: BTreeMap::new(),
            schedule: ScheduleState::default(),
            holds: BTreeMap::new(),
        }
    }
    /// The orchestrator has neither defined it into Tasks nor closed it.
    pub fn is_defining(&self, request: &Request) -> bool {
        !request.closed && !self.tasks.values().any(|task| task.request_id == request.id)
    }
}

/// What a node is told when it tries to send the user a Know card.
pub const NODES_DO_NOT_NOTIFY: &str = "Nodes don't send notices. To ask/decide use ask; to need an action use request_action; for input use request_input; to have the user try something use request_test; report results in present_result.";
