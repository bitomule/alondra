use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Client {
    Ios,
    Mac,
    Tui,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Transport {
    Loopback,
    Relay,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Tone {
    Neutral,
    Info,
    Accent,
    Success,
    Warning,
    Critical,
    YourTurn,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Level {
    Calm,
    Warm,
    Hot,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Icon {
    Question,
    Action,
    Document,
    PullRequest,
    Notice,
    Delivery,
    Task,
    Request,
    Node,
    Orchestrator,
    Mission,
    Project,
    Schedule,
    Attachment,
    Check,
    Warning,
    Merge,
    Branch,
    History,
    Machine,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Detail {
    Included,
    Reference,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TimeFormat {
    Relative,
    Absolute,
    Time,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Style {
    Primary,
    Secondary,
    Destructive,
    Plain,
}
impl Style {
    /// The colour a button of this style is painted in when nothing else decides it.
    pub fn role(self) -> Role {
        match self {
            Style::Primary => Role::Advance,
            Style::Destructive => Role::Risk,
            Style::Secondary | Style::Plain => Role::Neutral,
        }
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Presentation {
    Button,
    Options,
    Menu,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContentKind {
    Document,
    Diff,
    File,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AttachmentKind {
    Image,
    Text,
    Binary,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CheckState {
    Pass,
    Fail,
    Pending,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct View {
    pub protocol: u32,
    pub data_epoch: String,
    pub view: String,
    pub key: String,
    pub client: Client,
    pub version: String,
    pub cursor: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    pub status: Status,
    pub sections: Vec<Section>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub empty: Option<Notice>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub notices: Vec<Notice>,
    /// The daemon that served this view; the renderers leave it out and the route fills it in.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub service: Option<Service>,
    /// The first cards of the queue, each the whole `card:<id>` view, so the app draws the front
    /// one from this answer instead of asking for it next.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub first_cards: Vec<View>,
}
/// Which daemon answered and which views protocols it serves, so a client can tell it is behind.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Service {
    pub version: String,
    pub views_protocol: ProtocolRange,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProtocolRange {
    pub min: u32,
    pub max: u32,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Status {
    pub version: String,
    pub summary: String,
    pub badge: u32,
    pub counts: Vec<Count>,
    pub gauges: Vec<Gauge>,
    pub orchestrator: Label,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub update: Option<Notice>,
    /// Present while Claude usage has run out; absent otherwise.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage_exhausted: Option<UsageExhausted>,
}
/// Claude usage has run out. `scope` says what `macs_exhausted` of `macs_total` covers:
/// "this_mac" while the daemon only knows itself, "all_macs" once a relay aggregates every Mac.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct UsageExhausted {
    pub scope: String,
    pub until: String,
    pub macs_exhausted: u32,
    pub macs_total: u32,
    /// The user paused until tomorrow instead of Claude running out.
    #[serde(default)]
    pub paused_by_user: bool,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Count {
    pub id: String,
    pub label: String,
    pub value: u32,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Gauge {
    pub id: String,
    pub label: String,
    pub value: f64,
    pub text: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    pub level: Level,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Label {
    pub text: String,
    pub tone: Tone,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Section {
    pub id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub count: Option<u32>,
    pub collapsed: bool,
    pub items: Vec<Item>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub card_ids: Vec<String>,
    /// What each card in `card_ids` waits for, at the same index: "question" or "read".
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub card_waits: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub more: Option<More>,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct More {
    pub key: String,
    pub remaining: u32,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Item {
    Card(Card),
    Row(Row),
    Markdown(Markdown),
    ContentRef(ContentRef),
    Facts(Facts),
    Checks(Checks),
    Timeline(Timeline),
    Form(Form),
    Notice(Notice),
    Actions(ActionBar),
    /// A kind of item added by a newer daemon: an older client draws a row asking to update
    /// instead of failing the whole view. Never sent.
    #[serde(other)]
    Unknown,
}
/// What a card asks of the user; the stable string clients draw the label, colour and icon from.
pub use crate::domain::CardKind;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Card {
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<CardKind>,
    /// What the user must do, in the imperative; the same text as `title`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub need: Option<String>,
    /// Why, and whom it holds back; the same text as `subtitle`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub why: Option<String>,
    /// One line: who sent it and where ("node · project"). The age is in `timestamps`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub meta: Option<String>,
    /// The one line of content a card shows; the whole content is in the detail.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content_row: Option<ContentRow>,
    /// Cards this one holds back: "blocks PR #649".
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub blocks: Vec<BlockChip>,
    /// Cards that hold this one back.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub blocked_by: Vec<BlockChip>,
    /// Send text to the node (or the orchestrator) the card came from; on every card.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reply: Option<CardAction>,
    /// What does not fit the three visible `actions`, with "Request clarification" and
    /// "Open on GitHub".
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub more: Vec<CardAction>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub link: Option<Link>,
    pub icon: Icon,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub eyebrow: Option<Label>,
    pub title: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub subtitle: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub badges: Vec<Label>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub context: Vec<ContextLine>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub timestamps: Vec<Stamp>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub state: Option<Label>,
    pub detail: Detail,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub body: Option<Markdown>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub attachments: Vec<Attachment>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub actions: Vec<CardAction>,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ContentRow {
    /// `plan`, `document`, `diff`, `command`, `report`, `steps`, `options`, `diagnosis` or `attachments`.
    pub kind: String,
    pub icon: Icon,
    pub label: String,
    pub summary: String,
    /// Other contents behind the main one ("+2").
    #[serde(default, skip_serializing_if = "is_zero")]
    pub more_count: u32,
}
fn is_zero(value: &u32) -> bool {
    *value == 0
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct BlockChip {
    /// The id of the other card; open it with `link`.
    pub id: String,
    pub label: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub link: Option<Link>,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ContextLine {
    pub icon: Icon,
    pub text: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub link: Option<Link>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub chip: Option<Label>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub state: Option<Label>,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Stamp {
    pub role: String,
    pub at: String,
    pub format: TimeFormat,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Row {
    pub id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub link: Option<Link>,
    pub icon: Icon,
    pub title: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub subtitle: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub badges: Vec<Label>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub trailing: Option<Trailing>,
    pub depth: u8,
    pub struck: bool,
    /// The kind of the card this row stands for, so a list can colour it like the card.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<CardKind>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub children: Vec<Row>,
    /// `children` start hidden behind this row; tapping it shows them.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub folded: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub actions: Vec<Action>,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Trailing {
    Count { value: u32 },
    Time { at: String, format: TimeFormat },
    Text { text: String },
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Markdown {
    pub text: String,
    pub truncated: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub more: Option<Link>,
    pub commentable: bool,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ContentRef {
    pub kind: ContentKind,
    pub title: String,
    pub size: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub digest: Option<String>,
    pub fetch: String,
    pub commentable: bool,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Attachment {
    pub id: String,
    pub name: String,
    pub kind: AttachmentKind,
    pub size: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub digest: Option<String>,
    pub fetch: String,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Facts {
    pub items: Vec<Fact>,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Fact {
    pub label: String,
    pub value: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tone: Option<Tone>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub link: Option<Link>,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Checks {
    pub items: Vec<Check>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub link: Option<Link>,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Check {
    pub name: String,
    pub state: CheckState,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Timeline {
    pub entries: Vec<Entry>,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Entry {
    pub at: String,
    pub title: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tone: Option<Tone>,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Notice {
    pub text: String,
    pub tone: Tone,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub action: Option<Action>,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ActionBar {
    pub actions: Vec<Action>,
}
/// An action of a card: one the user can use now. A card never carries one that cannot be used,
/// so there is no way to build it disabled. The wire still says `enabled: true` for the clients
/// that read it; they stop reading it when they move to this type.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(from = "Action", into = "Action")]
pub struct CardAction(Action);
impl CardAction {
    /// The action as a card carries it; `None` when it cannot be used, so the card omits it.
    pub fn usable(action: Action) -> Option<Self> {
        action.enabled.then(|| Self(Action { disabled_reason: None, ..action }))
    }
}
impl From<Action> for CardAction {
    fn from(action: Action) -> Self {
        Self(Action { enabled: true, disabled_reason: None, ..action })
    }
}
impl From<CardAction> for Action {
    fn from(action: CardAction) -> Self {
        action.0
    }
}
impl std::ops::Deref for CardAction {
    type Target = Action;
    fn deref(&self) -> &Action {
        &self.0
    }
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Action {
    pub id: String,
    pub label: String,
    pub style: Style,
    /// How the button reads: green to advance, red when advancing is risky, grey otherwise.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<Role>,
    /// A button that answers with this option of the action: send it as `option_id`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub option_id: Option<String>,
    /// What choosing it means, when its author explained the option.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub presentation: Presentation,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub options: Vec<ActionOption>,
    pub input: Input,
    pub attachments: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub comments: Option<CommentSpec>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub confirm: Option<Confirm>,
    pub enabled: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub disabled_reason: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stamp: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub key: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub link: Option<Link>,
    /// Whether doing it takes its card out of the queue (`views::resolves_card`): the clients hide
    /// the card the moment it is sent when it does, and keep it in place when it does not.
    #[serde(default)]
    pub resolves_card: bool,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    Advance,
    Risk,
    Neutral,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ActionOption {
    pub id: String,
    pub label: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub style: Style,
    pub input: Input,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub key: Option<String>,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Input {
    None,
    Text {
        placeholder: String,
        required: bool,
        multiline: bool,
    },
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct CommentSpec {
    pub kind: CommentKind,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CommentKind {
    Blocks,
    DiffLines,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Confirm {
    pub title: String,
    pub message: String,
    pub button: String,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Link {
    View { key: String },
    Url { url: String },
    Local { local: LocalLink },
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum LocalLink {
    Attach {
        session: String,
    },
    ReviewClone {
        repo: String,
        branch: String,
        pr: u64,
        pr_id: String,
    },
    OpenPath {
        path: String,
    },
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Form {
    pub id: String,
    pub fields: Vec<Field>,
    pub submit: Action,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Field {
    pub id: String,
    pub label: String,
    pub kind: FieldKind,
    pub value: serde_json::Value,
    pub required: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub help: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub visible_when: Option<Condition>,
    pub reload_on_change: bool,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Condition {
    pub field: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub equals: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub any_of: Vec<serde_json::Value>,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum FieldKind {
    Text { placeholder: Option<String> },
    Multiline { placeholder: Option<String> },
    Attachments,
    Toggle,
    Choice { items: Vec<ChoiceItem> },
    Picker { source: String },
    Date,
    Time,
    Stepper { min: i64, max: i64 },
    Lines { placeholder: Option<String> },
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ChoiceItem {
    pub id: String,
    pub label: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}
