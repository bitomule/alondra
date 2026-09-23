use serde::{Deserialize, Serialize};

use crate::model::{Answer, Message, Millis, NodeState, Question, Report};

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Default)]
pub struct HookEvent {
    pub session_id: String,
    pub event: String,
    #[serde(default)]
    pub tool_name: Option<String>,
    #[serde(default)]
    pub tool_use_id: Option<String>,
    #[serde(default)]
    pub tool_summary: Option<String>,
    #[serde(default)]
    pub last_message: Option<String>,
    #[serde(default)]
    pub notification_type: Option<String>,
    #[serde(default)]
    pub error_type: Option<String>,
    #[serde(default)]
    pub cwd: Option<String>,
    #[serde(default)]
    pub agent_id: Option<String>,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Default)]
pub struct DaemonObservation {
    pub session_id: String,
    pub short_id: String,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub cwd: Option<String>,
    #[serde(default)]
    pub pid: Option<i32>,
    #[serde(default)]
    pub state: Option<String>,
    #[serde(default)]
    pub tempo: Option<String>,
    #[serde(default)]
    pub detail: Option<String>,
    #[serde(default)]
    pub needs: Option<String>,
    #[serde(default)]
    pub updated_at: Option<i64>,
    #[serde(default)]
    pub last_text: Option<String>,
    #[serde(default)]
    pub last_text_at: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tasks: Option<u32>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub task_kinds: Vec<String>,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Event {
    Hook(HookEvent),
    DaemonSeen(DaemonObservation),
    ProcessExited { session_id: String, pid: i32 },
    NodeRemoved { session_id: String },
    Acknowledged { session_id: String },
    MarkedOrchestrator { session_id: String },
    QuestionAsked(Question),
    QuestionAnswered { id: u64, answer: Answer },
    QuestionDelivered { id: u64 },
    QuestionClosed { id: u64, status: crate::model::QuestionStatus },
    ReportPosted(Report),
    ReportRead { id: u64 },
    MessageQueued(Message),
    MessageHanded { id: u64 },
    Classified { session_id: String, turn: u64, state: NodeState, confidence: Option<f64> },
    WaitingMachine { session_id: String, reason: String },
    Pruned { before: Millis },
    Clock {
        #[serde(default)]
        machine_strained: bool,
    },
}

impl Event {
    pub fn session_id(&self) -> Option<&str> {
        match self {
            Event::Hook(h) => Some(&h.session_id),
            Event::DaemonSeen(d) => Some(&d.session_id),
            Event::ProcessExited { session_id, .. }
            | Event::NodeRemoved { session_id }
            | Event::Acknowledged { session_id }
            | Event::MarkedOrchestrator { session_id }
            | Event::Classified { session_id, .. }
            | Event::WaitingMachine { session_id, .. } => Some(session_id),
            Event::QuestionAsked(q) => Some(&q.session_id),
            Event::ReportPosted(r) => Some(&r.session_id),
            Event::MessageQueued(m) => Some(&m.session_id),
            _ => None,
        }
    }

    pub fn kind(&self) -> &'static str {
        match self {
            Event::Hook(_) => "hook",
            Event::DaemonSeen(_) => "daemon_seen",
            Event::ProcessExited { .. } => "process_exited",
            Event::NodeRemoved { .. } => "node_removed",
            Event::Acknowledged { .. } => "acknowledged",
            Event::MarkedOrchestrator { .. } => "marked_orchestrator",
            Event::QuestionAsked(_) => "question_asked",
            Event::QuestionAnswered { .. } => "question_answered",
            Event::QuestionDelivered { .. } => "question_delivered",
            Event::QuestionClosed { .. } => "question_closed",
            Event::ReportPosted(_) => "report_posted",
            Event::ReportRead { .. } => "report_read",
            Event::MessageQueued(_) => "message_queued",
            Event::MessageHanded { .. } => "message_handed",
            Event::Classified { .. } => "classified",
            Event::WaitingMachine { .. } => "waiting_machine",
            Event::Pruned { .. } => "pruned",
            Event::Clock { .. } => "clock",
        }
    }
}
