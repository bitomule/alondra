use serde::{Deserialize, Serialize};

pub type Millis = i64;

pub const NEWS_WINDOW_MS: Millis = 48 * 3600 * 1000;

pub const ORCHESTRATOR_PROJECT: &str = "orchestrator";
pub const OTHERS_PROJECT: &str = "others";

#[derive(Serialize, Clone, Copy, PartialEq, Eq, Debug, Hash, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum NodeState {
    WaitingAnswer,
    YourTurn,
    WaitingMerge,
    Unclear,
    Delivered,
    WaitingMachine,
    Stalled,
    Working,
    Done,
}

impl NodeState {
    pub fn label(self) -> &'static str {
        match self {
            NodeState::Working => "working",
            NodeState::WaitingAnswer => "waiting for an answer",
            NodeState::YourTurn => "your turn",
            NodeState::WaitingMerge => "waiting for your merge",
            NodeState::Delivered => "delivering a result",
            NodeState::WaitingMachine => "waiting for the machine",
            NodeState::Done => "done",
            NodeState::Unclear => "unclear what it wants",
            NodeState::Stalled => "stalled",
        }
    }

    fn parse(value: &str) -> Self {
        match value {
            "working" => NodeState::Working,
            "waiting_answer" => NodeState::WaitingAnswer,
            "your_turn" => NodeState::YourTurn,
            "waiting_merge" => NodeState::WaitingMerge,
            "delivered" => NodeState::Delivered,
            "waiting_machine" => NodeState::WaitingMachine,
            "done" => NodeState::Done,
            "stalled" => NodeState::Stalled,
            _ => NodeState::Unclear,
        }
    }
}

/// A state this build does not know yet reads as `unclear`, so an older client keeps working
/// when the daemon learns a new state.
impl<'de> Deserialize<'de> for NodeState {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        Ok(NodeState::parse(&String::deserialize(d)?))
    }
}

/// What the orchestrator is doing. It is not a node like the others, so it has its own states
/// and its own place in the fleet; `state` still carries the closest node state for older clients.
#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug, Hash)]
#[serde(rename_all = "snake_case")]
pub enum OrchestratorState {
    OnDuty,
    Handling,
    WaitingForYou,
    Offline,
    #[serde(other)]
    Unknown,
}

impl OrchestratorState {
    pub fn label(self) -> &'static str {
        match self {
            OrchestratorState::OnDuty => "on duty",
            OrchestratorState::Handling => "handling a request",
            OrchestratorState::WaitingForYou => "waiting for you",
            OrchestratorState::Offline => "offline",
            OrchestratorState::Unknown => "unknown",
        }
    }

    pub fn node_state(self) -> NodeState {
        match self {
            OrchestratorState::Handling => NodeState::Working,
            OrchestratorState::WaitingForYou => NodeState::WaitingAnswer,
            OrchestratorState::OnDuty | OrchestratorState::Offline | OrchestratorState::Unknown => NodeState::Delivered,
        }
    }
}

/// The fact that put a node in its state.
#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug, Hash)]
#[serde(rename_all = "snake_case")]
pub enum Signal {
    OpenQuestion,
    PermissionPrompt,
    ActionRequested,
    CallStuck,
    ToolInFlight,
    Hook,
    MessageHanded,
    DaemonActive,
    BackgroundTask,
    BackgroundSuspect,
    UsageLimit,
    DeclaredWait,
    PrAwaitingUser,
    FinalReport,
    Reviewed,
    Report,
    TurnEndedUndeclared,
    NoProcess,
    #[serde(other)]
    Nothing,
}

/// The evidence behind a node's state: which row of the table fired, on what signal, and when
/// that signal was last confirmed.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Evidence {
    pub row: String,
    pub signal: Signal,
    pub at: Millis,
}

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug)]
#[serde(rename_all = "snake_case")]
pub enum StateSource {
    Hooks,
    Daemon,
    Classifier,
    Watcher,
}

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug)]
#[serde(rename_all = "snake_case")]
pub enum TurnSignal {
    Asked,
    Delivered,
    Finished,
    WaitingMachine,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct ToolInFlight {
    pub tool_use_id: Option<String>,
    pub name: String,
    pub summary: Option<String>,
    pub since: Millis,
}

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug, Hash)]
#[serde(rename_all = "snake_case")]
pub enum BackgroundKind {
    Agent,
    Shell,
    #[serde(other)]
    Other,
}

