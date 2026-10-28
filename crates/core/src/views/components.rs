use serde::de::DeserializeOwned;
use serde::{Deserialize, Deserializer, Serialize};

/// An optional value a newer daemon may send that this client does not know reads as absent.
fn known<'de, D: Deserializer<'de>, T: DeserializeOwned>(deserializer: D) -> Result<Option<T>, D::Error> {
    Ok(Option::<serde_json::Value>::deserialize(deserializer)?.and_then(|value| serde_json::from_value(value).ok()))
}

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
    Info,
    Accent,
    Success,
    Warning,
    Critical,
    YourTurn,
    #[serde(other)]
    Neutral,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Level {
    Warm,
    Hot,
    #[serde(other)]
    Calm,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Icon {
    Question,
    Action,
    Document,
    PullRequest,
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
    #[serde(other)]
    Notice,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Detail {
    Included,
    #[serde(other)]
    Reference,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TimeFormat {
    Absolute,
    Time,
    #[serde(other)]
    Relative,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Style {
    Primary,
    Destructive,
    Plain,
    #[serde(other)]
    Secondary,
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
    /// A kind from a newer daemon that this client does not know. Only decoded, never sent.
    #[serde(other, skip_serializing)]
    Unsupported,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContentKind {
    Document,
    Diff,
    #[serde(other)]
    File,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AttachmentKind {
    Image,
    Text,
    #[serde(other)]
    Binary,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CheckState {
    Pass,
    Fail,
    #[serde(other)]
    Pending,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct View {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub account_configuration: Option<AccountConfigurationPresentation>,
    pub protocol: u32,
    pub data_epoch: String,
    pub view: String,
    pub key: String,
    pub client: Client,
    pub version: String,
    pub cursor: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// What the view holds, in one line, for the row that leads to it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
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
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub capabilities: Vec<String>,
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
    /// Present while new work waits because no Claude account has a session: what to show across
    /// the top, written out.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_wait: Option<Notice>,
}
/// Claude usage has run out: no account of this Mac can work until `until`. `scope` says what
/// `macs_exhausted` of `macs_total` covers:
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
    /// What the reading is made of, one part each, for a client to list when the gauge is opened:
    /// the Claude margin's accounts.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub parts: Vec<GaugePart>,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct GaugePart {
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
    /// A client that knows it draws the time elapsed instead of `text`, which stays for those that do not.
    #[serde(default, deserialize_with = "known", skip_serializing_if = "Option::is_none")]
    pub elapsed: Option<Elapsed>,
}
/// A span of time a client counts itself: from `since` until now, or until `until` once it is over.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Elapsed {
    pub since: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub until: Option<String>,
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
    /// How a client lays its rows out; without one, as each screen lays them out.
    #[serde(default, deserialize_with = "known", skip_serializing_if = "Option::is_none")]
    pub style: Option<SectionStyle>,
    /// What its header offers over the whole section, such as clearing the completed work.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub actions: Vec<Action>,
}
/// A section of rows a client lays out as a settings list: each row a status symbol in its first
/// label's tone, the label as a chip, its subtitle, its first action as a button and the rest in a
/// menu.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SectionStyle {
    List,
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
    #[serde(default, deserialize_with = "known", skip_serializing_if = "Option::is_none")]
    pub kind: Option<CardKind>,
    /// What the user must do, in the imperative; the same text as `title`. Markdown, drawn on one line.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub need: Option<String>,
    /// Why, and whom it holds back; the same text as `subtitle`. Markdown: whole in a detail, on one line in a list.
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
pub struct AccountConfigurationPresentation {
    pub version: String,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AccountPresentation {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_catalog: Option<crate::model_selection::AccountModelCatalog>,
    pub key: crate::accounts::AccountKey,
    pub position: u32,
    pub availability: Label,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub email: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plan: Option<String>,
    pub active_nodes: u32,
    pub freshness: Label,
    #[serde(default)]
    pub windows: Vec<AccountWindowPresentation>,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AccountWindowPresentation {
    pub kind: String,
    pub label: String,
    pub used_percent: u32,
    pub measured_at: crate::Millis,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resets_at: Option<crate::Millis>,
    pub stale: bool,
    pub exhausted: bool,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TaskModelPresentation {
    pub requested: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effective: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pending: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Row {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub account: Option<AccountPresentation>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<TaskModelPresentation>,
    pub id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub link: Option<Link>,
    pub icon: Icon,
    /// Markdown, drawn on one line, like `subtitle`.
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
    #[serde(default, deserialize_with = "known", skip_serializing_if = "Option::is_none")]
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
    /// A kind from a newer daemon that this client does not know. Only decoded, never sent.
    #[serde(other, skip_serializing)]
    Unsupported,
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
    /// Markdown, drawn on one line: a task's state quotes its question or its node's words.
    pub value: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tone: Option<Tone>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub link: Option<Link>,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Checks {
    /// "3 of 5 passed, 1 failing": a client that knows it shows this line and the items only
    /// once the user opens it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<Label>,
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
    /// The action itself, for a client that keeps on the card, disabled, one from a newer daemon
    /// it cannot carry out.
    pub fn on_client(&mut self) -> &mut Action {
        &mut self.0
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
    /// Markdown, drawn on one line.
    pub label: String,
    pub style: Style,
    /// How the button reads: green to advance, red when advancing is risky, grey otherwise.
    #[serde(default, deserialize_with = "known", skip_serializing_if = "Option::is_none")]
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
    /// Markdown, drawn on one line, like `description`.
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
    /// A kind from a newer daemon that this client does not know. Only decoded, never sent.
    #[serde(other, skip_serializing)]
    Unsupported,
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
    /// A kind from a newer daemon that this client does not know. Only decoded, never sent.
    #[serde(other, skip_serializing)]
    Unsupported,
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
    /// A kind from a newer daemon that this client does not know. Only decoded, never sent.
    #[serde(other, skip_serializing)]
    Unsupported,
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
    /// The client opens the sign-in to the MCP server an MCP wait's card waits for: in the Mac's
    /// browser on the Mac, and elsewhere it brings back where the browser came back to.
    McpSignIn {
        card: String,
        server: String,
    },
    /// The client has the nodes that wait for the server connect it again.
    McpReconnect {
        card: String,
    },
    /// The client asks for the new account's name, then signs it in on the Mac: the terminal
    /// runs `command` followed by the name, the Mac app opens Terminal with it, and the iPhone
    /// shows it to finish on the Mac.
    AccountAdd {
        command: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        provider: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        argv: Option<Vec<String>>,
    },
    /// The client signs the account in on the Mac, as for `AccountAdd`, with `command` whole.
    AccountSignIn {
        account: String,
        command: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        provider: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        argv: Option<Vec<String>>,
    },
    /// The client opens the preview of a new crash report, whose issue goes as `failure`
    /// (`alondra report send --kind`). Nothing is sent until the user says so on the preview.
    CrashReport {
        failure: String,
    },
    /// A kind from a newer daemon that this client does not know. Only decoded, never sent.
    #[serde(other, skip_serializing)]
    Unsupported,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Form {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub substitution_was_edited: Option<bool>,
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
    /// A kind from a newer daemon that this client does not know. Only decoded, never sent.
    #[serde(other, skip_serializing)]
    Unsupported,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ChoiceItem {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub group: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub disabled_reason: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub available_when: Option<Condition>,
    pub id: String,
    pub label: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}
