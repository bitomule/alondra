pub mod action_id;
mod card;
mod common;
pub mod components;
mod details;
mod fleet;
mod home;
pub mod id;
pub mod live;
pub mod review_export;
mod misc;
mod schedules;
mod sidebar;
mod status;
mod summary;
mod tasks;
#[cfg(test)]
mod tests;
pub mod text;

pub use components::*;
pub use live::*;
pub use text::Lang;
pub const VIEWS_PROTOCOL: u32 = 1;

use crate::domain_v2::DomainState;
use sha2::{Digest, Sha256};

#[derive(Clone, Copy, Debug)]
pub struct RenderContext<'a> {
    pub domain: &'a DomainState,
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
/// The app icon badge: the same count as the Waiting for You queue and `Status::badge`.
pub fn inbox_badge(domain: &crate::domain_v2::DomainState) -> u32 {
    home::waiting(&domain.projection()).len() as u32
}
/// The cards of the Waiting for You queue, by the id a push names them with (`alondra.card`).
pub fn waiting_cards(domain: &crate::domain_v2::DomainState) -> Vec<String> {
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
fn push_level(kind: crate::domain_v2::CardKind, blocking: bool) -> &'static str {
    use crate::domain_v2::CardKind::*;
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
        domain, live: &live, client: Client::Ios, lang, transport: Transport::Relay,
        cursor: "push", rendered_at: "",
    })?;
    let interaction = domain.interactions.get(&crate::domain_v2::InteractionId(card_id.into()));
    let interruption_level = interaction.map_or("active", |i| push_level(i.card.kind, i.blocking));
    let thread = interaction.map(|i| match &i.scope {
        crate::domain_v2::Scope::Task { task_id } => task_id.0.clone(),
        crate::domain_v2::Scope::Orchestrator { request_id: Some(request_id) } => request_id.0.clone(),
        crate::domain_v2::Scope::Orchestrator { request_id: None } => "orchestrator".to_owned(),
    });
    let Some(card) = view.sections.iter().flat_map(|section| &section.items).find_map(|item| {
        if let Item::Card(card) = item { Some(card) } else { None }
    }) else { return Ok(PushContent { alert: None, actions: vec![], interruption_level, thread }) };
    let alert = push_alert(card);
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
    use crate::domain_v2::{InputKind, InteractionId, InteractionPayload};
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