/// A subagent or background shell the node left running, as the Claude daemon's job state
/// lists it.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct BackgroundItem {
    pub kind: BackgroundKind,
    pub label: String,
    pub started_at: Millis,
    /// Running for long while the node shows no other work. Only the server's views set it.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub suspect: bool,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Node {
    pub session_id: String,
    pub short_id: String,
    pub name: Option<String>,
    pub cwd: Option<String>,
    pub pid: Option<i32>,
    /// Internal: whether the daemon can attach a session right now. Not a node state: alondrad
    /// strips it from what clients receive (`http::client_node`), but the store keeps it, so a
    /// restart does not lose which nodes are alive.
    #[serde(default)]
    pub alive: bool,
    #[serde(default)]
    pub in_daemon: bool,
    pub is_orchestrator: bool,
    pub has_plugin: bool,
    pub state: NodeState,
    pub state_since: Millis,
    pub state_source: StateSource,
    pub busy: bool,
    pub tool: Option<ToolInFlight>,
    pub waiting_permission: bool,
    pub last_message: Option<String>,
    pub last_message_at: Option<Millis>,
    #[serde(default)]
    pub last_prompt: Option<String>,
    #[serde(default)]
    pub last_prompt_at: Option<Millis>,
    #[serde(default)]
    pub attached: bool,
    #[serde(default)]
    pub menu_waiting: Option<String>,
    #[serde(default)]
    pub mission: Option<String>,
    pub daemon_detail: Option<String>,
    pub daemon_needs: Option<String>,
    pub daemon_updated_at: Option<Millis>,
    pub turn: u64,
    pub turn_signal: Option<TurnSignal>,
    pub classified: Option<Classification>,
    pub usage_limited: bool,
    pub last_seen: Millis,
    #[serde(default)]
    pub acknowledged_at: Option<Millis>,
    #[serde(default)]
    pub last_work_at: Option<Millis>,
    #[serde(default)]
    pub last_stop_at: Option<Millis>,
    #[serde(default)]
    pub handed_at: Option<Millis>,
    #[serde(default)]
    pub daemon_state: Option<String>,
    #[serde(default)]
    pub daemon_tempo: Option<String>,
    #[serde(default)]
    pub daemon_seen_at: Option<Millis>,
    #[serde(default)]
    pub daemon_tasks: u32,
    #[serde(default)]
    pub background: Vec<BackgroundItem>,
    #[serde(default)]
    pub usage_limited_at: Option<Millis>,
    #[serde(default)]
    pub signal_at: Option<Millis>,
    #[serde(default)]
    pub episode_since: Millis,
    /// When the node last became done: what Alondra queued for it before then is void.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub done_at: Option<Millis>,
    /// When the node's current turn began: its last `UserPromptSubmit`, or the daemon seeing a
    /// node without the plugin wake up.
    #[serde(default)]
    pub turn_since: Millis,
    #[serde(default, deserialize_with = "lenient")]
    pub evidence: Option<Evidence>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub orchestrator_state: Option<OrchestratorState>,
}

fn lenient<'de, D: serde::Deserializer<'de>, T: serde::de::DeserializeOwned>(d: D) -> Result<Option<T>, D::Error> {
    let value = serde_json::Value::deserialize(d)?;
    Ok(serde_json::from_value(value).ok())
}

impl Node {
    pub fn new(session_id: &str, at: Millis) -> Self {
        Node {
            session_id: session_id.to_string(),
            short_id: session_id.chars().take(8).collect(),
            name: None,
            cwd: None,
            pid: None,
            alive: true,
            in_daemon: false,
            is_orchestrator: false,
            has_plugin: false,
            state: NodeState::Working,
            state_since: at,
            state_source: StateSource::Watcher,
            busy: false,
            tool: None,
            waiting_permission: false,
            last_message: None,
            last_message_at: None,
            last_prompt: None,
            last_prompt_at: None,
            attached: false,
            menu_waiting: None,
            mission: None,
            daemon_detail: None,
            daemon_needs: None,
            daemon_updated_at: None,
            turn: 0,
            turn_signal: None,
            classified: None,
            usage_limited: false,
            last_seen: at,
            acknowledged_at: None,
            last_work_at: None,
            last_stop_at: None,
            handed_at: None,
            daemon_state: None,
            daemon_tempo: None,
            daemon_seen_at: None,
            daemon_tasks: 0,
            background: vec![],
            usage_limited_at: None,
            signal_at: None,
            episode_since: 0,
            done_at: None,
            turn_since: 0,
            evidence: None,
            orchestrator_state: None,
        }
    }

