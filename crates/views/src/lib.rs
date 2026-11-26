#[cfg(test)]
#[path = "../../test_support.rs"]
mod test_support;
pub mod accounts;
pub use alondra_view_model::{action_id, components, live, now_fact, pr_images};
pub use alondra_view_model::{action_outcome, resolves, review_export};
use alondra_view_model::messages;
pub mod allow_always;
pub mod allow_for_task;
mod card;
mod common;
mod details;
mod task_node;
mod home;
pub mod mcp_servers;
mod merge_method;
mod branch_update;
pub mod metrics;
mod misc;
mod pacing;
pub mod pr_status;
mod schedules;
mod sidebar;
mod status;
mod summary;
mod task_state;
mod tasks;
#[cfg(test)]
mod domain_fixtures;
#[cfg(test)]
mod tests;
#[cfg(test)]
mod card_contract_tests;
#[cfg(test)]
mod capabilities_tests;
pub mod text;

pub use components::*;
pub use alondra_view_model::capabilities::{ClientCapabilities, InvalidCapabilities};
pub use card::answer_label;
pub use messages::{Outcome, OutcomeKind, refusal_text, scheduled_message_stamp, send_anyway, transport_failure, unknown_outcome};
pub use resolves::{pr_presentation, resolves_card};
pub use live::*;
pub use text::Lang;
pub use alondra_view_model::screens::*;
use alondra_domain_rules::{DomainState, ReadIndex};
use std::sync::Arc;

/// The screens this crate renders, as the port the rest of Alondra asks for them.
pub struct Views;

impl Screens for Views {
    fn build(&self, key: &str, context: RenderContext<'_>) -> Result<View, BuildError> {
        build(key, context)
    }
    fn inbox_badge(&self, index: &ReadIndex) -> u32 {
        inbox_badge(index)
    }
    fn activity_counts(&self, context: &RenderContext<'_>) -> alondra_domain_rules::ActivityCounts {
        activity_counts(context)
    }
    fn waiting_cards(&self, index: &ReadIndex) -> Vec<String> {
        waiting_cards(index)
    }
    fn push_content(&self, read: &alondra_domain_rules::ReadSnapshot, card_id: &str, lang: Lang) -> Result<PushContent, BuildError> {
        push_content(read, card_id, lang)
    }
    fn answer_label(&self, state: &DomainState, interaction: &alondra_domain_rules::Interaction, option: &alondra_domain_rules::QuestionOption, lang: Lang) -> String {
        answer_label(state, interaction, option, lang)
    }
}

/// Registers the screens as the implementation of the port `Screens`.
pub fn wire(registry: &alondra_engine_ports::Registry) {
    registry.provide::<dyn Screens>(Arc::new(Views));
}

/// The app icon badge: the cards of the Waiting for You queue that ask the user for something, as
/// `Status::badge` counts them; a card that is only read never counts.
pub fn inbox_badge(index: &alondra_domain_rules::ReadIndex) -> u32 {
    index.cards().iter().filter(|item| home::asks(item)).count() as u32
}
/// The counts the status header and the phone's Live Activity show, so they never differ: the index's, with a paused
/// task the gate queues counted as queued, as the list says it.
pub fn activity_counts(context: &RenderContext<'_>) -> alondra_domain_rules::ActivityCounts {
    let mut counts = context.index.activity();
    let queued = context.domain.tasks.values()
        .filter(|task| task_state::tally(task, context) == Some(alondra_domain_rules::Tally::Queued)
            && context.reading(&task.id).and_then(alondra_domain_rules::TaskReading::tally) == Some(alondra_domain_rules::Tally::Paused))
        .count() as u32;
    counts.paused -= queued;
    counts.queued += queued;
    counts
}
/// The cards of the Waiting for You queue, by the id a push names them with (`alondra.card`).
pub fn waiting_cards(index: &alondra_domain_rules::ReadIndex) -> Vec<String> {
    index.waiting().iter().map(|item| card::public_id(item).to_owned()).collect()
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
        Family::TaskSpecs => specs::build(tail(), &context)?,
        Family::Metrics => metrics::build(key, &context)?,
    };
    view.version = version(&view);
    Ok(view)
}

pub fn push_actions(
    read: &alondra_domain_rules::ReadSnapshot,
    card_id: &str,
    lang: Lang,
) -> Result<Vec<PushAction>, BuildError> {
    Ok(push_content(read, card_id, lang)?.actions)
}

/// How loudly a card interrupts: a problem, or a blocking permission or manual step, now; a
/// decision, review, test or input when the user looks; a notice or a proposal quietly.
fn push_level(kind: alondra_domain_rules::CardKind, blocking: bool) -> &'static str {
    use alondra_domain_rules::CardKind::*;
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
    read: &alondra_domain_rules::ReadSnapshot,
    card_id: &str,
    lang: Lang,
) -> Result<PushContent, BuildError> {
    let domain = read.state().as_ref();
    let live = LiveSnapshot::default();
    let view = build(&format!("card:{card_id}"), RenderContext {
        domain, index: read.index(), live: &live, client: Client::Ios, lang, transport: Transport::Relay,
        cursor: "push", rendered_at: "",
    })?;
    let interaction = domain.interactions.get(&alondra_domain_rules::InteractionId(card_id.into()));
    let interruption_level = interaction.map_or("active", |i| {
        let quiet = i.author == alondra_domain_rules::Author::System && !i.blocking && i.card.kind == alondra_domain_rules::CardKind::Decide;
        if quiet { "passive" } else { push_level(i.card.kind, i.blocking) }
    });
    let thread = interaction.map(|i| match &i.scope {
        alondra_domain_rules::Scope::Task { task_id } => task_id.0.clone(),
        alondra_domain_rules::Scope::Orchestrator { request_id: Some(request_id) } => request_id.0.clone(),
        alondra_domain_rules::Scope::Orchestrator { request_id: None } => "orchestrator".to_owned(),
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
    use alondra_domain_rules::{InteractionId, InteractionPayload};
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
pub mod model_controls;
pub mod specs;
