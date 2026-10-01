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

pub fn push_actions(
    domain: &DomainState,
    card_id: &str,
    lang: Lang,
) -> Result<Vec<PushAction>, BuildError> {
    let live = LiveSnapshot::default();
    let view = build(&format!("card:{card_id}"), RenderContext {
        domain, live: &live, client: Client::Ios, lang, transport: Transport::Relay,
        cursor: "push", rendered_at: "",
    })?;
    let Some(card) = view.sections.iter().flat_map(|section| &section.items).find_map(|item| {
        if let Item::Card(card) = item { Some(card) } else { None }
    }) else { return Ok(vec![]) };
    if let Some(hold) = domain.holds.get(&crate::domain_v2::InteractionId(card_id.into())) {
        return Ok(held_push_actions(*hold, card, lang));
    }
    let mut pushed = Vec::new();
    for action in &card.actions {
        if action.id.is_empty() || action.link.is_some() || !action.enabled ||
            action.confirm.is_some() ||
            (action.comments.is_some() && !action.id.starts_with("reply:")) { continue; }
        if action.options.is_empty() {
            let (text, placeholder) = push_text(&action.input);
            pushed.push(PushAction { id: action.id.clone(), option: None,
                label: action.label.clone(), text, placeholder, open: false });
        } else {
            for option in &action.options {
                if !push_supported_option(domain, action, option) { continue; }
                let (text, placeholder) = push_text(&option.input);
                pushed.push(PushAction { id: action.id.clone(), option: Some(option.id.clone()),
                    label: option.label.clone(), text, placeholder, open: false });
                if pushed.len() == 4 { break; }
            }
        }
        if pushed.len() == 4 { break; }
    }
    Ok(pushed)
}

/// 8ccd131's notification buttons: Allow and Deny answer a permission from the lock screen;
/// a plan offers Approve and Review, which opens the plan in the app.
fn held_push_actions(hold: crate::domain_v2::Hold, card: &Card, lang: Lang) -> Vec<PushAction> {
    let Some(answer) = card.actions.iter().find(|action| action.id.starts_with("ans:") && action.enabled) else {
        return vec![];
    };
    let button = |option: &ActionOption| PushAction { id: answer.id.clone(), option: Some(option.id.clone()),
        label: option.label.clone(), text: false, placeholder: None, open: false };
    match hold {
        crate::domain_v2::Hold::Permission => answer.options.iter().map(button).collect(),
        crate::domain_v2::Hold::Plan => answer.options.iter().filter(|option| hold.approves(&option.id)).map(button)
            .chain([PushAction { id: format!("open:{}", card.id), option: None,
                label: text::t!(lang, "Review", "Revisar").into(), text: false, placeholder: None, open: true }])
            .collect(),
        crate::domain_v2::Hold::Question => answer.options.iter().take(4).map(button).collect(),
        crate::domain_v2::Hold::Elicitation => answer.options.iter().map(|option| match &option.input {
            Input::Text { placeholder, .. } if option.description.is_some() =>
                PushAction { text: true, placeholder: Some(placeholder.clone()), ..button(option) },
            _ => button(option),
        }).collect(),
    }
}

fn push_supported_option(domain: &DomainState, action: &Action, option: &ActionOption) -> bool {
    use crate::domain_v2::{InputKind, InteractionId, InteractionPayload};
    let Ok(id) = action_id::ActionId::parse(&action.id) else { return false };
    if id.verb != action_id::Verb::Ans { return true; }
    let Some(interaction) = domain.interactions.get(&InteractionId(id.subject)) else { return false };
    let options = match &interaction.payload {
        InteractionPayload::Question { options, .. } | InteractionPayload::ActionRequest { options, .. } => options,
        _ => return false,
    };
    let Some(source) = options.iter().find(|source| source.id == option.id) else { return false };
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