    /// Something from the user or the orchestrator reached the node: what it declared before
    /// no longer explains what it does next.
    pub fn begin_episode(&mut self, at: Millis) {
        self.episode_since = at;
        self.turn_signal = None;
        self.signal_at = None;
    }

    /// Background nodes the sentinel looks after: the Claude daemon runs them and nobody is
    /// sitting at their terminal.
    pub fn watched(&self) -> bool {
        self.in_daemon && !self.is_orchestrator
    }

    pub fn has_news(&self, now: Millis) -> bool {
        let finished = matches!(self.state, NodeState::Delivered | NodeState::Done | NodeState::Unclear | NodeState::WaitingMachine);
        let said_at = self.last_message_at.unwrap_or(self.state_since).max(self.state_since);
        let recent = now - said_at < NEWS_WINDOW_MS;
        finished && recent && !self.is_orchestrator && self.acknowledged_at.is_none_or(|a| a < said_at)
    }

    pub fn project(&self) -> String {
        if self.is_orchestrator {
            return ORCHESTRATOR_PROJECT.to_string();
        }
        if let Some(cwd) = &self.cwd
            && let Some(rest) = cwd.split("/Projects/").nth(1)
            && let Some(first) = rest.split('/').next()
            && !first.is_empty()
        {
            return first.to_string();
        }
        self.name
            .as_deref()
            .and_then(|n| n.split('/').next())
            .filter(|p| !p.is_empty())
            .unwrap_or(OTHERS_PROJECT)
            .to_string()
    }

    pub fn display_name(&self) -> &str {
        self.name.as_deref().unwrap_or(&self.short_id)
    }

    /// Whether a card from this node may offer to close it. The orchestrator never: Alondra
    /// keeps it around on purpose. Whatever its session is doing does not matter here.
    pub fn offers_close(&self) -> bool {
        !self.is_orchestrator
    }
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Classification {
    pub turn: u64,
    pub state: NodeState,
    pub confidence: Option<f64>,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Default)]
pub struct QuestionOption {
    pub label: String,
    #[serde(default)]
    pub description: Option<String>,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Link {
    pub url: String,
    #[serde(default)]
    pub title: Option<String>,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Attachment {
    pub name: String,
    pub source_path: String,
    #[serde(default)]
    pub stored_path: Option<String>,
    #[serde(default)]
    pub bytes: Option<u64>,
}

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug)]
#[serde(rename_all = "snake_case")]
pub enum QuestionOrigin {
    Ask,
    AskUserQuestion,
    Permission,
    Plan,
    Classifier,
    Elicitation,
}

/// One of a node's cards, named by what it is and its number.
#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug, Hash, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum CardRef {
    Question(u64),
    Report(u64),
}

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug)]
#[serde(rename_all = "snake_case")]
pub enum QuestionStatus {
    Open,
    Answered,
    Delivered,
    Withdrawn,
    Orphaned,
    AnsweredInTerminal,
}

impl QuestionStatus {
    pub fn is_open(self) -> bool {
        matches!(self, QuestionStatus::Open)
    }
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Answer {
    pub text: String,
    #[serde(default)]
    pub option: Option<String>,
    #[serde(default)]
    pub by: Option<String>,
    #[serde(default)]
    pub device: Option<String>,
    pub at: Millis,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Question {
    pub id: u64,
    pub session_id: String,
    pub node_name: Option<String>,
    pub title: String,
    #[serde(default)]
    pub detail: Option<String>,
    #[serde(default)]
    pub options: Vec<QuestionOption>,
    #[serde(default)]
    pub links: Vec<Link>,
    #[serde(default)]
    pub attachments: Vec<Attachment>,
    pub blocking: bool,
    pub origin: QuestionOrigin,
    #[serde(default)]
    pub tool_use_id: Option<String>,
    #[serde(default)]
    pub to_orchestrator: bool,
    #[serde(default)]
    pub node_context: Option<String>,
    pub status: QuestionStatus,
    #[serde(default)]
    pub answer: Option<Answer>,
    pub asked_at: Millis,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub action: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub steps: Vec<String>,
    /// The node's mission, on cards Alondra built from its terminal, so they say what it was doing.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mission: Option<String>,
    /// The node's other open cards it declared this question belongs with.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub related: Vec<CardRef>,
}

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug)]
#[serde(rename_all = "snake_case")]
pub enum ReportKind {
    #[serde(alias = "delivery", alias = "finding", alias = "aviso")]
    Notice,
    #[serde(alias = "documento")]
    Document,
    Progress,
    #[serde(alias = "terminado")]
    Final,
}

