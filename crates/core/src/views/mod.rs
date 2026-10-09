pub mod action_id;
mod card;
mod common;
pub mod components;
mod details;
mod fleet;
mod home;
pub mod id;
pub mod live;
pub mod resolves;
pub mod review_export;
mod messages;
mod misc;
mod pacing;
mod schedules;
mod sidebar;
mod status;
mod summary;
mod task_state;
mod tasks;
#[cfg(test)]
mod tests;
pub mod text;

pub use components::*;
pub use card::answer_label;
pub use messages::{Outcome, OutcomeKind, refusal_text, send_anyway, transport_failure};
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
        Service { version: version.to_owned(), views_protocol: VIEWS_PROTOCOLS }
    }
}

use crate::domain::{DomainProjection, DomainState};
use sha2::{Digest, Sha256};

#[derive(Clone, Copy, Debug)]
pub struct RenderContext<'a> {
    pub domain: &'a DomainState,
    /// `domain.projection()`, computed once per render: it copies every interaction, so the
    /// rows of a view must never compute it again.
    pub projection: &'a DomainProjection,
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
        }
    }
}
/// The app icon badge: the same count as the Waiting for You queue and `Status::badge`.
pub fn inbox_badge(domain: &crate::domain::DomainState) -> u32 {
    home::waiting(&domain.projection()).len() as u32
}
/// The cards of the Waiting for You queue, by the id a push names them with (`alondra.card`).
pub fn waiting_cards(domain: &crate::domain::DomainState) -> Vec<String> {
    home::waiting(&domain.projection()).into_iter().map(|item| card::public_id(item).to_owned()).collect()
}
pub fn build(key: &str, context: RenderContext<'_>) -> Result<View, BuildError> {
    let mut view = if key == "home" || key.starts_with("home?focus=") {
        home::build(key, &context)?
    } else if let Some(id) = key.strip_prefix("card:") {
        card::build(id, &context)?
    } else if let Some(id) = key.strip_prefix("document:") {
        details::document(id, &context)?
    } else if let Some(id) = key.strip_prefix("pull_request:") {
        details::pull_request(id, &context)?
    } else if key == "sidebar" {
        sidebar::build(&context)?
    } else if key == "tasks" || key.starts_with("tasks?") {
        tasks::list(key, &context)?
    } else if let Some(id) = key.strip_prefix("task:") {
        tasks::detail(id, &context)?
    } else if let Some(id) = key.strip_prefix("request:") {
        tasks::request(id, &context)?
    } else if key == "fleet" {
        fleet::list(&context)
    } else if let Some(id) = key.strip_prefix("node:") {
        fleet::detail(id, &context)?
    } else if key == "schedules" {
        schedules::list(&context)
    } else if key.starts_with("schedule:") {
        schedules::editor(key, &context)?
    } else if key == "new_request" || key.starts_with("new_request?") {
        misc::new_request(key, &context)?
    } else if key == "issue_picker" || key.starts_with("issue_picker?") {
        misc::issue_picker(key, &context)?
    } else if key == "search" || key.starts_with("search?") {
        misc::search(key, &context)?
    } else if key == "machine" {
        misc::machine(&context)
    } else {
        return Err(BuildError::InvalidKey);
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
    domain: &DomainState,
    card_id: &str,
    lang: Lang,
) -> Result<Vec<PushAction>, BuildError> {
    Ok(push_content(domain, card_id, lang)?.actions)
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

pub fn push_content(
    domain: &DomainState,
    card_id: &str,
    lang: Lang,
) -> Result<PushContent, BuildError> {
    let live = LiveSnapshot::default();
    let view = build(&format!("card:{card_id}"), RenderContext {
        domain, projection: &domain.projection(), live: &live, client: Client::Ios, lang, transport: Transport::Relay,
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
    let title = one_line(card.need.as_deref().unwrap_or(&card.title), PUSH_TITLE_CHARS)?;
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

/// Only answers that need no more than one line of text can be given from a notification.
fn push_supported_option(domain: &DomainState, action: &Action) -> bool {
    use crate::domain::{InputKind, InteractionId, InteractionPayload};
    let Ok(id) = action_id::ActionId::parse(&action.id) else { return false };
    if id.verb != action_id::Verb::Ans { return true; }
    let Some(option_id) = action.option_id.as_deref() else { return false };
    let Some(interaction) = domain.interactions.get(&InteractionId(id.subject)) else { return false };
    let options = match &interaction.payload {
        InteractionPayload::Question { options, .. } | InteractionPayload::ActionRequest { options, .. } => options,
        _ => return false,
    };
    let Some(source) = options.iter().find(|source| source.id == option_id) else { return false };
    source.inputs.is_empty() || source.inputs.len() == 1 && source.inputs[0].kind == InputKind::Text
}

fn push_text(input: &Input) -> (bool, Option<String>) {
    match input {
        Input::None => (false, None),
        Input::Text { placeholder, .. } => (true, Some(placeholder.clone())),
    }
}
pub fn version<T: serde::Serialize>(value: &T) -> String {
    let mut json = serde_json::to_value(value).expect("view serialization");
    if let Some(object) = json.as_object_mut() {
        object.remove("version");
        object.remove("cursor");
    }
    let bytes = serde_json::to_vec(&json).expect("canonical JSON");
    let digest = Sha256::digest(bytes);
    let hex = digest[..8]
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    format!("v{hex}")
}
