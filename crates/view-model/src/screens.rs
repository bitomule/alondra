//! The screens port: what a client renders, asked of whichever crate the root registered.
use crate::text::{t, Lang};
use crate::{ClientCapabilities, LiveSnapshot, ProtocolRange, Service, View, Client, Transport, mcp, metrics};
use alondra_domain_model::{ActivityCounts, DomainState, InboxItem, Interaction, QuestionOption, ReadIndex, ReadSnapshot, Task, TaskId, TaskReading};
use sha2::{Digest, Sha256};

/// Everything a client sees, rendered from the committed domain and the live snapshot. Only the
/// root knows which crate implements it: the screens are a leaf every other crate reaches through
/// this trait, so editing a screen rebuilds that crate and the binaries alone.
pub trait Screens: Send + Sync {
    /// The view `key` names.
    fn build(&self, key: &str, context: RenderContext<'_>) -> Result<View, BuildError>;
    /// The app icon badge: the cards of the Waiting for You queue that ask the user for something.
    fn inbox_badge(&self, index: &ReadIndex) -> u32;
    /// The counts the status header and the phone's Live Activity show.
    fn activity_counts(&self, context: &RenderContext<'_>) -> ActivityCounts;
    /// The cards of the Waiting for You queue, by the id a push names them with.
    fn waiting_cards(&self, index: &ReadIndex) -> Vec<String>;
    /// What a notification about the card `card_id` says and offers.
    fn push_content(&self, read: &ReadSnapshot, card_id: &str, lang: Lang) -> Result<PushContent, BuildError>;
    /// The words an answer reads with once the user chose `option`.
    fn answer_label(&self, state: &DomainState, interaction: &Interaction, option: &QuestionOption, lang: Lang) -> String;
}

/// The newest views protocol this build speaks.
pub const VIEWS_PROTOCOL: u32 = 1;
/// The oldest one a daemon still serves: kept at least two releases after `VIEWS_PROTOCOL` rises.
pub const VIEWS_PROTOCOL_MIN: u32 = 1;
pub const VIEWS_PROTOCOLS: ProtocolRange = ProtocolRange { min: VIEWS_PROTOCOL_MIN, max: VIEWS_PROTOCOL };

/// A daemon of `version` serving every protocol this build knows.
pub fn service(version: &str) -> Service {
    Service { version: version.to_owned(), views_protocol: VIEWS_PROTOCOLS, capabilities: ClientCapabilities::SUPPORTED.iter().map(|name| (*name).to_owned()).collect() }
}

#[derive(Clone, Copy, Debug)]
pub struct RenderContext<'a> {
    pub domain: &'a DomainState,
    /// The read index of `domain`'s revision: everything a row reads of the domain beyond its own
    /// record, decided once per commit, so no view walks the domain once per row.
    pub index: &'a ReadIndex,
    pub live: &'a LiveSnapshot,
    pub client: Client,
    pub lang: Lang,
    pub transport: Transport,
    pub cursor: &'a str,
    pub rendered_at: &'a str,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BuildError {
    InvalidKey,
    InvalidActionId,
    NotFound,
    UnsupportedClient,
}
/// The view a card key renders once the card no longer waits for the user: no card, only the
/// short notice in `empty` that says why, for the client to show as it closes the screen.
pub const SETTLED_VIEW: &str = "settled_card";
/// Why a card reached by its link is gone: answered, withdrawn, accepted, read, or its task ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Settled {
    Answered,
    Withdrawn,
    Accepted,
    Read,
    TaskOver,
    Merged,
    ClosedUnmerged,
}
impl Settled {
    /// The short notice a client shows as it closes the card.
    pub fn notice(self, lang: Lang) -> &'static str {
        match self {
            Self::Answered => t!(lang, "Already answered", "Ya respondida"),
            Self::Withdrawn => t!(lang, "No longer needed", "Ya no hace falta"),
            Self::Accepted => t!(lang, "Already accepted", "Ya aceptada"),
            Self::Read => t!(lang, "Already read", "Ya leída"),
            Self::TaskOver => t!(lang, "Its task is over", "Su tarea ya terminó"),
            Self::Merged => t!(lang, "Already merged", "Ya tiene merge"),
            Self::ClosedUnmerged => t!(lang, "Closed without merging", "Cerrada sin hacer merge"),
        }
    }
}
impl RenderContext<'_> {
    /// Waiting for You, in its order, each card with its buttons.
    pub fn waiting(&self) -> &[InboxItem] {
        self.index.cards()
    }
    pub fn reading(&self, task: &TaskId) -> Option<&TaskReading> {
        self.index.task(task)
    }
    pub fn attention(&self, task: &Task) -> alondra_domain_model::Attention {
        self.reading(&task.id).map_or(alondra_domain_model::Attention::Idle, |read| read.attention.clone())
    }
    pub fn waits_for_review(&self, task: &TaskId) -> bool {
        self.reading(task).is_some_and(|read| read.waits_for_review)
    }
    /// The cards a task waits on the user with, in the queue's order.
    pub fn waiting_cards_of(&self, task: &TaskId) -> impl Iterator<Item = &InboxItem> {
        self.reading(task).into_iter().flat_map(|read| &read.waiting).flat_map(|id| {
            let cards = self.index.cards_of(id);
            if cards.is_empty() { self.index.snoozed_card(id).map(|entry| std::slice::from_ref(&entry.card)).unwrap_or_default() } else { cards }
        })
    }
    /// The task a node works for: its latest open one, or else its latest.
    pub fn node_task(&self, agent: &alondra_domain_model::AgentId) -> Option<&Task> {
        self.index.task_of_node(agent).and_then(|id| self.domain.tasks.get(id))
    }
    /// The orchestrator has neither split the request into tasks nor closed it.
    pub fn is_defining(&self, request: &alondra_domain_model::Request) -> bool {
        !request.closed && self.index.tasks_of_request(&request.id).is_empty()
    }
}
macro_rules! families {
    ($($family:ident),* $(,)?) => {
        /// Every kind of view `build` renders, each named by what its keys start with.
        #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
        pub enum Family { $($family),* }
        impl Family {
            pub const ALL: &[Family] = &[$(Family::$family),*];
        }
    };
}
families!(Home, Card, Document, PullRequest, Sidebar, Tasks, Task, Request, Orchestrator, Schedules, Schedule, NewRequest,
    IssuePicker, Search, Machine, McpServers, Accounts, ModelProfile, TaskSelection, Metrics, TaskSpecs);

