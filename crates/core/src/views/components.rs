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
    Unclear,
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
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Card {
    pub id: String,
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
    pub quotes: Vec<Quote>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub children: Vec<Card>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub facts: Option<Facts>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub checks: Option<Checks>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub actions: Vec<Action>,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ContextLine {
    pub icon: Icon,
    pub text: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub link: Option<Link>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub chip: Option<Label>,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Stamp {
    pub role: String,
    pub at: String,
    pub format: TimeFormat,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Quote {
    pub label: String,
    pub text: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub at: Option<String>,
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
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub children: Vec<Row>,
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
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Action {
    pub id: String,
    pub label: String,
    pub style: Style,
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
}
