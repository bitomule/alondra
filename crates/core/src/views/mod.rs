pub mod accounts;
pub mod action_outcome;
mod capabilities;
pub mod action_id;
pub mod allow_always;
pub mod allow_for_task;
mod card;
mod common;
pub mod components;
mod details;
mod task_node;
pub mod now_fact;
mod home;
pub mod mcp_servers;
pub mod id;
pub mod live;
pub mod resolves;
pub mod review_export;
mod merge_method;
mod branch_update;
mod messages;
pub mod metrics;
mod misc;
mod pacing;
pub mod pr_images;
pub mod pr_status;
mod schedules;
mod sidebar;
mod status;
mod summary;
mod task_state;
mod tasks;
#[cfg(test)]
mod tests;
#[cfg(test)]
mod card_contract_tests;
pub mod text;

pub use components::*;
pub use capabilities::{ClientCapabilities, InvalidCapabilities};
pub use card::answer_label;
pub use messages::{Outcome, OutcomeKind, refusal_text, send_anyway, transport_failure, unknown_outcome};
pub use resolves::{pr_presentation, resolves_card};
pub use live::*;
pub use text::Lang;
/// The newest views protocol this build speaks.
pub const VIEWS_PROTOCOL: u32 = 1;
/// The oldest one a daemon still serves: kept at least two releases after `VIEWS_PROTOCOL` rises.
pub const VIEWS_PROTOCOL_MIN: u32 = 1;
pub const VIEWS_PROTOCOLS: ProtocolRange = ProtocolRange { min: VIEWS_PROTOCOL_MIN, max: VIEWS_PROTOCOL };

impl Service {
    /// A daemon of `version` serving every protocol this build knows.
    pub fn serving(version: &str) -> Self {
        Service { version: version.to_owned(), views_protocol: VIEWS_PROTOCOLS, capabilities: ClientCapabilities::SUPPORTED.iter().map(|name| (*name).to_owned()).collect() }
    }
}