impl Family {
    /// The family a key belongs to, or none for a key no view answers.
    pub fn of(key: &str) -> Option<Family> {
        let listed = |name: &str| key == name || key.strip_prefix(name).is_some_and(|rest| rest.starts_with('?'));
        Some(if key == "home" || key.starts_with("home?focus=") {
            Family::Home
        } else if key.starts_with("card:") {
            Family::Card
        } else if key.starts_with("document:") {
            Family::Document
        } else if key.starts_with("pull_request:") {
            Family::PullRequest
        } else if key == "sidebar" {
            Family::Sidebar
        } else if listed("tasks") {
            Family::Tasks
        } else if key.starts_with("task_specs:") {
            Family::TaskSpecs
        } else if key.starts_with("task_selection:") {
            Family::TaskSelection
        } else if key.starts_with("model_profile:") {
            Family::ModelProfile
        } else if key.starts_with("task:") {
            Family::Task
        } else if key.starts_with("request:") {
            Family::Request
        } else if key == "orchestrator" {
            Family::Orchestrator
        } else if key == "schedules" {
            Family::Schedules
        } else if key.starts_with("schedule:") {
            Family::Schedule
        } else if listed("new_request") {
            Family::NewRequest
        } else if listed("issue_picker") {
            Family::IssuePicker
        } else if listed("search") {
            Family::Search
        } else if key == "machine" {
            Family::Machine
        } else if key == mcp::SERVERS_KEY {
            Family::McpServers
        } else if key == "accounts" {
            Family::Accounts
        } else if listed(metrics::KEY) {
            Family::Metrics
        } else {
            return None;
        })
    }
}

/// Notification actions are the same opaque actions rendered on an iOS card. The
/// notification extension only has room for four controls and can collect at most text.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct PushAction {
    pub id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub option: Option<String>,
    pub label: String,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub text: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub placeholder: Option<String>,
    /// Opens the card in the app instead of acting from the notification.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub open: bool,
}

/// What the lock screen shows for a card: what the user must do over why.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PushAlert {
    pub title: String,
    pub body: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PushContent {
    pub alert: Option<PushAlert>,
    pub actions: Vec<PushAction>,
    /// iOS `interruption-level`: `time-sensitive`, `active` or `passive`.
    pub interruption_level: &'static str,
    /// iOS `thread-id`: the notifications of one Task group together, then those of one Request.
    pub thread: Option<String>,
}

pub const PUSH_TITLE_CHARS: usize = 80;
pub const PUSH_BODY_CHARS: usize = 180;

pub fn version<T: serde::Serialize>(value: &T) -> String {
    let mut json = serde_json::to_value(value).expect("view serialization");
    if let Some(object) = json.as_object_mut() {
        object.remove("version");
        object.remove("cursor");
        for card in object.get_mut("first_cards").and_then(|cards| cards.as_array_mut()).into_iter().flatten() {
            card.as_object_mut().map(|card| card.remove("cursor"));
        }
    }
    let bytes = serde_json::to_vec(&json).expect("canonical JSON");
    let digest = Sha256::digest(bytes);
    let hex = digest[..8]
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    format!("v{hex}")
}

