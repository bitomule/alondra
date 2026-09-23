use serde::{Deserialize, Serialize};

pub type Millis = i64;

pub const NEWS_WINDOW_MS: Millis = 48 * 3600 * 1000;

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug, Hash, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum NodeState {
    WaitingAnswer,
    Unclear,
    Delivered,
    WaitingMachine,
    Working,
    Done,
}

impl NodeState {
    pub fn label(self) -> &'static str {
        match self {
            NodeState::Working => "trabajando",
            NodeState::WaitingAnswer => "esperando respuesta",
            NodeState::Delivered => "entrega resultado",
            NodeState::WaitingMachine => "esperando máquina",
            NodeState::Done => "terminado",
            NodeState::Unclear => "no sé qué quiere",
        }
    }
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

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Node {
    pub session_id: String,
    pub short_id: String,
    pub name: Option<String>,
    pub cwd: Option<String>,
    pub pid: Option<i32>,
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
            daemon_detail: None,
            daemon_needs: None,
            daemon_updated_at: None,
            turn: 0,
            turn_signal: None,
            classified: None,
            usage_limited: false,
            last_seen: at,
            acknowledged_at: None,
        }
    }

    pub fn has_news(&self, now: Millis) -> bool {
        let finished = matches!(self.state, NodeState::Delivered | NodeState::Done | NodeState::Unclear | NodeState::WaitingMachine);
        let said_at = self.last_message_at.unwrap_or(self.state_since).max(self.state_since);
        let recent = now - said_at < NEWS_WINDOW_MS;
        finished && recent && !self.is_orchestrator && self.acknowledged_at.is_none_or(|a| a < said_at)
    }

    pub fn project(&self) -> String {
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
            .unwrap_or("otros")
            .to_string()
    }

    pub fn display_name(&self) -> &str {
        self.name.as_deref().unwrap_or(&self.short_id)
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
            ReportKind::Notice => "aviso",
            ReportKind::Document => "documento",
            ReportKind::Progress => "progreso",
            ReportKind::Final => "terminado",
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
}

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug)]
#[serde(rename_all = "snake_case")]
pub enum MessageFrom {
    David,
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