impl ReportKind {
    pub fn label(self) -> &'static str {
        match self {
            ReportKind::Notice => "notice",
            ReportKind::Document => "document",
            ReportKind::Progress => "progress",
            ReportKind::Final => "finished",
        }
    }
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Report {
    pub id: u64,
    pub session_id: String,
    pub node_name: Option<String>,
    pub title: String,
    #[serde(default)]
    pub body: Option<String>,
    #[serde(default)]
    pub links: Vec<Link>,
    #[serde(default)]
    pub attachments: Vec<Attachment>,
    pub kind: ReportKind,
    pub created_at: Millis,
    #[serde(default)]
    pub read: bool,
    /// The node's mission, on cards Alondra built from its terminal, so they say what it was doing.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mission: Option<String>,
    /// The report gate thinks it asks the user for something that should have been a question.
    #[serde(default)]
    pub looks_like_question: bool,
    /// The node's other open cards it declared this report belongs with.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub related: Vec<CardRef>,
}

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug)]
#[serde(rename_all = "snake_case")]
pub enum MessageFrom {
    #[serde(rename = "david", alias = "user")]
    User,
    Orchestrator,
    Alondra,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Message {
    pub id: u64,
    pub session_id: String,
    pub from: MessageFrom,
    pub text: String,
    #[serde(default)]
    pub answers_question: Option<u64>,
    pub created_at: Millis,
    #[serde(default)]
    pub handed_at: Option<Millis>,
    #[serde(default)]
    pub delivered_at: Option<Millis>,
    #[serde(default, skip_serializing_if = "Option::is_none", deserialize_with = "lenient")]
    pub about: Option<MessageAbout>,
}

/// What a message Alondra queued stands for, when the state machine has to know.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum MessageAbout {
    PrMerged { pr_id: u64 },
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Default)]
pub struct Machine {
    pub at: Millis,
    pub memory_total: u64,
    pub memory_available: u64,
    pub memory_free_percent: Option<u32>,
    pub load: [f64; 3],
    pub cpus: usize,
    pub cpu_usage: f32,
    pub disk_total: u64,
    pub disk_available: u64,
    pub unreliable: bool,
}

pub const STRAINED_FREE_PERCENT: u32 = 15;

impl Machine {
    pub fn strained(&self) -> bool {
        self.unreliable || self.memory_free_percent.is_some_and(|p| p < STRAINED_FREE_PERCENT)
    }
}

/// Option labels the server writes on held questions and matches when they come back.
/// Apps shipped before the switch to English send the Spanish labels, so `canonical`
/// maps them onto the English ones.
pub mod answer_option {
    pub const APPROVE: &str = "Approve";
    pub const REQUEST_CHANGES: &str = "Request changes";
    pub const ALLOW: &str = "Allow";
    pub const DENY: &str = "Deny";
    pub const ACCEPT: &str = "Accept";
    pub const DECLINE: &str = "Decline";
    pub const MORE_CONTEXT: &str = "I need more context";
    pub const DONE: &str = "Done";
    pub const CANNOT: &str = "I can't / explain";
    pub const WRITE_THE_REASON: &str = "Write the reason";

    /// The two answers a "your turn" card offers: it is done, or the user explains what stops them.
    pub fn for_action() -> Vec<super::QuestionOption> {
        vec![
            super::QuestionOption { label: DONE.into(), description: None },
            super::QuestionOption { label: CANNOT.into(), description: Some(WRITE_THE_REASON.into()) },
        ]
    }

    const LEGACY: [(&str, &str); 5] = [
        ("Aprobar", APPROVE),
        ("Pedir cambios", REQUEST_CHANGES),
        ("Permitir", ALLOW),
        ("Denegar", DENY),
        ("Necesito más contexto", MORE_CONTEXT),
    ];

    pub fn canonical(label: &str) -> &str {
        LEGACY.iter().find(|(old, _)| label.trim().eq_ignore_ascii_case(old)).map_or(label, |(_, new)| new)
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn legacy_spanish_labels_map_onto_the_english_ones() {
            assert_eq!(canonical("Aprobar"), APPROVE);
            assert_eq!(canonical("Pedir cambios"), REQUEST_CHANGES);
            assert_eq!(canonical("Permitir"), ALLOW);
            assert_eq!(canonical("denegar "), DENY);
            assert_eq!(canonical("Necesito más contexto"), MORE_CONTEXT);
        }

        #[test]
        fn english_and_free_labels_pass_through() {
            assert_eq!(canonical(APPROVE), APPROVE);
            assert_eq!(canonical("Merge #150"), "Merge #150");
        }
    }
}