use crate::domain::{DomainState, InboxItem, ReadIndex, Task, TaskId, TaskReading};
use sha2::{Digest, Sha256};

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
        use text::t;
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
    pub(super) fn waiting(&self) -> &[InboxItem] {
        self.index.cards()
    }
    pub(super) fn reading(&self, task: &TaskId) -> Option<&TaskReading> {
        self.index.task(task)
    }
    pub(super) fn attention(&self, task: &Task) -> crate::domain::Attention {
        self.reading(&task.id).map_or(crate::domain::Attention::Idle, |read| read.attention.clone())
    }
    pub(super) fn waits_for_review(&self, task: &TaskId) -> bool {
        self.reading(task).is_some_and(|read| read.waits_for_review)
    }
    /// The cards a task waits on the user with, in the queue's order.
    pub(super) fn waiting_cards_of(&self, task: &TaskId) -> impl Iterator<Item = &InboxItem> {
        self.reading(task).into_iter().flat_map(|read| &read.waiting).flat_map(|id| {
            let cards = self.index.cards_of(id);
            if cards.is_empty() { self.index.snoozed_card(id).map(|entry| std::slice::from_ref(&entry.card)).unwrap_or_default() } else { cards }
        })
    }
    /// The task a node works for: its latest open one, or else its latest.
    pub(super) fn node_task(&self, agent: &crate::domain::AgentId) -> Option<&Task> {
        self.index.task_of_node(agent).and_then(|id| self.domain.tasks.get(id))
    }
    /// The orchestrator has neither split the request into tasks nor closed it.
    pub(super) fn is_defining(&self, request: &crate::domain::Request) -> bool {
        !request.closed && self.index.tasks_of_request(&request.id).is_empty()
    }
}
/// The app icon badge: the cards of the Waiting for You queue that ask the user for something, as
/// `Status::badge` counts them; a card that is only read never counts.
pub fn inbox_badge(index: &crate::domain::ReadIndex) -> u32 {
    index.cards().iter().filter(|item| home::asks(item)).count() as u32
}
/// The counts the status header and the phone's Live Activity show, so they never differ: the index's, with a paused
/// task the gate queues counted as queued, as the list says it.
pub fn activity_counts(context: &RenderContext<'_>) -> crate::domain::ActivityCounts {
    let mut counts = context.index.activity();
    let queued = context.domain.tasks.values()
        .filter(|task| task_state::tally(task, context) == Some(crate::domain::Tally::Queued)
            && context.reading(&task.id).and_then(crate::domain::TaskReading::tally) == Some(crate::domain::Tally::Paused))
        .count() as u32;
    counts.paused -= queued;
    counts.queued += queued;
    counts
}
/// The cards of the Waiting for You queue, by the id a push names them with (`alondra.card`).
pub fn waiting_cards(index: &crate::domain::ReadIndex) -> Vec<String> {
    index.waiting().iter().map(|item| card::public_id(item).to_owned()).collect()
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
    IssuePicker, Search, Machine, McpServers, Accounts, ModelProfile, TaskSelection, Metrics);

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
        } else if key == mcp_servers::KEY {
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

pub fn build(key: &str, context: RenderContext<'_>) -> Result<View, BuildError> {
    let tail = || key.split_once(':').map_or("", |(_, id)| id);
    let mut view = match Family::of(key).ok_or(BuildError::InvalidKey)? {
        Family::Home => home::build(key, &context)?,
        Family::Card => card::build(tail(), &context)?,
        Family::Document => details::document(tail(), &context)?,
        Family::PullRequest => details::pull_request(tail(), &context)?,
        Family::Sidebar => sidebar::build(&context)?,
        Family::Tasks => tasks::list(key, &context)?,
        Family::Task => tasks::detail(tail(), &context)?,
        Family::Request => tasks::request(tail(), &context)?,
        Family::Orchestrator => misc::orchestrator(key, &context),
        Family::Schedules => schedules::list(&context),
        Family::Schedule => schedules::editor(key, &context)?,
        Family::NewRequest => misc::new_request(key, &context)?,
        Family::IssuePicker => misc::issue_picker(key, &context)?,
        Family::Search => misc::search(key, &context)?,
        Family::Machine => misc::machine(&context),
        Family::McpServers => mcp_servers::build(&context),
        Family::Accounts => accounts::list(&context),
        Family::ModelProfile => model_controls::profile(tail(), &context)?,
        Family::TaskSelection => model_controls::task(tail(), &context)?,
        Family::Metrics => metrics::build(key, &context)?,
    };
    view.version = version(&view);
    Ok(view)
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

pub fn push_actions(
    read: &crate::domain::ReadSnapshot,
    card_id: &str,
    lang: Lang,
) -> Result<Vec<PushAction>, BuildError> {
    Ok(push_content(read, card_id, lang)?.actions)
}

/// How loudly a card interrupts: a problem, or a blocking permission or manual step, now; a
/// decision, review, test or input when the user looks; a notice or a proposal quietly.
fn push_level(kind: crate::domain::CardKind, blocking: bool) -> &'static str {
    use crate::domain::CardKind::*;
    match kind {
        Problem => "time-sensitive",
        Authorize | Do if blocking => "time-sensitive",
        Authorize | Do | Decide | Review | Test | Provide => "active",
        Know | Propose => "passive",
    }
}

/// The answers a notification carries besides its text reply: the card's first three.
const PUSHED_ANSWERS: usize = 3;

pub fn push_content(
    read: &crate::domain::ReadSnapshot,
    card_id: &str,
    lang: Lang,
) -> Result<PushContent, BuildError> {
    let domain = read.state().as_ref();
    let live = LiveSnapshot::default();
    let view = build(&format!("card:{card_id}"), RenderContext {
        domain, index: read.index(), live: &live, client: Client::Ios, lang, transport: Transport::Relay,
        cursor: "push", rendered_at: "",
    })?;
    let interaction = domain.interactions.get(&crate::domain::InteractionId(card_id.into()));
    let interruption_level = interaction.map_or("active", |i| {
        let quiet = i.author == crate::domain::Author::System && !i.blocking && i.card.kind == crate::domain::CardKind::Decide;
        if quiet { "passive" } else { push_level(i.card.kind, i.blocking) }
    });
    let thread = interaction.map(|i| match &i.scope {
        crate::domain::Scope::Task { task_id } => task_id.0.clone(),
        crate::domain::Scope::Orchestrator { request_id: Some(request_id) } => request_id.0.clone(),
        crate::domain::Scope::Orchestrator { request_id: None } => "orchestrator".to_owned(),
    });
    let Some(card) = view.sections.iter().flat_map(|section| &section.items).find_map(|item| {
        if let Item::Card(card) = item { Some(card) } else { None }
    }) else { return Ok(PushContent { alert: None, actions: vec![], interruption_level, thread }) };
    let mut alert = push_alert(card);
    if let (Some(alert), Some(group)) = (alert.as_mut(), interaction.and_then(|i| i.card.group.as_ref())) {
        let position = card::group_position(group, lang);
        alert.body = Some(match alert.body.take() {
            Some(body) => format!("{position} · {body}"),
            None => position,
        });
    }
    let mut pushed = Vec::new();
    for action in &card.actions {
        if action.id.is_empty() || action.link.is_some() || !action.enabled || action.confirm.is_some() ||
            action.comments.is_some() { continue; }
        if !push_supported_option(domain, action) { continue; }
        if pushed.len() == PUSHED_ANSWERS { break; }
        let (text, placeholder) = push_text(&action.input);
        pushed.push(PushAction { id: action.id.clone(), option: action.option_id.clone(),
            label: action.label.clone(), text, placeholder, open: false });
    }
    if let Some(reply) = card.reply.as_ref().filter(|reply| reply.enabled) {
        let (_, placeholder) = push_text(&reply.input);
        pushed.push(PushAction { id: reply.id.clone(), option: None, label: reply.label.clone(),
            text: true, placeholder, open: false });
    }
    Ok(PushContent { alert, actions: pushed, interruption_level, thread })
}

fn push_alert(card: &Card) -> Option<PushAlert> {
    let title = one_line(&card.title, PUSH_TITLE_CHARS)?;
    let body = card.why.as_deref().and_then(|why| one_line(why, PUSH_BODY_CHARS))
        .or_else(|| card.eyebrow.as_ref().and_then(|label| one_line(&label.text, PUSH_BODY_CHARS)));
    Some(PushAlert { title, body })
}

fn one_line(text: &str, limit: usize) -> Option<String> {
    let line = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if line.is_empty() { return None; }
    if line.chars().count() <= limit { return Some(line); }
    let cut: String = line.chars().take(limit - 1).collect();
    Some(format!("{}…", cut.trim_end()))
}

/// An option is a button, so any of them can be pressed from a notification.
fn push_supported_option(domain: &DomainState, action: &Action) -> bool {
    use crate::domain::{InteractionId, InteractionPayload};
    let Ok(id) = action_id::ActionId::parse(&action.id) else { return false };
    if id.verb != action_id::Verb::Ans { return true; }
    let Some(option_id) = action.option_id.as_deref() else { return false };
    let Some(interaction) = domain.interactions.get(&InteractionId(id.subject)) else { return false };
    match &interaction.payload {
        InteractionPayload::Question { options, .. } | InteractionPayload::ActionRequest { options, .. } =>
            options.iter().any(|source| source.id == option_id),
        _ => false,
    }
}

fn push_text(input: &Input) -> (bool, Option<String>) {
    match input {
        Input::None | Input::Unsupported => (false, None),
        Input::Text { placeholder, .. } => (true, Some(placeholder.clone())),
    }
}
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

pub mod model_controls;
