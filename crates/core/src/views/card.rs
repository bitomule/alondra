//! One card per interaction: what the user must do (`need`), why, one line of content, at most
//! three buttons and a reply. The detail adds the whole content, its context and its history.

use super::{
    CardAction,
    Action, ActionOption, Attachment, AttachmentKind, BlockChip, BuildError, Card, Check, CheckState,
    Checks, Client, CommentSpec, ContentRef, ContentRow, ContextLine, Detail, Fact, Facts, Icon, Input, Item,
    Label, Lang, Link, LocalLink, Markdown, Notice, Presentation, RenderContext, Role, Row, SETTLED_VIEW, Settled, Stamp, Style,
    TimeFormat, Tone, VIEWS_PROTOCOL, View,
    action_id::{ActionId, Verb},
    summary,
    text::{t, tf},
};
use crate::domain::{
    AvailableAction, Author, CardKind, CardSpec, ClarificationReason, ContentKind as DomainContent,
    DeliveryTarget, DocumentRevisionId, DomainState, EvidenceKind, Hold, InboxItem, InboxSubject, Instruction, Interaction, InteractionId,
    InteractionPayload, InteractionSubject, MergeMethod, PrId, PrState, PullRequest, QuestionGroup, QuestionOption, Recipient, Scope, clip, fit_title,
};

const VISIBLE_ACTIONS: usize = 3;

pub(super) fn plain_id(id: &str) -> bool {
    super::id::valid(id)
}
pub(super) fn public_id(item: &InboxItem) -> &str {
    let InboxSubject::Interaction { interaction_id } = &item.subject;
    &interaction_id.0
}
pub(super) fn card_key(item: &InboxItem) -> String {
    format!("card:{}", public_id(item))
}

fn interaction_of<'a>(item: &InboxItem, domain: &'a DomainState) -> Option<&'a Interaction> {
    let InboxSubject::Interaction { interaction_id } = &item.subject;
    domain.interactions.get(interaction_id)
}
fn targets_of(interaction: &Interaction) -> &[DeliveryTarget] {
    match &interaction.payload {
        InteractionPayload::Delivery { targets, .. } => targets,
        _ => &[],
    }
}
fn pr_target<'a>(interaction: &Interaction, domain: &'a DomainState) -> Option<&'a PullRequest> {
    targets_of(interaction).iter().find_map(|target| match target {
        DeliveryTarget::PullRequest { pr_id, .. } => domain.pull_requests.get(pr_id),
        _ => None,
    })
}
/// The document revision a card is about: a delivery of one, or a question its node linked to one.
fn reviewed_revision<'a>(
    interaction: &'a Interaction,
) -> Option<&'a crate::domain::DocumentRevisionId> {
    targets_of(interaction)
        .iter()
        .find_map(|target| match target {
            DeliveryTarget::DocumentRevision { revision_id, .. } => Some(revision_id),
            _ => None,
        })
        .or(match &interaction.subject {
            Some(InteractionSubject::DocumentReview { revision_id, .. }) => Some(revision_id),
            _ => None,
        })
}
/// The first card of the inbox, in its order, among `cards` that `about` keeps.
fn first_queued<'a>(context: &'a RenderContext<'_>, cards: &[InteractionId], about: impl Fn(&Interaction) -> bool)
    -> Option<std::borrow::Cow<'a, InboxItem>> {
    let first = cards.iter()
        .filter_map(|id| Some((context.index.queue_position(id)?, context.domain.interactions.get(id)?)))
        .filter(|(_, i)| about(i))
        .min_by_key(|(at, _)| *at)?;
    Some(context.index.item(context.domain, first.1))
}
/// The inbox card that stands for a pull request, whichever way it is addressed.
pub(super) fn item_for_pr<'a>(context: &'a RenderContext<'_>, pr_id: &PrId) -> Option<std::borrow::Cow<'a, InboxItem>> {
    first_queued(context, context.index.cards_of_pr(pr_id), |i| {
        targets_of(i).iter().any(|target| matches!(target, DeliveryTarget::PullRequest { pr_id: id, .. } if id == pr_id))
    })
}
pub(super) fn item_for_revision<'a>(context: &'a RenderContext<'_>, revision_id: &DocumentRevisionId) -> Option<std::borrow::Cow<'a, InboxItem>> {
    first_queued(context, context.index.cards_of_document_revision(revision_id), |i| reviewed_revision(i) == Some(revision_id))
}

pub(super) fn build(id: &str, context: &RenderContext<'_>) -> Result<View, BuildError> {
    if !plain_id(id) {
        return Err(BuildError::InvalidKey);
    }
    let key = format!("card:{id}");
    let domain = context.domain;
    let direct = domain.interactions.get(&InteractionId(id.into())).filter(|i| i.recipient == Recipient::User);
    let found = direct.or_else(|| alias(context, id)).ok_or(BuildError::NotFound)?;
    let item = context.index.item(domain, found);
    let item = item.as_ref();
    if let Some(settled) = settled(item, found, domain) {
        return Ok(settled_view(key, settled, context));
    }
    let card = from_item(item, context, true)?;
    let title = card.title.clone();
    let mut sections = vec![super::common::section("card", None, vec![Item::Card(card)])];
    if let Some((title, points)) = plan_points(item, context) {
        sections.push(super::common::section("plan", Some(title), points));
    }
    match pr_sections(item, context) {
        Some(pr_sections) => sections.extend(pr_sections),
        None => {
            let content = content_items(item, context);
            if !content.is_empty() {
                sections.push(super::common::section("content", Some(t!(context.lang, "Content", "Contenido").into()), content));
            }
        }
    }
    let around = context_items(item, context);
    if !around.is_empty() {
        sections.push(super::common::section("context", Some(t!(context.lang, "Context", "Contexto").into()), around));
    }
    let history = history_rows(item, context);
    if !history.is_empty() {
        sections.push(super::common::section("history", Some(t!(context.lang, "History", "Historial").into()), history));
    }
    Ok(View {
        protocol: VIEWS_PROTOCOL,
        data_epoch: domain.data_epoch.0.clone(),
        view: "card".into(),
        key,
        client: context.client,
        version: String::new(),
        cursor: context.cursor.into(),
        title: Some(title),
        status: super::status::build(context),
        sections,
        empty: None,
        notices: vec![],
        service: None,
        first_cards: vec![],
    })
}

/// How many cards of the queue travel inside it: the one in front and the two behind it.
pub(super) const FIRST_CARDS: usize = 3;

/// The first cards of a queue, each the view its `card:<id>` key answers.
pub(super) fn first_cards<'a>(ids: impl IntoIterator<Item = &'a str>, context: &RenderContext<'_>) -> Result<Vec<View>, BuildError> {
    ids.into_iter().take(FIRST_CARDS).map(|id| {
        let mut view = build(id, context)?;
        view.version = super::version(&view);
        Ok(view)
    }).collect()
}

/// A settled card is never shown: its view carries only why it went, for the client to say as it
/// closes the screen.
fn settled_view(key: String, settled: Settled, context: &RenderContext<'_>) -> View {
    View {
        protocol: VIEWS_PROTOCOL,
        data_epoch: context.domain.data_epoch.0.clone(),
        view: SETTLED_VIEW.into(),
        key,
        client: context.client,
        version: String::new(),
        cursor: context.cursor.into(),
        title: None,
        status: super::status::build(context),
        sections: vec![],
        empty: Some(Notice { text: settled.notice(context.lang).into(), tone: Tone::Neutral, action: None }),
        notices: vec![],
        service: None,
        first_cards: vec![],
    }
}

/// Why a card no longer waits for the user: answered, withdrawn, accepted, read, or its task is
/// over. A card that still waits, or that went quiet another way, is none of these.
fn settled(item: &InboxItem, interaction: &Interaction, domain: &DomainState) -> Option<Settled> {
    if item.pending {
        return None;
    }
    let answered = match &interaction.payload {
        _ if interaction.withdrawn => return Some(Settled::Withdrawn),
        InteractionPayload::Question { answer, .. } | InteractionPayload::ActionRequest { answer, .. } => {
            answer.is_some().then_some(Settled::Answered)
        }
        InteractionPayload::Information { read_by } => (!read_by.is_empty()).then_some(Settled::Read),
        InteractionPayload::Delivery { acceptance, .. } if acceptance.as_ref().is_some_and(|acceptance| acceptance.valid) => {
            Some(Settled::Accepted)
        }
        InteractionPayload::Delivery { .. } => domain
            .handed_back(interaction)
            .filter(|instruction| *instruction != Instruction::Conflicts)
            .map(|_| Settled::Answered),
    };
    let task_over = || match &interaction.scope {
        Scope::Task { task_id } => domain.tasks.get(task_id).is_some_and(|task| task.state.terminal()).then_some(Settled::TaskOver),
        Scope::Orchestrator { .. } => None,
    };
    answered.or_else(task_over)
}

/// The card a pull request or a document revision is about, for old links to them: its
/// current delivery, else a question its node linked to it.
fn alias<'a>(context: &RenderContext<'a>, id: &str) -> Option<&'a Interaction> {
    let domain = context.domain;
    let mut named: Vec<&InteractionId> = context.index.cards_of_pr(&PrId(id.into())).iter()
        .chain(context.index.cards_of_document_revision(&DocumentRevisionId(id.into())))
        .collect();
    named.sort();
    named.dedup();
    let cards = || named.iter().filter_map(|card| domain.interactions.get(*card));
    let names = |target: &DeliveryTarget| match target {
        DeliveryTarget::PullRequest { pr_id, .. } => pr_id.0 == id,
        DeliveryTarget::DocumentRevision { revision_id, .. } => revision_id.0 == id,
        _ => false,
    };
    let delivery = cards().find(|interaction| {
        let current = match &interaction.scope {
            Scope::Task { task_id } => {
                domain.tasks.get(task_id).is_some_and(|t| t.final_presentation.as_ref() == Some(&interaction.id))
            }
            _ => false,
        };
        current && targets_of(interaction).iter().any(names)
    });
    delivery.or_else(|| {
        cards().find(|interaction| {
            interaction.recipient == Recipient::User
                && domain.open(interaction)
                && match &interaction.subject {
                    Some(InteractionSubject::PullRequest { pr_id }) => pr_id.0 == id,
                    Some(InteractionSubject::DocumentReview { revision_id, .. }) => revision_id.0 == id,
                    None => false,
                }
        })
    })
}

fn kind_label(kind: CardKind, lang: Lang) -> &'static str {
    match kind {
        CardKind::Decide => t!(lang, "Decide", "Decidir"),
        CardKind::Review => t!(lang, "Review", "Revisar"),
        CardKind::Authorize => t!(lang, "Authorize", "Autorizar"),
        CardKind::Know => t!(lang, "FYI", "Info"),
        CardKind::Provide => t!(lang, "Provide", "Aportar"),
        CardKind::Test => t!(lang, "Test", "Probar"),
        CardKind::Do => t!(lang, "Do", "Hacer"),
        CardKind::Problem => t!(lang, "Problem", "Problema"),
        CardKind::Propose => t!(lang, "Suggestion", "Sugerencia"),
    }
}
fn kind_icon(kind: CardKind, carries: Option<Icon>) -> Icon {
    if let Some(icon) = carries.filter(|_| kind == CardKind::Review) {
        return icon;
    }
    match kind {
        CardKind::Decide => Icon::Question,
        CardKind::Review => Icon::Document,
        CardKind::Authorize | CardKind::Problem => Icon::Warning,
        CardKind::Know | CardKind::Propose => Icon::Notice,
        CardKind::Provide => Icon::Attachment,
        CardKind::Test => Icon::Check,
        CardKind::Do => Icon::Action,
    }
}
fn kind_tone(kind: CardKind, pending: bool) -> Tone {
    if !pending {
        return Tone::Neutral;
    }
    match kind {
        CardKind::Problem => Tone::Critical,
        CardKind::Authorize | CardKind::Do => Tone::Warning,
        CardKind::Know => Tone::Neutral,
        CardKind::Propose => Tone::Info,
        CardKind::Decide | CardKind::Review | CardKind::Provide | CardKind::Test => Tone::Accent,
    }
}

fn node_name(item: &InboxItem, context: &RenderContext<'_>) -> String {
    match &item.agent_id {
        Some(agent_id) => super::common::agent_label(agent_id, context),
        None if item.task_id.is_some() => t!(context.lang, "this node", "este nodo").into(),
        None => "Alondra".into(),
    }
}

/// What a delivery or a card stands for in another card's chip: "PR #649", a document title, or
/// the card's own need.
fn target_label(domain: &DomainState, id: &InteractionId, lang: Lang) -> String {
    let Some(interaction) = domain.interactions.get(id) else { return id.0.clone() };
    if let Some(pr) = pr_target(interaction, domain) {
        return format!("PR #{}", pr.number);
    }
    for target in targets_of(interaction) {
        if let DeliveryTarget::DocumentRevision { document_id, .. } = target {
            if let Some(document) = domain.documents.get(document_id) {
                return document.title.clone();
            }
        }
    }
    clip(&shown_need(interaction, domain, lang), 50)
}

/// What `interaction` asks, as its card shows it to the viewer.
pub(super) fn shown_need(interaction: &Interaction, domain: &DomainState, lang: Lang) -> String {
    own_text(interaction, domain, lang).map_or_else(|| humanize_task_ids(&interaction.card.need, domain), |(need, _)| need)
}

fn chips(item: &InboxItem, context: &RenderContext<'_>) -> (Vec<BlockChip>, Vec<BlockChip>) {
    let lang = context.lang;
    let chip = |id: &InteractionId, label: String| BlockChip {
        id: id.0.clone(),
        label,
        link: Some(Link::View { key: format!("card:{id}") }),
    };
    let blocks = item
        .blocks
        .iter()
        .map(|id| chip(id, tf!(lang, "blocks {target}", "bloquea {target}", target = target_label(context.domain, id, lang))))
        .collect();
    let blocked_by = item
        .blocked_by
        .iter()
        .map(|id| chip(id, tf!(lang, "blocked by {target}", "bloqueada por {target}", target = target_label(context.domain, id, lang))))
        .collect();
    (blocks, blocked_by)
}

/// Why a delivery cannot be accepted or merged yet, in the user's words.
fn blocked_reason(item: &InboxItem, context: &RenderContext<'_>) -> Option<String> {
    let first = item.blocked_by.first()?;
    let domain = context.domain;
    let blocker = domain.interactions.get(first)?;
    Some(match pr_target(blocker, domain) {
        Some(pr) => tf!(context.lang, "Merge #{number} first", "Haz antes el merge de la #{number}", number = pr.number),
        None => tf!(context.lang, "Answer “{need}” first", "Responde primero «{need}»", need = clip(&shown_need(blocker, domain, context.lang), 50)),
    })
}

fn meta_line(item: &InboxItem, interaction: Option<&Interaction>, context: &RenderContext<'_>) -> Option<String> {
    let lang = context.lang;
    let request_title = || interaction.and_then(|i| match &i.scope {
        Scope::Orchestrator { request_id: Some(request) } => context.domain.requests.get(request).map(|request|
            request.title.clone().unwrap_or_else(|| super::tasks::summary(&request.body))),
        _ => None,
    });
    let who: String = match interaction.map(|i| &i.author) {
        Some(Author::System) => "Alondra".into(),
        Some(Author::Orchestrator { .. }) => request_title().unwrap_or_else(|| "Alondra".into()),
        _ => match &item.agent_id {
            Some(agent_id) => super::common::agent_label(agent_id, context),
            None => request_title().unwrap_or_else(|| "Alondra".into()),
        },
    };
    let project = item.agent_id.as_ref().and_then(|agent_id| {
        context.live.nodes.iter().find(|node| node.agent_id == agent_id.0).and_then(|node| node.project.clone())
    });
    let position = interaction.and_then(|i| i.card.group.as_ref()).filter(|_| context.client == Client::Ios)
        .map(|group| group_position(group, lang));
    Some(std::iter::once(who).chain(project).chain(position).collect::<Vec<_>>().join(" · "))
}

/// "1 of 3": where a question stands among those asked together.
pub(super) fn group_position(group: &QuestionGroup, lang: Lang) -> String {
    tf!(lang, "{index} of {total}", "{index} de {total}", index = group.index, total = group.total)
}

fn state_label(item: &InboxItem, interaction: Option<&Interaction>, context: &RenderContext<'_>) -> Option<Label> {
    let lang = context.lang;
    let interaction = interaction?;
    if let Some(live) = pr_target(interaction, context.domain)
        .and_then(|pr| context.live.pull_requests.iter().find(|detail| detail.id == pr.id.0))
    {
        let mut parts = vec![];
        if live.changed_files > 0 {
            parts.push(format!("+{} −{}", live.additions, live.deletions));
        }
        let (mark, tone) = match live.checks_passed {
            Some(true) => (Some("✓"), Tone::Success),
            Some(false) => (Some("✗"), Tone::Critical),
            None => (None, Tone::Neutral),
        };
        parts.extend(mark.map(str::to_owned));
        return (!parts.is_empty()).then(|| Label { text: parts.join(" · "), tone });
    }
    if matches!(interaction.payload, InteractionPayload::Delivery { .. }) {
        return None;
    }
    Some(if interaction.blocking {
        Label { text: tf!(lang, "Blocks {node}", "Bloquea a {node}", node = node_name(item, context)), tone: Tone::Warning }
    } else {
        Label { text: t!(lang, "Doesn't block", "No bloquea").into(), tone: Tone::Neutral }
    })
}

/// What the tasks that reviewed this one concluded: the line of each approval and, when the
/// rounds ran out, the failures that were left.
fn review_lines(item: &InboxItem, context: &RenderContext<'_>) -> Vec<ContextLine> {
    use crate::domain::{MAX_REVIEW_ROUNDS, Verdict};
    let lang = context.lang;
    let Some(task) = &item.task_id else { return vec![] };
    let line = |text: String| ContextLine { icon: Icon::Check, text, link: None, chip: None, state: None };
    let mut lines = vec![];
    let reviewed_after = |reviewer: &crate::domain::Task| {
        reviewer.final_presentation.as_ref().and_then(|id| context.domain.interactions.get(id)).is_some_and(|result| result.order > item.order)
    };
    let reviewers = context.index.reviewers(task).iter().filter_map(|id| context.domain.tasks.get(id));
    for reviewer in reviewers.filter(|b| reviewed_after(b)) {
        match &reviewer.verdict {
            Some(Verdict::Approved { summary, open_issues: false, .. }) => lines.push(line(
                tf!(lang, "Reviewed by “{title}”: {summary}", "Revisado por «{title}»: {summary}", title = reviewer.title, summary = summary.trim()),
            )),
            Some(Verdict::Approved { summary, open_issues: true, .. }) => {
                let left = summary.lines().filter(|line| !line.trim().is_empty()).count().max(1);
                let text = if left == 1 {
                    tf!(lang, "Reviewed by “{title}”: 1 failure left after {rounds} rounds", "Revisado por «{title}»: queda 1 fallo tras {rounds} vueltas",
                        title = reviewer.title, rounds = MAX_REVIEW_ROUNDS)
                } else {
                    tf!(lang, "Reviewed by “{title}”: {left} failures left after {rounds} rounds", "Revisado por «{title}»: quedan {left} fallos tras {rounds} vueltas",
                        title = reviewer.title, left = left, rounds = MAX_REVIEW_ROUNDS)
                };
                lines.push(line(text));
                lines.push(line(summary.trim().to_owned()));
            }
            _ => {}
        }
    }
    lines
}

fn context_lines(item: &InboxItem, context: &RenderContext<'_>) -> Vec<ContextLine> {
    let mut lines = vec![];
    let task = item.task_id.as_ref().and_then(|id| context.domain.tasks.get(id));
    if let Some(agent_id) = &item.agent_id {
        let node = context.live.nodes.iter().find(|node| node.agent_id == agent_id.0);
        let chip = node.and_then(|node| node.project.as_ref()).map(|project| Label { text: project.clone(), tone: Tone::Neutral });
        lines.push(match task {
            Some(task) => {
                let presentation = super::task_state::present(task, context);
                ContextLine {
                    icon: Icon::Task,
                    text: task.title.clone(),
                    link: Some(Link::View { key: format!("task:{}", task.id) }),
                    chip,
                    state: node.map(|_| Label { text: presentation.state, tone: presentation.tone }),
                }
            }
            None => ContextLine { icon: Icon::Node, text: super::common::agent_label(agent_id, context), link: None, chip, state: None },
        });
    }
    lines.extend(review_lines(item, context));
    lines.extend(merge_unavailable(item, context).map(|text| ContextLine { icon: Icon::Warning, text, link: None, chip: None, state: None }));
    lines
}

/// Why a pull request's card offers no merge, when nothing else on it says so already.
fn merge_unavailable(item: &InboxItem, context: &RenderContext<'_>) -> Option<String> {
    let interaction = interaction_of(item, context.domain)?;
    let pr = pr_target(interaction, context.domain).filter(|pr| pr.state == PrState::Open)?;
    let live = live_pr(pr, context);
    if blocked_reason(item, context).is_some() || live.is_some_and(|detail| detail.conflicts || detail.stacked_on.is_some()) {
        return None;
    }
    let merge = item.available_actions.iter().find(|action| action.kind == "authorize_merge" && !action.enabled)?;
    let refusal = merge.refusal.clone()
        .unwrap_or_else(|| crate::domain::Refusal::for_code(merge.disabled_reason.as_deref().unwrap_or_default()));
    Some(super::messages::refusal_text(&refusal, context.lang))
}

pub(super) fn from_item(item: &InboxItem, context: &RenderContext<'_>, included: bool) -> Result<Card, BuildError> {
    let lang = context.lang;
    let domain = context.domain;
    let interaction = interaction_of(item, domain);
    let spec = &item.card;
    let pr = interaction.and_then(|i| pr_target(i, domain));
    let carries = if pr.is_some() {
        Some(Icon::PullRequest)
    } else {
        interaction.and_then(reviewed_revision).map(|_| Icon::Document)
    };
    let eyebrow_text = match pr {
        Some(pr) if spec.kind == CardKind::Review => format!("{} · PR #{}", kind_label(spec.kind, lang), pr.number),
        _ if spec.group.is_some() => format!("{} · {}", kind_label(spec.kind, lang), group_position(spec.group.as_ref().unwrap(), lang)),
        _ => kind_label(spec.kind, lang).to_owned(),
    };
    let timestamps = context
        .live
        .time(&item.id)
        .map(|at| vec![Stamp { role: "created".into(), at: at.into(), format: TimeFormat::Relative }])
        .unwrap_or_default();
    let (blocks, blocked_by) = chips(item, context);
    let (need, why) = match interaction.and_then(|i| own_text(i, domain, lang)) {
        Some((need, why)) => (need, Some(why)),
        None => (
            humanize_task_ids(&spec.need, domain),
            Some(humanize_task_ids(spec.why.trim(), domain)).filter(|why| !why.is_empty()),
        ),
    };
    let mut card = Card {
        id: public_id(item).into(),
        kind: Some(spec.kind),
        need: Some(need.clone()),
        why: why.clone(),
        meta: meta_line(item, interaction, context),
        content_row: content_row(item, interaction, context),
        blocks,
        blocked_by,
        reply: None,
        more: vec![],
        link: Some(Link::View { key: card_key(item) }),
        icon: kind_icon(spec.kind, carries),
        eyebrow: Some(Label { text: eyebrow_text, tone: kind_tone(spec.kind, item.pending) }),
        title: need,
        subtitle: why,
        badges: vec![],
        context: context_lines(item, context),
        timestamps,
        state: state_label(item, interaction, context),
        detail: if included { Detail::Included } else { Detail::Reference },
        body: None,
        attachments: vec![],
        actions: vec![],
    };
    if !included {
        return Ok(card);
    }
    card.attachments = attachment_refs(interaction);
    if let Some(pr) = pr {
        card.badges = pr_badges(pr, context);
        let own_title = live_pr(pr, context).and_then(|detail| detail.title.as_deref()).map(str::trim).filter(|title| !title.is_empty());
        if let Some(title) = own_title {
            card.title = title.into();
        }
        card.meta = Some(pr_meta(pr, context));
    }
    let (mut visible, mut more) = actions(item, interaction, context)?;
    more.extend(extra_actions(item, interaction, context));
    visible.retain(|action| action.enabled);
    more.retain(|action| action.enabled);
    if visible.len() > VISIBLE_ACTIONS {
        let rest = visible.split_off(VISIBLE_ACTIONS);
        more.splice(0..0, rest);
    }
    if context.client != Client::Ios {
        for (index, action) in visible.iter_mut().enumerate() {
            action.key = Some((index + 1).to_string());
        }
        let mut next = visible.len() + 1;
        for action in more.iter_mut().filter(|action| action.key.is_none()) {
            if next > 9 {
                break;
            }
            action.key = Some(next.to_string());
            next += 1;
        }
    }
    let worded = interaction.map(|interaction| claudes_worded_options(interaction, context.domain)).unwrap_or_default();
    let settles = |mut action: Action| {
        action.resolves_card = ActionId::parse(&action.id).is_ok_and(|id| super::resolves_card(context.domain, &id));
        painted(action, &worded)
    };
    card.reply = reply_action(item, interaction, context).map(settles).and_then(CardAction::usable);
    card.actions = visible.into_iter().map(settles).filter_map(CardAction::usable).collect();
    card.more = more.into_iter().map(settles).filter_map(CardAction::usable).collect();
    Ok(card)
}

/// The ids of the options Claude explained in the question it asked.
fn claudes_worded_options(interaction: &Interaction, domain: &DomainState) -> Vec<String> {
    match (&interaction.payload, domain.holds.get(&interaction.id)) {
        (InteractionPayload::Question { options, .. }, Some(Hold::Question)) => options.iter()
            .filter(|option| option.description.is_some())
            .map(|option| option.id.clone())
            .collect(),
        _ => vec![],
    }
}

/// How a card's button reads is settled here, once, for every client: red only when its style is
/// destructive, and words under it only for «Allow always», which says exactly what stops asking,
/// and for the options of Claude's question that Claude explained.
fn painted(mut action: Action, worded: &[String]) -> Action {
    action.role = Some(match (action.style, action.role) {
        (Style::Destructive, _) => Role::Risk,
        (_, Some(Role::Advance)) | (Style::Primary, _) => Role::Advance,
        _ => Role::Neutral,
    });
    if !action.option_id.as_deref().is_some_and(|id| id == Hold::ALWAYS || worded.iter().any(|worded| worded == id)) {
        action.description = None;
    }
    action
}

fn attachment_refs(interaction: Option<&Interaction>) -> Vec<Attachment> {
    interaction
        .map(|interaction| {
            interaction
                .attachments
                .iter()
                .map(|a| Attachment {
                    id: a.id.clone(),
                    name: a.name.clone(),
                    kind: AttachmentKind::Binary,
                    size: a.bytes,
                    digest: Some(a.content_digest.clone()),
                    fetch: a.content_reference.clone(),
                })
                .collect()
        })
        .unwrap_or_default()
}

// ---------------------------------------------------------------------------------------------
// The one line of content
// ---------------------------------------------------------------------------------------------

fn content_row(item: &InboxItem, interaction: Option<&Interaction>, context: &RenderContext<'_>) -> Option<ContentRow> {
    let lang = context.lang;
    let domain = context.domain;
    let interaction = interaction?;
    let attachments = interaction.attachments.len();
    let live = pr_target(interaction, domain)
        .and_then(|pr| context.live.pull_requests.iter().find(|detail| detail.id == pr.id.0));
    let shown = interaction.shown_content();
    let (kind, summary_text, others) = match shown.as_deref() {
        Some(content) => {
            let agent = content.summary.as_deref().map(str::trim).filter(|text| !text.is_empty()).map(|text| clip(text, 60));
            let derived = match content.kind {
                DomainContent::Diff if pr_target(interaction, domain).is_some() => diff_summary(interaction, live, domain, lang),
                DomainContent::Document if reviewed_revision(interaction).is_some() => {
                    Some(document_summary(interaction, domain, lang)).filter(|title| !title.is_empty())
                }
                DomainContent::Options => payload_options(interaction).map(|n| summary::options(lang, n)),
                DomainContent::Attachments => Some(summary::attachments(lang, attachments)),
                _ => summary::of_content(lang, content),
            };
            let text = agent.or(derived).or_else(|| summary::of_content(lang, content));
            let others = if content.kind == DomainContent::Attachments { 0 } else { attachments };
            (content.kind, text.unwrap_or_default(), others)
        }
        None if pr_target(interaction, domain).is_some() => {
            (DomainContent::Diff, diff_summary(interaction, live, domain, lang).unwrap_or_default(), attachments)
        }
        None if reviewed_revision(interaction).is_some() => {
            (DomainContent::Document, document_summary(interaction, domain, lang), attachments)
        }
        None if attachments > 0 => (DomainContent::Attachments, summary::attachments(lang, attachments), 0),
        None => return None,
    };
    let _ = item;
    let icon = match kind {
        DomainContent::Diff => Icon::PullRequest,
        DomainContent::Document | DomainContent::Plan => Icon::Document,
        DomainContent::Attachments => Icon::Attachment,
        DomainContent::Steps => Icon::Check,
        DomainContent::Diagnosis => Icon::Warning,
        DomainContent::Command => Icon::Action,
        DomainContent::Report | DomainContent::Options => Icon::Notice,
    };
    Some(ContentRow {
        kind: summary::kind_name(kind).into(),
        icon,
        label: summary::label(lang, kind).into(),
        summary: summary_text,
        more_count: others as u32,
    })
}

fn payload_options(interaction: &Interaction) -> Option<usize> {
    match &interaction.payload {
        InteractionPayload::Question { options, .. } | InteractionPayload::ActionRequest { options, .. } => Some(options.len()),
        _ => None,
    }
}

fn diff_summary(
    interaction: &Interaction,
    live: Option<&super::LivePullRequest>,
    domain: &DomainState,
    lang: Lang,
) -> Option<String> {
    if let Some(live) = live.filter(|live| live.changed_files > 0) {
        return Some(summary::changes(lang, live.changed_files, live.additions, live.deletions));
    }
    pr_target(interaction, domain).map(|pr| format!("{} #{}", pr.repository, pr.number))
}

fn document_summary(interaction: &Interaction, domain: &DomainState, lang: Lang) -> String {
    let _ = lang;
    targets_of(interaction)
        .iter()
        .find_map(|target| match target {
            DeliveryTarget::DocumentRevision { document_id, .. } => domain.documents.get(document_id).map(|d| d.title.clone()),
            _ => None,
        })
        .unwrap_or_default()
}

// ---------------------------------------------------------------------------------------------
// Buttons
// ---------------------------------------------------------------------------------------------

fn action_id(verb: Verb, subject: &str, revision: u64, extra: Option<String>) -> Option<ActionId> {
    match ActionId::new(verb, subject, revision, extra) {
        Ok(id) => Some(id),
        Err(_) => {
            eprintln!("Alondra omitted an action with an invalid identifier ({verb:?})");
            None
        }
    }
}

pub(super) fn button(id: String, label: &str, role: Role) -> Action {
    Action {
        id,
        label: label.into(),
        style: match role {
            Role::Advance => Style::Primary,
            Role::Risk => Style::Destructive,
            Role::Neutral => Style::Secondary,
        },
        role: Some(role),
        option_id: None,
        description: None,
        presentation: Presentation::Button,
        options: vec![],
        input: Input::None,
        attachments: false,
        comments: None,
        confirm: None,
        enabled: true,
        disabled_reason: None,
        stamp: None,
        key: None,
        link: None,
        resolves_card: false,
    }
}

fn with_availability(mut action: Action, available: Option<&AvailableAction>, lang: Lang) -> Action {
    if let Some(available) = available {
        action.enabled = available.enabled;
    }
    action
}

fn text_input(placeholder: &str, required: bool) -> Input {
    Input::Text { placeholder: placeholder.into(), required, multiline: true }
}

/// The buttons the card shows (the rest go behind "More"), then the ones that always go there.
fn actions(
    item: &InboxItem,
    interaction: Option<&Interaction>,
    context: &RenderContext<'_>,
) -> Result<(Vec<Action>, Vec<Action>), BuildError> {
    let Some(interaction) = interaction else { return Ok((vec![], vec![])) };
    let find = |kind: &str| item.available_actions.iter().find(|action| action.kind == kind);
    Ok(match &interaction.payload {
        InteractionPayload::Information { .. } => (mark_read(item, interaction, find("mark_read"), context), vec![]),
        InteractionPayload::Question { .. } | InteractionPayload::ActionRequest { .. } => {
            answer_actions(item, interaction, find("answer_question"), context)
        }
        InteractionPayload::Delivery { .. } => delivery_actions(item, interaction, context),
    })
}

fn mark_read(
    item: &InboxItem,
    interaction: &Interaction,
    available: Option<&AvailableAction>,
    context: &RenderContext<'_>,
) -> Vec<Action> {
    let lang = context.lang;
    let Some(id) = action_id(Verb::Read, &interaction.id.0, interaction.revision, None) else { return vec![] };
    let label = if item.card.kind == CardKind::Know { t!(lang, "Seen", "Visto") } else { t!(lang, "Mark as read", "Leído") };
    let mut action = button(id.to_string(), label, Role::Neutral);
    action.stamp = Some(t!(lang, "Read", "Leída").into());
    vec![with_availability(action, available, lang)]
}

/// What an option reads in the user's language; Claude Code's own prompts keep their fixed answers.
fn held_option(hold: Hold, option: &QuestionOption, lang: Lang) -> String {
    let label = match (hold, hold.approves(&option.id)) {
        (Hold::Permission, true) => t!(lang, "Allow", "Permitir"),
        (Hold::Permission, false) if option.id == Hold::ALWAYS => t!(lang, "Allow always", "Permitir siempre"),
        (Hold::Permission, false) => t!(lang, "Deny", "Denegar"),
        (Hold::Plan, true) => t!(lang, "Approve", "Aprobar"),
        (Hold::Plan, false) => t!(lang, "Request changes", "Pedir cambios"),
        (Hold::Elicitation, true) => t!(lang, "Accept", "Aceptar"),
        (Hold::Elicitation, false) => t!(lang, "Decline", "Rechazar"),
        (Hold::Question, _) => option.label.as_str(),
    };
    label.to_owned()
}

/// What the user chose, as the card showed it: the translated words of a held prompt or of an
/// option the system wrote, otherwise the label its author gave. Never the option's id.
pub fn answer_label(state: &DomainState, interaction: &Interaction, option: &QuestionOption, lang: Lang) -> String {
    match state.holds.get(&interaction.id).copied().filter(|hold| *hold != Hold::Question) {
        Some(Hold::Permission) if option.id == Hold::ALWAYS => interaction.card.allow_always.as_ref()
            .and_then(|offer| super::allow_always::chosen(offer, lang))
            .unwrap_or_else(|| held_option(Hold::Permission, option, lang)),
        Some(hold) => held_option(hold, option, lang),
        None => fixed_option_text(interaction, option, lang).0,
    }
}

/// The problem of Alondra's card for a task that cannot start by itself.
fn is_machine_card(interaction: &Interaction) -> bool {
    interaction.author == Author::System && interaction.id.0.starts_with(crate::machine_incident::MACHINE_PREFIX)
}

fn start_problem_of(interaction: &Interaction) -> Option<crate::domain::StartProblem> {
    (interaction.author == Author::System && interaction.id.0.starts_with(crate::domain::START_PROBLEM_PREFIX))
        .then(|| crate::start_problem::from_body(&interaction.body))
        .flatten()
}

fn start_problem_card(interaction: &Interaction, domain: &DomainState, lang: Lang) -> Option<CardSpec> {
    let problem = start_problem_of(interaction)?;
    let Scope::Task { task_id } = &interaction.scope else { return None };
    let title = domain.tasks.get(task_id)?.title.as_str();
    let dependency = match &problem {
        crate::domain::StartProblem::DependencyCancelled { dependency } => domain.tasks.get(dependency).map(|task| task.title.as_str()),
        _ => None,
    };
    Some(crate::start_problem::card(title, &problem, dependency, lang))
}

fn start_problem_text(interaction: &Interaction, domain: &DomainState, lang: Lang) -> Option<(String, String)> {
    start_problem_card(interaction, domain, lang).map(|card| (card.need, card.why))
}

/// What one of Alondra's own cards asks and why, in the viewer's language; `None` for a card
/// someone worded.
pub(super) fn own_text(interaction: &Interaction, domain: &DomainState, lang: Lang) -> Option<(String, String)> {
    host_lost_text(interaction, domain, lang)
        .or_else(|| start_problem_text(interaction, domain, lang))
        .or_else(|| pace_text(interaction, domain, lang))
        .or_else(|| unheard_text(interaction, domain, lang))
        .or_else(|| turn_ended_text(interaction, domain, lang))
        .or_else(|| session_ended_text(interaction, domain, lang))
        .or_else(|| dropped_text(interaction, lang))
        .or_else(|| decision_cancelled_text(interaction, domain, lang))
        .or_else(|| claude_hold_text(interaction, domain, lang))
        .or_else(|| mcp_wait_text(interaction, domain, lang))
}

/// How Alondra can sign in to the server an MCP wait's card waits for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum McpWay {
    /// Claude signs in to it: Connect opens the sign-in.
    SignIn,
    /// A claude.ai connector: it is connected on claude.ai, then connected again here.
    ClaudeAi,
    /// Turned off in Claude Code: only going on without it is left.
    Off,
}

fn mcp_way(interaction: &Interaction, domain: &DomainState) -> Option<(crate::domain::McpServerKey, McpWay)> {
    let server = crate::domain::McpServerKey::of_card(interaction)?;
    let Scope::Task { task_id } = &interaction.scope else { return Some((server, McpWay::SignIn)) };
    let reported = domain.mcp_server_of(task_id, &server);
    let way = match reported {
        Some(reported) if reported.status == crate::domain::McpStatus::Disabled => McpWay::Off,
        Some(reported) if reported.source.as_deref() == Some("claudeai") || reported.transport.as_deref() == Some("claudeai-proxy") => McpWay::ClaudeAi,
        _ => McpWay::SignIn,
    };
    Some((server, way))
}

/// The card of the tasks that wait for an MCP server, in the viewer's language: Waiting for You
/// shows one of them for every task that waits for the same server.
fn mcp_wait_text(interaction: &Interaction, domain: &DomainState, lang: Lang) -> Option<(String, String)> {
    let (server, way) = mcp_way(interaction, domain)?;
    let name = server.name.as_str();
    let tasks: Vec<&str> = domain.mcp_waiting_for(&server).iter().filter_map(|waiting| domain.tasks.get(&waiting.task)).map(|task| task.title.as_str()).collect();
    let waiting = tasks.len().max(1);
    let need = match waiting {
        1 => tf!(lang, "Sign in to {name}: a task waits for it", "Inicia sesión en {name}: espera una tarea"),
        _ => tf!(lang, "Sign in to {name}: {waiting} tasks wait for it", "Inicia sesión en {name}: esperan {waiting} tareas"),
    };
    let why = match way {
        McpWay::SignIn => tf!(lang,
            "Claude cannot use the MCP server {name} until you sign in. Connect opens the sign-in; once it is done, every waiting task goes on by itself.",
            "Claude no puede usar el servidor MCP {name} hasta que inicies sesión. Conectar abre el inicio de sesión; al terminar, las tareas que esperan siguen solas."),
        McpWay::ClaudeAi => tf!(lang,
            "{name} is a claude.ai connector. Connect opens it on claude.ai; once it is connected there, press Reconnect and every waiting task goes on.",
            "{name} es un conector de claude.ai. Conectar lo abre en claude.ai; cuando esté conectado allí, pulsa Reconectar y las tareas que esperan siguen."),
        McpWay::Off => tf!(lang,
            "{name} is turned off in Claude Code, so Alondra cannot sign in to it. Turn it on in Claude Code or go on without it.",
            "{name} está desactivado en Claude Code, así que Alondra no puede iniciar sesión. Actívalo en Claude Code o sigue sin él."),
    };
    let titles = |list: &[&str]| list.iter().map(|title| tf!(lang, "“{title}”", "«{title}»")).collect::<Vec<_>>();
    let who = match tasks.as_slice() {
        [] => String::new(),
        [one] => {
            let one = titles(&[*one]).concat();
            tf!(lang, " Waiting: {one}.", " Espera: {one}.")
        }
        [first @ .., last] if tasks.len() <= 3 => {
            let (first, last) = (titles(first).join(", "), titles(&[*last]).concat());
            tf!(lang, " Waiting: {first} and {last}.", " Esperan: {first} y {last}.")
        }
        [first, second, rest @ ..] => {
            let (shown, more) = (titles(&[*first, *second]).join(", "), rest.len());
            tf!(lang, " Waiting: {shown} and {more} more.", " Esperan: {shown} y {more} más.")
        }
    };
    Some((clip(&need, CardSpec::NEED_MAX), format!("{why}{who}")))
}

/// Connect and Reconnect of an MCP wait's card: every client runs them itself, through the daemon,
/// and brings back where the browser came back to when it signs in on another device.
fn mcp_actions(interaction: &Interaction, context: &RenderContext<'_>) -> Vec<Action> {
    let Some((server, way)) = mcp_way(interaction, context.domain) else { return vec![] };
    if way == McpWay::Off {
        return vec![];
    }
    let lang = context.lang;
    let card = interaction.id.0.clone();
    let mut connect = local_action(t!(lang, "Connect", "Conectar"), "1", LocalLink::McpSignIn { card: card.clone(), server: server.name.clone() });
    connect.style = Style::Primary;
    connect.role = Some(Role::Advance);
    let mut reconnect = local_action(t!(lang, "Reconnect", "Reconectar"), "2", LocalLink::McpReconnect { card });
    if context.client == Client::Tui {
        connect.description = Some(t!(lang, "Opens the sign-in in the browser", "Abre el inicio de sesión en el navegador").into());
        reconnect.description = Some(t!(lang, "The waiting tasks read their sign-in again", "Las tareas que esperan vuelven a leer su sesión").into());
    }
    vec![connect, reconnect]
}

/// The card of a task Claude holds, in the viewer's language: Waiting for You shows one of them
/// for every task that waits on the same hold.
fn claude_hold_text(interaction: &Interaction, domain: &DomainState, lang: Lang) -> Option<(String, String)> {
    use crate::domain::ClaudeHold;
    if interaction.author != Author::System {
        return None;
    }
    let (hold, account) = domain.claude_card_waits_for(interaction)?;
    let waiting = domain.claude_waiting(interaction).max(1);
    let named = account.as_ref().filter(|account| !account.is_default()).map(|account| {
        domain.accounts.iter().find(|status| status.id == *account).map_or_else(|| account.to_string(), |status| status.name.clone())
    });
    let need = match (hold, waiting, &named) {
        (ClaudeHold::SignedOut, _, None) => t!(lang, "Sign in to Claude outside Alondra", "Inicia sesión en Claude fuera de Alondra").to_owned(),
        (ClaudeHold::SignedOut, _, Some(name)) => tf!(lang, "Sign in to the Claude account {name} on the Mac", "Inicia sesión en la cuenta de Claude {name} en el Mac"),
        (ClaudeHold::Outage, 1, _) => t!(lang, "Claude's API is not answering: a task waits", "La API de Claude no responde: una tarea espera").to_owned(),
        (ClaudeHold::Outage, _, _) => tf!(lang, "Claude's API is not answering: {waiting} tasks wait", "La API de Claude no responde: esperan {waiting} tareas"),
        (ClaudeHold::Usage, 1, None) => t!(lang, "Claude's usage limit is reached: a task waits", "Se agotó el límite de uso de Claude: una tarea espera").to_owned(),
        (ClaudeHold::Usage, _, None) => tf!(lang, "Claude's usage limit is reached: {waiting} tasks wait", "Se agotó el límite de uso de Claude: esperan {waiting} tareas"),
        (ClaudeHold::Usage, 1, Some(name)) => tf!(lang, "The Claude account {name} reached its limit: a task waits", "La cuenta de Claude {name} agotó su límite: una tarea espera"),
        (ClaudeHold::Usage, _, Some(name)) => tf!(lang, "The Claude account {name} reached its limit: {waiting} tasks wait", "La cuenta de Claude {name} agotó su límite: esperan {waiting} tareas"),
    };
    let how = match &named {
        None => t!(lang, "Open Terminal, run claude and sign in with /login, then press I signed in.",
            "Abre Terminal, ejecuta claude e inicia sesión con /login; luego pulsa Ya he iniciado sesión.").to_owned(),
        Some(name) => tf!(lang, "Open Terminal on the Mac, run alondra accounts login \"{name}\", then press I signed in.",
            "Abre Terminal en el Mac, ejecuta alondra accounts login \"{name}\"; luego pulsa Ya he iniciado sesión."),
    };
    let why = match (hold, waiting, &named) {
        (ClaudeHold::SignedOut, 1, None) => format!("{} {how}", t!(lang, "Claude Code's sign-in expired and a task waits.", "La sesión de Claude Code caducó y una tarea espera.")),
        (ClaudeHold::SignedOut, _, None) => format!("{} {how}", tf!(lang, "Claude Code's sign-in expired and {waiting} tasks wait.", "La sesión de Claude Code caducó y esperan {waiting} tareas.")),
        (ClaudeHold::SignedOut, 1, Some(name)) => format!("{} {how}", tf!(lang, "The sign-in of {name} expired and a task waits. No new work starts on it meanwhile.", "La sesión de {name} caducó y una tarea espera. Mientras, no empieza trabajo nuevo en ella.")),
        (ClaudeHold::SignedOut, _, Some(name)) => format!("{} {how}", tf!(lang, "The sign-in of {name} expired and {waiting} tasks wait. No new work starts on it meanwhile.", "La sesión de {name} caducó y esperan {waiting} tareas. Mientras, no empieza trabajo nuevo en ella.")),
        (ClaudeHold::Outage, _, _) => t!(lang,
            "The Claude API kept failing after Claude Code's own retries. Alondra tries one task again every 2 minutes; when Claude answers, every waiting task goes on.",
            "La API de Claude siguió fallando tras los reintentos de Claude Code. Alondra prueba una tarea cada 2 minutos; cuando Claude responda, siguen todas.").to_owned(),
        (ClaudeHold::Usage, _, None) => t!(lang,
            "The tasks that run through Claude Code wait until the limit resets. Alondra tries one then; when Claude answers, every waiting task goes on.",
            "Las tareas que van por Claude Code esperan a que se reinicie el límite. Entonces Alondra prueba una; cuando Claude responda, siguen todas.").to_owned(),
        (ClaudeHold::Usage, _, Some(name)) => tf!(lang,
            "The tasks that run on {name} wait until its limit resets; new work starts on the other accounts meanwhile. Alondra tries one then; when Claude answers, the rest go on.",
            "Las tareas que van con {name} esperan a que se reinicie su límite; mientras, el trabajo nuevo empieza en las otras cuentas. Entonces Alondra prueba una; cuando Claude responda, siguen las demás."),
    };
    Some((clip(&need, CardSpec::NEED_MAX), why))
}

/// What a refused Claude API request needs, in the viewer's language.
fn api_refusal_text(error: &str, lang: Lang) -> &'static str {
    match error {
        "authentication_failed" => t!(lang, "the Claude account refuses this request", "la cuenta de Claude rechaza esta petición"),
        "oauth_org_not_allowed" => t!(lang, "the Claude organization does not allow Claude Code", "la organización de Claude no permite Claude Code"),
        "account_on_hold" => t!(lang, "the Claude account is on hold", "la cuenta de Claude está suspendida"),
        "verification_required" => t!(lang, "the Claude account needs verifying", "la cuenta de Claude necesita verificarse"),
        "billing_error" => t!(lang, "the Claude account has a billing problem", "la cuenta de Claude tiene un problema de facturación"),
        "cloud_credential_error" => t!(lang, "Claude Code's cloud credentials fail", "fallan las credenciales en la nube de Claude Code"),
        "model_not_found" => t!(lang, "Claude Code's model is not available", "el modelo de Claude Code no está disponible"),
        "invalid_request" => t!(lang, "the Claude API refuses Claude Code's request", "la API de Claude rechaza la petición de Claude Code"),
        _ => t!(lang, "the Claude API refuses its request", "la API de Claude rechaza su petición"),
    }
}

/// What a node said last, when the card holds its words: the task's row shows them.
pub(super) fn node_words(interaction: &Interaction) -> Option<&str> {
    (interaction.id.0.starts_with(crate::domain::UNHEARD_PREFIX) && interaction.body.trim() != interaction.card.need.trim())
        .then_some(interaction.card.why.trim())
}

/// The card with the last words of a node whose turn ended with nothing for the user, in the
/// viewer's language.
fn unheard_text(interaction: &Interaction, domain: &DomainState, lang: Lang) -> Option<(String, String)> {
    if !interaction.id.0.starts_with(crate::domain::UNHEARD_PREFIX) {
        return None;
    }
    let Scope::Task { task_id } = &interaction.scope else { return None };
    let title = domain.tasks.get(task_id)?.title.as_str();
    let silent = interaction.body.trim() == interaction.card.need.trim();
    if !silent {
        let need = fit_title(title, CardSpec::TITLED_NEED_MAX, |title| tf!(lang, "“{title}” stopped and wrote to you", "«{title}» se ha parado y te escribe"));
        return Some((need, interaction.card.why.clone()));
    }
    let need = fit_title(title, CardSpec::TITLED_NEED_MAX, |title| tf!(lang, "“{title}” stopped without a word", "«{title}» se ha parado sin decir nada"));
    Some((need, t!(lang,
        "Its turn ended without a result or a question for you. Answer it, or write to it, and it goes on.",
        "Terminó su turno sin un resultado ni una pregunta para ti. Respóndele o escríbele y sigue.").into()))
}

/// The card that asks how a task goes on when Claude stopped waiting for the decision it asked
/// for and nothing else explains its next step, in the viewer's language.
fn decision_cancelled_text(interaction: &Interaction, domain: &DomainState, lang: Lang) -> Option<(String, String)> {
    if interaction.author != Author::System || !interaction.id.0.starts_with(crate::domain::DECISION_CANCELLED_PREFIX) {
        return None;
    }
    let Scope::Task { task_id } = &interaction.scope else { return None };
    let title = domain.tasks.get(task_id)?.title.as_str();
    let need = fit_title(title, CardSpec::TITLED_NEED_MAX, |title| tf!(lang, "“{title}” stopped waiting for your decision: tell it how to go on", "«{title}» dejó de esperar tu decisión: dile cómo seguir"));
    Some((need, t!(lang,
        "Claude Code stopped waiting for the decision it asked you for. Tell it to go on, write to it what to do, or cancel the task.",
        "Claude Code dejó de esperar la decisión que te pidió. Dile que siga, escríbele qué hacer o cancela la tarea.").into()))
}

/// The card that asks how a task goes on when its SDK turn ended with nothing that explains its
/// next step, in the viewer's language.
fn turn_ended_text(interaction: &Interaction, domain: &DomainState, lang: Lang) -> Option<(String, String)> {
    if interaction.author != Author::System || !interaction.id.0.starts_with(crate::domain::TURN_ENDED_PREFIX) {
        return None;
    }
    let Scope::Task { task_id } = &interaction.scope else { return None };
    let title = domain.tasks.get(task_id)?.title.as_str();
    if interaction.card.why == crate::domain::REFUSED_WHY {
        let refusal = crate::domain::REFUSALS.into_iter().find(|error| interaction.card.need.ends_with(crate::domain::api_refusal(error)))
            .map_or(t!(lang, "Claude Code cannot use the Claude API", "Claude Code no puede usar la API de Claude"), |error| api_refusal_text(error, lang));
        let need = fit_title(title, CardSpec::TITLED_NEED_MAX, |title| tf!(lang, "“{title}” stopped: {refusal}", "«{title}» se ha parado: {refusal}"));
        return Some((need, t!(lang,
            "Claude Code cannot use the Claude API until this is fixed outside Alondra. Fix it, then tell the task to go on, or cancel it.",
            "Claude Code no puede usar la API de Claude hasta que se arregle fuera de Alondra. Arréglalo y dile a la tarea que siga, o cancélala.").into()));
    }
    if interaction.card.why == crate::domain::INTERRUPTED_WHY {
        return Some(interrupted_text(title, lang));
    }
    if interaction.card.why == crate::domain::FAILED_WHY {
        let code = interaction.card.need.split('`').nth(1)?;
        let need = tf!(lang, "Claude's turn ended with an error (`{code}`): tell it how to go on",
            "El turno de Claude terminó con un error (`{code}`): dile cómo seguir");
        return Some((clip(&need, CardSpec::NEED_MAX), t!(lang,
            "Claude Code ended its turn as failed. Tell it how to go on, write to it what to do, or cancel the task.",
            "Claude Code terminó su turno como fallido. Dile cómo seguir, escríbele qué hacer o cancela la tarea.").into()));
    }
    let need = fit_title(title, CardSpec::TITLED_NEED_MAX, |title| tf!(lang, "“{title}” stopped without delivering or asking: tell it how to go on", "«{title}» paró sin entregar ni preguntar: dile cómo seguir"));
    Some((need, t!(lang,
        "Its turn ended with nothing that explains its next step. Tell it to go on, write to it what to do, or cancel the task.",
        "Su turno terminó sin nada que explique qué hace después. Dile que siga, escríbele qué hacer o cancela la tarea.").into()))
}

/// The card of a task whose SDK execution Alondra could not take over, with the reason, in the
/// viewer's language: the card it waited on becomes this one, so the task has one.
fn host_lost_text(interaction: &Interaction, domain: &DomainState, lang: Lang) -> Option<(String, String)> {
    let retake = matches!(&interaction.payload, InteractionPayload::Question { options, .. } if options.iter().any(|option| option.id == crate::domain::RETAKE));
    if interaction.author != Author::System || !retake {
        return None;
    }
    let Scope::Task { task_id } = &interaction.scope else { return None };
    let title = domain.tasks.get(task_id)?.title.as_str();
    let need = fit_title(title, CardSpec::TITLED_NEED_MAX, |title| tf!(lang, "“{title}” stopped: Alondra could not resume its session", "«{title}» se paró: Alondra no pudo retomar su sesión"));
    Some((need, t!(lang,
        "Its session could not be started again; the details say why. Try again once that is fixed, or cancel the task.",
        "Su sesión no pudo volver a arrancar; el detalle dice por qué. Reinténtalo cuando esté arreglado o cancela la tarea.").into()))
}

/// The card about a message Claude will never read, in the viewer's language.
fn dropped_text(interaction: &Interaction, lang: Lang) -> Option<(String, String)> {
    if interaction.author != Author::System || !interaction.id.0.starts_with(crate::domain::DROPPED_PREFIX) {
        return None;
    }
    Some((t!(lang, "Claude didn't get to read your message", "Claude no llegó a leer tu mensaje").into(), t!(lang,
        "Claude Code finished or cancelled your message without reading it, so it will not act on it. Nothing is sent until you decide.",
        "Claude Code terminó o canceló tu mensaje sin leerlo, así que no hará nada con él. No se envía nada hasta que decidas.").into()))
}

/// The card of a turn the user stopped, by interrupting it or forcing it to stop.
fn interrupted_text(title: &str, lang: Lang) -> (String, String) {
    let need = fit_title(title, CardSpec::TITLED_NEED_MAX, |title| tf!(lang, "You interrupted “{title}”: tell it how to go on", "Lo interrumpiste: dile a «{title}» cómo seguir"));
    (need, t!(lang,
        "You stopped its turn. Tell it to carry on, write what to do, or cancel the task.",
        "Paraste su turno. Dile que siga, escríbele qué hacer o cancela la tarea.").into())
}

/// The cards about a task whose SDK session closed mid-way: how it goes on, and whether a message
/// it may not have read is sent again, in the viewer's language.
fn session_ended_text(interaction: &Interaction, domain: &DomainState, lang: Lang) -> Option<(String, String)> {
    if interaction.author != Author::System {
        return None;
    }
    let Scope::Task { task_id } = &interaction.scope else { return None };
    let title = domain.tasks.get(task_id)?.title.as_str();
    if interaction.id.0.starts_with(crate::domain::SESSION_ENDED_PREFIX) && interaction.card.why == crate::domain::POST_MERGE_STOPPED_WHY {
        let need = fit_title(title, CardSpec::TITLED_NEED_MAX, |title| tf!(lang, "You stopped “{title}” during its work after the merge: tell it how to go on",
            "Paraste «{title}» durante su trabajo tras el merge: dile cómo seguir"));
        return Some((need, t!(lang,
            "Its PR is merged and you stopped what it ran after the merge. Tell it to go on, with all its context, or close the task if that work is done.",
            "Su PR ya tiene el merge hecho y paraste lo que hacía tras el merge. Dile que siga, con todo su contexto, o cierra la tarea si ese trabajo ya está hecho.").into()));
    }
    if interaction.id.0.starts_with(crate::domain::SESSION_ENDED_PREFIX) && interaction.card.why == crate::domain::POST_MERGE_ENDED_WHY {
        let need = fit_title(title, CardSpec::TITLED_NEED_MAX, |title| tf!(lang, "“{title}” stopped its work after the merge when its session closed: tell it how to go on",
            "«{title}» dejó a medias su trabajo tras el merge al cerrarse su sesión: dile cómo seguir"));
        return Some((need, t!(lang,
            "Its PR is merged. Its session closed while it ran what it said comes after the merge, and whatever ran in the background stopped. Tell it to go on, with all its context, or close the task if that work is done.",
            "Su PR ya tiene el merge hecho. Su sesión se cerró mientras hacía lo que dijo que venía tras el merge, y lo que corría en segundo plano se paró. Dile que siga, con todo su contexto, o cierra la tarea si ese trabajo ya está hecho.").into()));
    }
    if interaction.id.0.starts_with(crate::domain::SESSION_ENDED_PREFIX) && interaction.card.why == crate::domain::INTERRUPTED_WHY {
        return Some(interrupted_text(title, lang));
    }
    if interaction.id.0.starts_with(crate::domain::SESSION_ENDED_PREFIX) {
        let need = fit_title(title, CardSpec::TITLED_NEED_MAX, |title| tf!(lang, "“{title}” was interrupted when its session closed: tell it how to go on",
            "«{title}» se interrumpió al cerrarse su sesión: dile cómo seguir"));
        let why = if interaction.card.why == crate::domain::SESSION_ENDED_AGAIN_WHY {
            t!(lang, "Alondra resumed its session after it closed, and it closed again before it could finish a turn. It resumes with all its context if you tell it to go on.",
                "Alondra retomó su sesión después de cerrarse, y se volvió a cerrar antes de terminar un turno. Retoma con todo su contexto si le dices que siga.")
        } else {
            t!(lang, "Its session closed mid-way, as when the Mac shuts down. It resumes with all its context; whatever ran in the background stopped.",
                "Su sesión se cerró a medias, como al apagar el Mac. Retoma con todo su contexto; lo que corría en segundo plano se paró.")
        };
        return Some((need, why.into()));
    }
    if !interaction.id.0.starts_with(crate::domain::HANDOFF_DOUBT_PREFIX) {
        return None;
    }
    let need = fit_title(title, CardSpec::TITLED_NEED_MAX, |title| tf!(lang, "“{title}” may not have got your message: send it again?", "«{title}» quizá no recibió tu mensaje: ¿se lo reenvío?"));
    let launch = matches!(&interaction.payload, InteractionPayload::Question { options, .. } if options.iter().any(|option| option.id == crate::domain::CANCEL_TASK));
    let why = if launch {
        t!(lang, "Its session closed before it started on the task, right after the message was handed over. Nothing is sent until you decide.",
            "Su sesión se cerró antes de empezar la tarea, justo después de entregarle el mensaje. No se envía nada hasta que decidas.")
    } else {
        t!(lang, "Its session closed right after the message was handed over and before it showed it had read it. Nothing is sent until you decide.",
            "Su sesión se cerró justo después de entregarle el mensaje y antes de mostrar que lo leyó. No se envía nada hasta que decidas.")
    };
    Some((need, why.into()))
}

/// What Alondra's own cards about the pace of the week's usage, about the machine and about a task
/// that ended with a card unanswered say, in the viewer's language.
fn pace_text(interaction: &Interaction, domain: &DomainState, lang: Lang) -> Option<(String, String)> {
    use crate::usage::{OVER_PACE_PREFIX, PACE_RESUME_PREFIX, PaceNumbers};
    if interaction.author != Author::System {
        return None;
    }
    if let Some(question) = interaction.id.0.strip_prefix(crate::domain::ENDED_UNANSWERED_PREFIX) {
        let question = domain.interactions.get(&InteractionId(question.into()))?;
        let need = shown_need(question, domain, lang);
        return Some((
            clip(&tf!(lang, "No longer waits for your answer: {need}", "Ya no espera tu respuesta: {need}", need = need), CardSpec::NEED_MAX),
            t!(lang, "The work was finished before you answered", "El trabajo se terminó antes de que respondieras").into(),
        ));
    }
    if interaction.id.0.starts_with(crate::machine_incident::MACHINE_PREFIX) {
        return crate::machine_incident::MachineCard::from_body(&interaction.body).map(|card| card.text(lang));
    }
    if interaction.id.0.starts_with(crate::local_network_card::LOCAL_NETWORK_PREFIX) {
        return crate::local_network_card::LocalNetworkCard::from_body(&interaction.body).map(|card| card.text(lang));
    }
    if interaction.id.0.starts_with(crate::orchestrator_batch::BATCH_PROBLEM_PREFIX) {
        return crate::orchestrator_batch::BatchProblem::from_body(&interaction.body).map(|card| card.text(lang));
    }
    if interaction.id.0.starts_with(crate::orchestrator_cards::STUCK_PROBLEM_PREFIX) {
        return crate::orchestrator_cards::StuckProblem::from_body(&interaction.body).map(|card| card.text(lang));
    }
    if interaction.id.0.starts_with(crate::orchestrator_cards::UNSPLIT_PROBLEM_PREFIX) {
        return crate::orchestrator_cards::UnsplitProblem::from_body(&interaction.body).map(|card| card.text(lang));
    }
    if interaction.id.0.starts_with(crate::orchestrator_cards::UNANSWERED_PROBLEM_PREFIX) {
        return crate::orchestrator_cards::UnansweredProblem::from_body(&interaction.body).map(|card| card.text(lang));
    }
    if interaction.id.0.starts_with(crate::domain::PLAN_REVIEW_PROBLEM_PREFIX) {
        return crate::domain::PlanReviewProblem::from_body(&interaction.body).map(|card| card.text(lang));
    }
    if interaction.id.0.starts_with(crate::close_problem::CLOSE_PROBLEM_PREFIX)
        && let Some(card) = crate::close_problem::CloseProblem::from_body(&interaction.body)
    {
        return Some(card.text(lang));
    }
    if interaction.id.0.starts_with(OVER_PACE_PREFIX) {
        let numbers = PaceNumbers::from_body(&interaction.body)?;
        return Some((
            t!(lang, "Today is over its share of the weekly Claude usage", "Hoy vas por encima de tu parte del uso semanal").into(),
            tf!(lang,
                "{spent}% used today against a {share}% share · {week}% of the week used",
                "{spent}% usado hoy frente a una parte del {share}% · {week}% de la semana usado",
                spent = numbers.today_spent, share = numbers.today_share, week = numbers.week_percent),
        ));
    }
    interaction.id.0.starts_with(PACE_RESUME_PREFIX).then(|| (
        t!(lang, "Resume Claude work now?", "¿Reanudar el trabajo con Claude ahora?").into(),
        t!(lang, "Paused by you until tomorrow 00:00", "Pausado por ti hasta mañana 00:00").into(),
    ))
}

/// The words of an option the system writes itself, keyed by the id and English text it was
/// built with, in the viewer's language. An option someone worded keeps its own label.
fn fixed_option_text(interaction: &Interaction, option: &QuestionOption, lang: Lang) -> (String, Option<String>) {
    if let Some(problem) = start_problem_of(interaction)
        && let Some(own) = crate::start_problem::options(&problem, lang).into_iter().find(|own| own.id == option.id)
    {
        return (own.label, own.description);
    }
    if interaction.author == Author::System
        && interaction.id.0.starts_with(crate::local_network_card::LOCAL_NETWORK_PREFIX)
        && let Some((label, description)) = crate::local_network_card::option_text(&option.id, &option.label, lang)
    {
        return (label.into(), Some(description.into()));
    }
    if option.id == crate::domain::MCP_GO_ON_WITHOUT
        && let Some(server) = crate::domain::McpServerKey::of_card(interaction)
    {
        let name = server.name.as_str();
        return (tf!(lang, "Go on without {name}", "Seguir sin {name}"),
            Some(t!(lang, "Every task that waits for it goes on without it", "Las tareas que lo esperan siguen sin él").into()));
    }
    if let Some((label, description)) = crate::close_problem::option_text(&option.id, &option.label, lang) {
        return (label.into(), Some(description.into()));
    }
    if let Some((label, description)) = crate::orchestrator_cards::option_text(&option.id, &option.label, lang) {
        return (label.into(), Some(description.into()));
    }
    let (label, description): (&str, Option<&str>) = match (option.id.as_str(), option.label.as_str()) {
        ("pause_until_tomorrow", "Pause until tomorrow") => (
            t!(lang, "Pause until tomorrow", "Pausar hasta mañana"),
            Some(t!(lang, "Nothing starts and nodes wait until midnight, then carry on", "No arranca nada y los nodos esperan hasta medianoche; luego continúan")),
        ),
        ("keep_going", "Keep going") => (t!(lang, "Keep going", "Seguir"), None),
        (crate::domain::SIGNED_IN, crate::domain::SIGNED_IN_LABEL) => (
            t!(lang, "I signed in", "Ya he iniciado sesión"),
            Some(t!(lang, "One waiting task goes on; when Claude answers it, the rest go on too",
                "Sigue una de las tareas que esperan; cuando Claude le responda, siguen las demás")),
        ),
        (crate::domain::TRY_NOW, crate::domain::TRY_NOW_LABEL) => (
            t!(lang, "Try now", "Reintentar ahora"),
            Some(t!(lang, "One waiting task tries again now; when Claude answers it, the rest go on too",
                "Una de las tareas que esperan lo intenta ahora; cuando Claude le responda, siguen las demás")),
        ),
        (crate::domain::CARRY_ON, crate::domain::CARRY_ON_LABEL) => (
            t!(lang, "Carry on", "Que siga"),
            Some(t!(lang, "It goes on with its task and asks you through a card when it needs you",
                "Sigue con su tarea y te pregunta con una tarjeta cuando te necesite")),
        ),
        (crate::domain::RETAKE, crate::domain::RETAKE_LABEL) => (
            t!(lang, "Try again", "Reintentar"),
            Some(t!(lang, "Alondra starts its session again where it left off", "Alondra vuelve a arrancar su sesión donde se quedó")),
        ),
        (crate::domain::RESEND, crate::domain::RESEND_LABEL) => (
            t!(lang, "Send it again", "Reenviar"),
            Some(t!(lang, "It gets the message again; if it had read it, it sees it twice",
                "Lo recibe otra vez; si ya lo había leído, lo verá dos veces")),
        ),
        (crate::domain::SEND_AGAIN, crate::domain::SEND_AGAIN_LABEL) => (
            t!(lang, "Send it again", "Enviar otra vez"),
            Some(t!(lang, "It gets the same message again", "Le llega el mismo mensaje otra vez")),
        ),
        (crate::domain::DISCARD, crate::domain::DISCARD_LABEL) => (
            t!(lang, "Discard", "Descartar"),
            Some(t!(lang, "The message is not sent and the task goes on", "No se envía y la tarea sigue")),
        ),
        (crate::domain::TAKE_AS_RECEIVED, crate::domain::TAKE_AS_RECEIVED_LABEL) => (
            t!(lang, "Take it as received", "Darlo por recibido"),
            Some(t!(lang, "The message is not sent again and the task goes on", "No se reenvía y la tarea sigue")),
        ),
        (crate::domain::CLOSE_TASK, crate::domain::CLOSE_TASK_LABEL) => (
            t!(lang, "Close the task", "Cerrar la tarea"),
            Some(t!(lang, "Its work after the merge counts as done: the task is completed and Alondra closes its session",
                "Su trabajo tras el merge se da por terminado: la tarea se completa y Alondra cierra su sesión")),
        ),
        (crate::domain::CANCEL_TASK, crate::domain::CANCEL_TASK_LABEL) => (
            t!(lang, "Cancel the task", "Cancelar la tarea"),
            Some(t!(lang, "The task ends and its execution is closed", "La tarea termina y se cierra su ejecución")),
        ),
        ("fixed", "I've fixed it") => (
            t!(lang, "I've fixed it", "Ya lo he arreglado"),
            Some(t!(lang, "Alondra measures again now and wakes the nodes if the Mac is fine", "Alondra vuelve a medir ahora y despierta a los nodos si el Mac está bien")),
        ),
        ("resume_now", "Resume now") => (t!(lang, "Resume now", "Reanudar ahora"), None),
        ("stay_paused", "Stay paused") => (t!(lang, "Stay paused", "Seguir en pausa"), None),
        ("done", "Done") => (t!(lang, "Done", "Hecho"), None),
        ("cannot", "I can't do it") => (t!(lang, "I can't do it", "No puedo hacerlo"), None),
        ("works", "Works") => (t!(lang, "Works", "Funciona"), None),
        ("fails", "Doesn't work") => (t!(lang, "Doesn't work", "No funciona"), None),
        ("skip", "I can't provide it") => (t!(lang, "I can't provide it", "No puedo dárselo"), None),
        ("provide", "Send the file") => (t!(lang, "Send the file", "Enviar el archivo"), None),
        ("provide", "Send the text") => (t!(lang, "Send the text", "Enviar el texto"), None),
        ("provide", "Send the secret") => (t!(lang, "Send the secret", "Enviar el secreto"), None),
        (crate::orchestrator_batch::BATCH_CONTINUE, crate::orchestrator_batch::BATCH_CONTINUE_LABEL) => (
            t!(lang, "Continue without running it again", "Seguir sin repetirlo"),
            Some(t!(lang,
                "Nothing is done twice. The orchestrator knows it stopped and picks up the requests still without a task.",
                "No se hace nada dos veces. El orquestador sabe que se cortó y retoma las peticiones que sigan sin tarea.")),
        ),
        (crate::orchestrator_batch::BATCH_RETRY, crate::orchestrator_batch::BATCH_RETRY_LABEL) => (
            t!(lang, "Run it again from the start", "Repetirlo desde el principio"),
            Some(t!(lang,
                "It handles the same messages again. Choose it only if Tasks shows none of what it did; otherwise it is done twice.",
                "Vuelve a atender los mismos mensajes. Elígelo solo si en Tareas no ves nada de lo que hizo; si no, se hará dos veces.")),
        ),
        _ => return (option.label.clone(), option.description.clone()),
    };
    (label.to_owned(), description.map(str::to_owned).or_else(|| option.description.clone()))
}

fn answer_actions(
    item: &InboxItem,
    interaction: &Interaction,
    available: Option<&AvailableAction>,
    context: &RenderContext<'_>,
) -> (Vec<Action>, Vec<Action>) {
    let lang = context.lang;
    let kind = item.card.kind;
    let hold = context.domain.holds.get(&interaction.id).copied();
    let options = match &interaction.payload {
        InteractionPayload::Question { options, .. } | InteractionPayload::ActionRequest { options, .. } => options.as_slice(),
        _ => &[],
    };
    let revision = interaction.revision;
    if hold == Some(Hold::Plan) {
        return (plan_actions(interaction, available, context), vec![]);
    }
    let mut buttons: Vec<Action> = vec![];
    let mut ordered: Vec<&QuestionOption> = options.iter().collect();
    if let Some(recommended) = interaction.card.recommended.as_deref() {
        if let Some(at) = ordered.iter().position(|option| option.id == recommended) {
            let first = ordered.remove(at);
            ordered.insert(0, first);
        }
    }
    for (index, option) in ordered.into_iter().enumerate() {
        let Some(id) = action_id(Verb::Ans, &interaction.id.0, revision, None) else { continue };
        let recommended = interaction.card.recommended.as_deref() == Some(option.id.as_str());
        let (fixed_label, fixed_description) = fixed_option_text(interaction, option, lang);
        let label = match hold.filter(|hold| *hold != Hold::Question) {
            Some(hold) => held_option(hold, option, lang),
            None if recommended => format!("★ {fixed_label}"),
            None => fixed_label,
        };
        let role = match hold {
            Some(hold) if hold.approves(&option.id) => if kind == CardKind::Authorize { Role::Risk } else { Role::Advance },
            Some(Hold::Question) | None if kind == CardKind::Decide => if recommended { Role::Advance } else { Role::Neutral },
            Some(Hold::Question) | None => if index == 0 { Role::Advance } else { Role::Neutral },
            Some(_) => Role::Neutral,
        };
        let mut action = button(id.to_string(), &label, role);
        action.option_id = Some(option.id.clone());
        action.description = match hold {
            None | Some(Hold::Question) => fixed_description,
            Some(Hold::Permission) if option.id == Hold::ALWAYS => interaction.card.allow_always.as_ref()
                .and_then(|offer| super::allow_always::description(offer, lang)),
            Some(_) => None,
        };
        action.stamp = Some(t!(lang, "Answered", "Respondida").into());
        buttons.push(with_availability(action, available, lang));
    }
    if let Some(server) = crate::domain::McpServerKey::of_card(interaction) {
        let name = server.name.as_str();
        let mut own = mcp_actions(interaction, context);
        let reconnect = own.pop();
        own.extend(buttons.drain(..).map(|mut button| {
            button.role = Some(Role::Neutral);
            button.stamp = Some(tf!(lang, "The tasks go on without {name}", "Las tareas siguen sin {name}"));
            button
        }));
        own.extend(reconnect);
        buttons = own;
    }
    if matches!(interaction.subject, Some(InteractionSubject::DocumentReview { .. })) {
        buttons.extend(request_changes(Verb::Changes, &interaction.id.0, revision, None, true, available, context));
    }
    (buttons, vec![])
}

fn request_changes(
    verb: Verb,
    subject: &str,
    revision: u64,
    extra: Option<String>,
    comments: bool,
    available: Option<&AvailableAction>,
    context: &RenderContext<'_>,
) -> Option<Action> {
    let lang = context.lang;
    let id = action_id(verb, subject, revision, extra)?;
    let mut action = button(id.to_string(), t!(lang, "Request changes", "Pedir cambios"), Role::Neutral);
    action.input = text_input(t!(lang, "Describe the changes", "Describe los cambios"), true);
    action.attachments = true;
    action.comments = comments.then_some(CommentSpec { kind: super::CommentKind::Blocks });
    action.stamp = Some(t!(lang, "Changes requested", "Cambios solicitados").into());
    Some(with_availability(action, available, lang))
}

/// A plan Claude Code holds: approve it, or send comments on its blocks as the changes to make.
fn plan_actions(interaction: &Interaction, available: Option<&AvailableAction>, context: &RenderContext<'_>) -> Vec<Action> {
    let lang = context.lang;
    let mut out = vec![];
    if let Some(id) = action_id(Verb::Accept, &interaction.id.0, interaction.revision, None) {
        let mut approve = button(id.to_string(), t!(lang, "Approve", "Aprobar"), Role::Advance);
        approve.stamp = Some(t!(lang, "Approved", "Aprobado").into());
        out.push(with_availability(approve, available, lang));
    }
    out.extend(request_changes(Verb::Changes, &interaction.id.0, interaction.revision, None, true, available, context));
    out
}

fn is_document_review(interaction: &Interaction) -> bool {
    reviewed_revision(interaction).is_some()
}

fn merge_extra(head: &str, presentation_id: &str) -> String {
    format!("{}-{presentation_id}", head.chars().take(8).collect::<String>())
}

fn delivery_actions(
    item: &InboxItem,
    interaction: &Interaction,
    context: &RenderContext<'_>,
) -> (Vec<Action>, Vec<Action>) {
    let lang = context.lang;
    let domain = context.domain;
    let find = |kind: &str| item.available_actions.iter().find(|action| action.kind == kind);
    let held = blocked_reason(item, context).is_some();
    let pr = pr_target(interaction, domain);
    let live = pr.and_then(|pr| context.live.pull_requests.iter().find(|detail| detail.id == pr.id.0));
    let document = is_document_review(interaction);
    let accept_label = if item.card.kind == CardKind::Know {
        t!(lang, "Seen", "Visto")
    } else if document {
        t!(lang, "Approve", "Aprobar")
    } else {
        t!(lang, "Accept", "Aceptar")
    };
    let accepted = matches!(&interaction.payload, InteractionPayload::Delivery { acceptance: Some(acceptance), .. } if acceptance.valid);
    let accept = |role: Role, label: &str| {
        if accepted {
            return None;
        }
        let id = action_id(Verb::Accept, &interaction.id.0, interaction.revision, None)?;
        let mut action = button(id.to_string(), label, role);
        action.stamp = Some(if document { t!(lang, "Approved", "Aprobada") } else { t!(lang, "Accepted", "Aceptada") }.into());
        Some(with_availability(action, find("accept_delivery"), lang))
    };
    let mut visible = vec![];
    let mut more = vec![];
    if pr.is_none() && interaction.card.plan.is_some() && item.card.kind != CardKind::Know {
        if !accepted {
            let choices = [
                (super::action_id::IMPLEMENT_PLAN, t!(lang, "Approve and start implementing", "Aprobar y empezar a implementar"), Role::Advance,
                    t!(lang, "Implementation requested", "Implementación pedida"), "implement_plan"),
                (super::action_id::ARCHIVE_PLAN, t!(lang, "Only archive", "Solo archivar"), Role::Neutral, t!(lang, "Archived", "Archivada"), "archive_plan"),
            ];
            for (extra, label, role, done, kind) in choices {
                if let Some(id) = action_id(Verb::Accept, &interaction.id.0, interaction.revision, Some(extra.to_owned())) {
                    let mut action = button(id.to_string(), label, role);
                    action.stamp = Some(done.into());
                    visible.push(with_availability(action, find(kind), lang));
                }
            }
        }
        visible.extend(request_changes(
            Verb::Changes, &interaction.id.0, interaction.revision, None, document, find("submit_feedback"), context,
        ));
        return (visible, more);
    }
    let Some(pr) = pr else {
        let role = if item.card.kind == CardKind::Know { Role::Neutral } else { Role::Advance };
        visible.extend(accept(role, accept_label));
        if item.card.kind != CardKind::Know {
            visible.extend(request_changes(
                Verb::Changes, &interaction.id.0, interaction.revision, None, document, find("submit_feedback"), context,
            ));
        }
        return (visible, more);
    };
    let revision = context.domain.revision;
    let conflicts = live.is_some_and(|detail| detail.conflicts);
    let stacked = live.is_some_and(|detail| detail.stacked_on.is_some());
    if held || conflicts || stacked {
        // The task shows what it waits for; the card has nothing to press until that is over.
    } else if let Some(available) = find("authorize_merge").filter(|available| available.enabled) {
        let head = available.expected_head.as_ref().map(|head| merge_extra(head, &interaction.id.0));
        if let Some(id) = action_id(Verb::Merge, &interaction.id.0, interaction.revision, head) {
            visible.push(merge_action(id.to_string(), pr, context));
        }
    } else if find("authorize_merge").is_none() {
        visible.extend(accept(Role::Advance, accept_label));
    }
    let head: String = pr.head.chars().take(8).collect();
    visible.extend(request_changes(
        Verb::Changes, &pr.id.0, revision, Some(head), false, find("submit_feedback"), context,
    ));
    if let Some(action) = visible.last_mut().filter(|action| action.id.starts_with("changes:")) {
        action.comments = Some(CommentSpec { kind: super::CommentKind::DiffLines });
        action.link = Some(Link::View { key: format!("pull_request:{}", pr.id) });
    }
    if pr.state == PrState::Open {
        more.extend(close_unmerged_action(pr, &interaction.id.0, interaction.revision, context));
    }
    (visible, more)
}

fn method_text(method: MergeMethod, lang: Lang) -> (&'static str, &'static str) {
    match method {
        MergeMethod::Squash => (t!(lang, "Squash and merge", "Squash and merge"), t!(lang, "squash", "squash")),
        MergeMethod::Merge => (t!(lang, "Merge commit", "Merge commit"), t!(lang, "a merge commit", "un merge commit")),
        MergeMethod::Rebase => (t!(lang, "Rebase and merge", "Rebase and merge"), t!(lang, "rebase", "rebase")),
    }
}

fn method_id(method: MergeMethod) -> &'static str {
    match method {
        MergeMethod::Squash => "squash",
        MergeMethod::Merge => "merge",
        MergeMethod::Rebase => "rebase",
    }
}

/// Merge the PR with a way its repository admits: a plain button when only one is, a menu of
/// described ways when there are several. The server words every label; clients paint them.
fn merge_action(id: String, pr: &PullRequest, context: &RenderContext<'_>) -> Action {
    let lang = context.lang;
    let mut merge = button(id, t!(lang, "Merge", "Hacer merge"), Role::Advance);
    merge.stamp = Some(t!(lang, "Merging…", "Haciendo merge…").into());
    if let [only] = pr.allowed_methods.as_slice() {
        merge.label = tf!(lang, "Merge PR #{number}", "Hacer merge de la PR #{number}", number = pr.number);
        merge.option_id = Some(method_id(*only).into());
        merge.description = Some(tf!(lang, "{method}: the only way this repository allows",
            "{method}: la única forma que permite este repositorio", method = method_text(*only, lang).0));
        return merge;
    }
    merge.presentation = Presentation::Menu;
    merge.options = pr.allowed_methods.iter().enumerate().map(|(index, method)| ActionOption {
        id: method_id(*method).into(),
        label: method_text(*method, lang).0.into(),
        description: Some(match method {
            MergeMethod::Squash => tf!(lang, "One commit on {base}", "Un solo commit en {base}", base = pr.base),
            MergeMethod::Merge => t!(lang, "Keeps every commit", "Conserva todos los commits").into(),
            MergeMethod::Rebase => tf!(lang, "Commits replayed on {base}", "Los commits se reaplican sobre {base}", base = pr.base),
        }),
        style: Style::Secondary,
        input: Input::None,
        key: (context.client != Client::Ios && index < 9).then(|| (index + 1).to_string()),
    }).collect();
    merge
}

/// The way out when the user does not want the PR: close it on GitHub and cancel the task.
fn close_unmerged_action(pr: &PullRequest, subject: &str, revision: u64, context: &RenderContext<'_>) -> Option<Action> {
    let lang = context.lang;
    let id = action_id(Verb::CloseUnmerged, subject, revision, None)?;
    let mut action = button(id.to_string(), t!(lang, "Close without merging", "Cerrar sin hacer merge"), Role::Risk);
    action.stamp = Some(t!(lang, "Closing…", "Cerrando…").into());
    action.confirm = Some(super::Confirm {
        title: tf!(lang, "Close PR #{number} without merging?", "¿Cerrar la PR #{number} sin hacer merge?", number = pr.number),
        message: t!(lang, "Alondra closes the PR on GitHub and cancels this task. Nothing is merged.",
            "Alondra cierra la PR en GitHub y cancela esta tarea. No se hace merge de nada.").into(),
        button: t!(lang, "Close PR", "Cerrar la PR").into(),
    });
    Some(action)
}

/// Always behind "More": open the PR, ask the author to say it again, and the terminal's local
/// shortcuts.
/// "Change answers": while the rest of the questions asked together are still open, an answer
/// already given can be taken back from any card still waiting; its card comes back to the stack.
fn change_answers(interaction: &Interaction, context: &RenderContext<'_>) -> Option<Action> {
    let lang = context.lang;
    let domain = context.domain;
    interaction.card.group.as_ref()?;
    if !domain.open(interaction) || context.index.group_complete(domain, interaction) {
        return None;
    }
    let answered: Vec<ActionOption> = context.index.group_members(domain, interaction).into_iter().filter_map(|member| {
        let InteractionPayload::Question { options, answer: Some(answer), .. } = &member.payload else { return None };
        let said = options.iter().find(|option| option.id == answer.option_id).map(|option| option.label.clone())
            .or_else(|| answer.inputs.values().find_map(|value| match value {
                crate::domain::InputValue::Text(text) if !text.trim().is_empty() => Some(clip(text, 40)),
                _ => None,
            }))?;
        Some(ActionOption {
            id: member.id.0.clone(),
            label: format!("{} → {}", clip(&member.card.need, 50), said),
            description: None,
            style: Style::Secondary,
            input: Input::None,
            key: None,
        })
    }).collect();
    if answered.is_empty() {
        return None;
    }
    let id = action_id(Verb::Retake, &interaction.id.0, interaction.revision, None)?;
    let mut action = button(id.to_string(), t!(lang, "Change answers", "Cambiar respuestas"), Role::Neutral);
    action.presentation = Presentation::Options;
    action.options = answered;
    action.stamp = Some(t!(lang, "Question reopened", "Pregunta reabierta").into());
    action.key = (context.client != Client::Ios).then(|| "x".into());
    Some(action)
}

fn extra_actions(item: &InboxItem, interaction: Option<&Interaction>, context: &RenderContext<'_>) -> Vec<Action> {
    let lang = context.lang;
    let mut out = vec![];
    let Some(interaction) = interaction else { return out };
    let pr = pr_target(interaction, context.domain);
    let live = pr.and_then(|pr| context.live.pull_requests.iter().find(|detail| detail.id == pr.id.0));
    if let Some(url) = live.and_then(|detail| detail.url.as_ref()) {
        let mut open = button(String::new(), t!(lang, "Open on GitHub", "Abrir en GitHub"), Role::Neutral);
        open.style = Style::Plain;
        open.link = Some(Link::Url { url: url.clone() });
        open.key = (context.client != Client::Ios).then(|| "w".into());
        out.push(open);
    }
    if let Some(available) = item.available_actions.iter().find(|action| action.kind == "request_clarification") {
        if let Some(id) = action_id(Verb::Clarify, &interaction.id.0, interaction.revision, None) {
            let option = |reason: ClarificationReason, label: &str| ActionOption {
                id: super::action_id::clarification_name(reason).into(),
                label: label.into(),
                description: None,
                style: Style::Secondary,
                input: Input::None,
                key: None,
            };
            let mut ask = button(id.to_string(), t!(lang, "Request clarification", "Pedir aclaración"), Role::Neutral);
            ask.presentation = Presentation::Options;
            ask.options = vec![
                option(ClarificationReason::Unclear, t!(lang, "It is unclear", "No se entiende")),
                option(ClarificationReason::Contradicts, t!(lang, "It contradicts something", "Se contradice")),
                option(ClarificationReason::MissingContext, t!(lang, "It lacks context", "Le falta contexto")),
            ];
            ask.input = text_input(t!(lang, "What should it clarify?", "¿Qué debe aclarar?"), false);
            ask.stamp = Some(t!(lang, "Clarification requested", "Aclaración solicitada").into());
            ask.key = (context.client != Client::Ios).then(|| "c".into());
            out.push(with_availability(ask, Some(available), lang));
        }
    }
    out.extend(change_answers(interaction, context));
    if context.client == Client::Tui && context.transport == super::Transport::Loopback {
        if let Some(pr) = pr {
            out.push(local_action(
                t!(lang, "Review local clone", "Revisar copia local"),
                "d",
                LocalLink::ReviewClone { repo: pr.repository.clone(), branch: pr.head.clone(), pr: pr.number, pr_id: pr.id.0.clone() },
            ));
        }
    }
    out
}

fn local_action(label: &str, key: &str, local: LocalLink) -> Action {
    let mut action = button(String::new(), label, Role::Neutral);
    action.style = Style::Plain;
    action.key = Some(key.into());
    action.link = Some(Link::Local { local });
    action
}

/// Text to the node (or the orchestrator) the card came from; every card has it but a task's start
/// problem, which no node is behind, and a machine card, which Alondra raises itself. Under an
/// open question it is the answer to that question, the orchestrator's included.
fn reply_action(item: &InboxItem, interaction: Option<&Interaction>, context: &RenderContext<'_>) -> Option<Action> {
    let lang = context.lang;
    let interaction = interaction.filter(|interaction| {
        start_problem_of(interaction).is_none() && !is_machine_card(interaction) && crate::domain::ClaudeHold::of(&interaction.id).is_none()
            && crate::domain::McpServerKey::of_card(interaction).is_none()
    })?;
    let answers = answered_by_reply(interaction)
        && (interaction.author != Author::System || matches!(interaction.scope, Scope::Task { .. }));
    let held_placeholder = match answers.then(|| context.domain.holds.get(&interaction.id).copied()).flatten() {
        Some(Hold::Permission) => Some(t!(lang, "Deny it with a reason…", "Denegar con un motivo…").to_owned()),
        Some(Hold::Plan) => Some(t!(lang, "Write what to change…", "Escribe qué cambiar…").to_owned()),
        Some(Hold::Elicitation) => Some(match &interaction.payload {
            InteractionPayload::Question { options, .. } => options.iter().find(|option| Hold::Elicitation.approves(&option.id))
                .and_then(|option| option.description.clone())
                .unwrap_or_else(|| t!(lang, "Write your answer…", "Escribe tu respuesta…").to_owned()),
            _ => t!(lang, "Write your answer…", "Escribe tu respuesta…").to_owned(),
        }),
        Some(Hold::Question) | None => None,
    };
    let (id, placeholder) = if let Some(placeholder) = held_placeholder {
        (action_id(Verb::Reply, &interaction.id.0, interaction.revision, None)?, placeholder)
    } else if answers && item.card.kind == CardKind::Provide {
        (
            action_id(Verb::Reply, &interaction.id.0, interaction.revision, None)?,
            t!(lang, "Write it here or attach the file…", "Escríbelo aquí o adjunta el archivo…").into(),
        )
    } else if interaction.author == Author::System && interaction.id.0.starts_with(crate::domain::HANDOFF_DOUBT_PREFIX) {
        (
            action_id(Verb::Reply, &interaction.id.0, interaction.revision, None)?,
            t!(lang, "Write what to send instead…", "Escribe qué enviarle en su lugar…").into(),
        )
    } else if matches!(interaction.scope, Scope::Task { .. }) {
        (
            action_id(Verb::Reply, &interaction.id.0, interaction.revision, None)?,
            tf!(lang, "Reply to {node}…", "Responder a {node}…", node = node_name(item, context)),
        )
    } else if answers {
        (action_id(Verb::Reply, &interaction.id.0, interaction.revision, None)?, t!(lang, "Reply…", "Responder…").into())
    } else {
        (
            action_id(Verb::Msg, "view", context.domain.revision, None)?,
            t!(lang, "Reply…", "Responder…").into(),
        )
    };
    let mut action = button(id.to_string(), t!(lang, "Reply", "Responder"), Role::Neutral);
    action.input = text_input(&placeholder, item.card.kind != CardKind::Provide);
    action.attachments = true;
    action.stamp = Some(if answers { t!(lang, "You answered", "Respondiste") } else { t!(lang, "Sent", "Enviado") }.into());
    action.key = (context.client != Client::Ios).then(|| "r".into());
    action.enabled = !interaction.withdrawn;
    Some(action)
}

/// Words written under an open question to the user are its answer, whoever asked it, held by
/// Claude Code or not.
fn answered_by_reply(interaction: &Interaction) -> bool {
    matches!(interaction.recipient, Recipient::User)
        && matches!(interaction.payload, InteractionPayload::Question { answer: None, .. } | InteractionPayload::ActionRequest { answer: None, .. })
}

// ---------------------------------------------------------------------------------------------
// The detail: content, context, history
// ---------------------------------------------------------------------------------------------

fn fenced(text: &str, language: &str) -> String {
    let longest_run = text.split(|c| c != '`').map(str::len).max().unwrap_or(0);
    let fence = "`".repeat((longest_run + 1).max(3));
    format!("{fence}{language}\n{}\n{fence}", text.trim_end())
}

fn markdown(text: String) -> Item {
    Item::Markdown(Markdown { text, truncated: false, more: None, commentable: false })
}

fn commentable_markdown(text: String) -> Item {
    Item::Markdown(Markdown { text, truncated: false, more: None, commentable: true })
}

fn content_items(item: &InboxItem, context: &RenderContext<'_>) -> Vec<Item> {
    let lang = context.lang;
    let domain = context.domain;
    let Some(interaction) = interaction_of(item, domain) else { return vec![] };
    let mut out = vec![];
    let shown = interaction.shown_content();
    let content = shown.as_deref().filter(|content| !content.body.trim().is_empty());
    if let Some(revision_id) = reviewed_revision(interaction)
        && let Some(revision) = domain.document_revisions.get(revision_id)
        && let Some(document) = domain.documents.get(&revision.document_id)
    {
        out.push(Item::ContentRef(ContentRef {
            kind: super::ContentKind::Document,
            title: document.title.clone(),
            size: 0,
            digest: Some(revision.content_digest.clone()),
            fetch: format!("/document-revisions/{revision_id}/content"),
            commentable: true,
        }));
        out.extend(proposed_plan(interaction, lang));
        out.extend(kept_at(interaction, lang));
        return out;
    }
    if interaction.author == Author::System
        && interaction.id.0.starts_with(crate::close_problem::CLOSE_PROBLEM_PREFIX)
        && let Some(card) = crate::close_problem::CloseProblem::from_body(&interaction.body)
    {
        out.push(markdown(card.detail(lang)));
        return out;
    }
    if let Some(content) = start_problem_card(interaction, domain, lang).and_then(|card| card.content) {
        out.push(markdown(content.body));
        return out;
    }
    if interaction.author == Author::System
        && interaction.id.0.starts_with(crate::orchestrator_cards::STUCK_PROBLEM_PREFIX)
        && let Some(card) = crate::orchestrator_cards::StuckProblem::from_body(&interaction.body)
    {
        let detail = card.detail(lang);
        if !detail.is_empty() {
            out.push(markdown(detail));
        }
        return out;
    }
    if interaction.author == Author::System
        && interaction.id.0.starts_with(crate::orchestrator_batch::BATCH_PROBLEM_PREFIX)
        && let Some(card) = crate::orchestrator_batch::BatchProblem::from_body(&interaction.body)
    {
        out.push(markdown(card.detail(lang)));
        return out;
    }
    let plan_held = domain.holds.get(&interaction.id) == Some(&Hold::Plan);
    if let Some(content) = content {
        if plan_held {
            out.push(commentable_markdown(content.body.clone()));
            return out;
        }
        out.push(markdown(match content.kind {
            DomainContent::Command => fenced(&content.body, ""),
            DomainContent::Diff if summary::diff_stats(&content.body).is_some() => fenced(&content.body, "diff"),
            _ => content.body.clone(),
        }));
    }
    out.extend(proposed_plan(interaction, lang));
    out.extend(kept_at(interaction, lang));
    if domain.holds.get(&interaction.id) == Some(&Hold::Permission) {
        if let Some(line) = interaction.card.allow_always.as_ref().and_then(|offer| super::allow_always::refusal(offer, lang)) {
            out.push(markdown(line));
        }
        if context.live.allow_always_missing.contains(&interaction.id.0) {
            out.push(markdown(t!(lang, "Claude Code did not save these rules, so this command will keep asking.",
                "Claude Code no guardó estas reglas, así que este comando seguirá preguntando.").into()));
        }
    }
    out
}

/// The plan a final result proposes, under its own heading after the report.
fn proposed_plan(interaction: &Interaction, lang: Lang) -> Option<Item> {
    let plan = interaction.card.plan.as_deref().map(str::trim).filter(|plan| !plan.is_empty())?;
    let under_heading: Vec<String> = plan.lines().map(|line| {
        let level = line.bytes().take_while(|b| *b == b'#').count();
        if (1..=6).contains(&level) && line[level..].starts_with(' ') {
            format!("{}{}", "#".repeat((level + 2).min(6)), &line[level..])
        } else {
            line.to_owned()
        }
    }).collect();
    Some(markdown(format!("---\n\n## {}\n\n{}", t!(lang, "Proposed plan", "Plan propuesto"), under_heading.join("\n"))))
}

/// Where Alondra keeps the whole result, which outlives the node's session.
fn kept_at(interaction: &Interaction, lang: Lang) -> Option<Item> {
    let path = interaction.card.kept_at.as_deref()?;
    Some(markdown(format!("{} `{path}`", t!(lang, "Kept in", "Guardado en"))))
}

fn live_pr<'a>(pr: &PullRequest, context: &'a RenderContext<'_>) -> Option<&'a super::LivePullRequest> {
    context.live.pull_requests.iter().find(|detail| detail.id == pr.id.0)
}

/// Task ids are internal: wherever a node wrote one, the reader sees the task's title.
fn humanize_task_ids(text: &str, domain: &DomainState) -> String {
    let is_id_char = |c: char| c.is_ascii_alphanumeric() || c == '-' || c == '_';
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = rest.find(is_id_char) {
        out.push_str(&rest[..start]);
        let tail = &rest[start..];
        let end = tail.find(|c: char| !is_id_char(c)).unwrap_or(tail.len());
        let word = &tail[..end];
        match domain.tasks.get(&word.into()) {
            Some(task) if word.starts_with("t-") => out.push_str(&task.title),
            _ => out.push_str(word),
        }
        rest = &tail[end..];
    }
    out.push_str(rest);
    out
}

/// One line for everything the PR table used to say: repository, branches, size and checks.
fn pr_meta(pr: &PullRequest, context: &RenderContext<'_>) -> String {
    let lang = context.lang;
    let live = live_pr(pr, context);
    let branches = match live.and_then(|detail| detail.head_ref.as_deref()) {
        Some(head) => format!("{head} → {}", pr.base),
        None => format!("→ {}", pr.base),
    };
    let size = live.filter(|detail| detail.changed_files > 0).map(|detail| changes_summary(detail, lang));
    let checks = live.map(|detail| match detail.checks_passed {
        Some(true) => t!(lang, "Checks passed", "Comprobaciones superadas"),
        Some(false) => t!(lang, "Checks failing", "Comprobaciones fallidas"),
        None => t!(lang, "Checking…", "Comprobando…"),
    });
    [Some(pr.repository.clone()), Some(branches), size, checks.map(str::to_owned)]
        .into_iter()
        .flatten()
        .collect::<Vec<_>>()
        .join(" · ")
}

/// The node's report without the line that only points back at this PR.
fn without_pr_link(report: &str, pr: &PullRequest) -> String {
    let marker = format!("/pull/{}", pr.number);
    let kept = report
        .lines()
        .filter(|line| !(line.trim_start().to_ascii_lowercase().starts_with("pr:") && line.contains(&marker)))
        .collect::<Vec<_>>()
        .join("\n");
    kept.trim().to_owned()
}

/// A PR card: the diff first, what failed in the checks, then the description folded away.
fn pr_sections(item: &InboxItem, context: &RenderContext<'_>) -> Option<Vec<super::Section>> {
    let lang = context.lang;
    let domain = context.domain;
    let interaction = interaction_of(item, domain)?;
    let pr = pr_target(interaction, domain)?;
    let (facts, checks, _) = pr_overview(pr, context);
    let shown = interaction.shown_content();
    let content = shown.as_deref().filter(|content| content.kind != DomainContent::Diff);
    let report = content.map(|content| without_pr_link(&content.body, pr)).filter(|text| !text.is_empty());
    let description = live_pr(pr, context)
        .and_then(|detail| detail.body.clone())
        .map(|text| text.trim().to_owned())
        .filter(|text| !text.is_empty())
        .filter(|text| report.as_deref().is_none_or(|report| !report.contains(text.as_str()) && !text.contains(report)));
    let mut sections = vec![];
    if let Some(failing) = checks.filter(|checks| checks.items.iter().any(|check| check.state != CheckState::Pass)) {
        sections.push(super::common::section("checks", None, vec![Item::Checks(failing)]));
    }
    let stack: Vec<Fact> = facts.items.into_iter().filter(|fact| fact.link.is_some()).collect();
    if !stack.is_empty() {
        sections.push(super::common::section("stack", None, vec![Item::Facts(Facts { items: stack })]));
    }
    sections.push(super::common::section(
        "diff",
        Some(t!(lang, "Diff", "Diferencias").into()),
        vec![Item::ContentRef(ContentRef {
            kind: super::ContentKind::Diff,
            title: t!(lang, "Diff", "Diferencias").into(),
            size: 0,
            digest: Some(pr.head.clone()),
            fetch: format!("/pull-requests/{}/diff", super::id::path_segment(&pr.id.0)),
            commentable: true,
        })],
    ));
    let text = [report, description].into_iter().flatten().collect::<Vec<_>>().join("\n\n");
    if !text.is_empty() {
        sections.push(super::common::section(
            "description",
            Some(t!(lang, "Description", "Descripción").into()),
            vec![markdown(text)],
        ));
    }
    Some(sections)
}

fn context_items(item: &InboxItem, context: &RenderContext<'_>) -> Vec<Item> {
    let task = item.task_id.as_ref().and_then(|id| context.domain.tasks.get(id)).map(|task| task.title.clone());
    let node = item.agent_id.as_ref().map(|_| node_name(item, context));
    let node = node.filter(|node| task.as_deref().is_none_or(|task| !task.trim().eq_ignore_ascii_case(node.trim())));
    let line = [task, node].into_iter().flatten().collect::<Vec<_>>().join(" · ");
    if line.is_empty() {
        return vec![];
    }
    let link = item.task_id.as_ref().map(|id| Link::View { key: format!("task:{id}") });
    vec![Item::Facts(Facts { items: vec![Fact { label: t!(context.lang, "Task", "Tarea").into(), value: line, tone: None, link }] })]
}

/// The points of the task's approved plan its final delivery marked done, each with the evidence
/// under it.
fn plan_points(item: &InboxItem, context: &RenderContext<'_>) -> Option<(String, Vec<Item>)> {
    let lang = context.lang;
    let domain = context.domain;
    let interaction = interaction_of(item, domain)?;
    let marks = domain.checklist_marks.get(&interaction.id)?;
    let Scope::Task { task_id } = &interaction.scope else { return None };
    let items = domain.checklists.get(task_id).map(|checklist| checklist.items.as_slice()).unwrap_or_default();
    let repository = domain.tasks.get(task_id).and_then(|task| task.pr_id.as_ref()).and_then(|pr| domain.pull_requests.get(pr)).map(|pr| {
        context.live.pull_requests.iter().find(|detail| detail.id == pr.id.0).and_then(|detail| detail.url.as_deref())
            .and_then(|url| url.rsplit_once("/pull/")).map(|(base, _)| base.to_owned())
            .unwrap_or_else(|| format!("https://{}/{}", pr.host, pr.repository))
    });
    let bare = |id: String, icon: Icon, title: String, subtitle: Option<String>| {
        let mut row: Row = super::common::row(&id, "", icon, title, subtitle);
        row.link = None;
        row
    };
    let points = marks.iter().map(|mark| {
        let text = items.iter().find(|point| point.id == mark.item).map(|point| point.text.clone())
            .unwrap_or_else(|| tf!(lang, "Point {n}", "Punto {n}", n = mark.item));
        let mut row = bare(format!("plan-{}", mark.item), Icon::Check, text, None);
        row.children = mark.evidence.iter().enumerate().map(|(n, evidence)| {
            let value = evidence.value.trim();
            let (icon, label, shown) = match evidence.kind {
                EvidenceKind::Commit => (Icon::Branch, "Commit", &value[..value.len().min(7)]),
                EvidenceKind::Test => (Icon::Check, "Test", value),
                EvidenceKind::Command => (Icon::Action, t!(lang, "Command", "Comando"), value),
                EvidenceKind::Screenshot => (Icon::Attachment, t!(lang, "Screenshot", "Captura"),
                    interaction.attachments.iter().find(|a| a.id == value).map_or(value, |a| a.name.as_str())),
                EvidenceKind::Pr => (Icon::PullRequest, "PR", value),
                EvidenceKind::File => (Icon::Document, t!(lang, "File", "Archivo"), value),
                EvidenceKind::Link => (Icon::Notice, t!(lang, "Link", "Enlace"), value),
            };
            let mut child = bare(format!("plan-{}-{n}", mark.item), icon, format!("{label} · {}", clip(shown, 120)), evidence.detail.as_deref().map(|d| clip(d, 240)));
            child.depth = 1;
            child.link = match evidence.kind {
                EvidenceKind::Commit => repository.as_ref().map(|base| Link::Url { url: format!("{base}/commit/{value}") }),
                EvidenceKind::Link if value.starts_with("https://") || value.starts_with("http://") => Some(Link::Url { url: value.into() }),
                _ => None,
            };
            child
        }).collect();
        Item::Row(row)
    }).collect::<Vec<_>>();
    let total = items.len().max(marks.len());
    let title = tf!(lang, "Approved plan · {done} of {total} done", "Plan aprobado · {done} de {total} hechos", done = marks.len(), total = total);
    Some((title, points))
}

fn history_rows(item: &InboxItem, context: &RenderContext<'_>) -> Vec<Item> {
    let lang = context.lang;
    let Some(interaction) = interaction_of(item, context.domain) else { return vec![] };
    let mut rows = vec![];
    let mut row = |id: String, icon: Icon, title: String, subtitle: Option<String>| {
        let mut row: Row = super::common::row(&id, "", icon, title, subtitle);
        row.link = None;
        rows.push(Item::Row(row));
    };
    if let Some(clarification) = &interaction.clarification {
        row(
            "clarification".into(),
            Icon::History,
            t!(lang, "You asked for clarification", "Pediste una aclaración").into(),
            clarification.text.as_ref().map(|text| clip(text, 240)),
        );
    }
    match &interaction.payload {
        InteractionPayload::Question { options, answer: Some(answer) }
        | InteractionPayload::ActionRequest { options, answer: Some(answer) } => {
            let label = options.iter().find(|option| option.id == answer.option_id).map(|option| fixed_option_text(interaction, option, lang).0);
            let text = answer.inputs.values().find_map(|value| match value {
                crate::domain::InputValue::Text(text) => Some(clip(text, 240)),
                _ => None,
            });
            row(
                "answer".into(),
                Icon::Check,
                match label {
                    Some(label) => tf!(lang, "Answered · {option}", "Respondida · {option}", option = label),
                    None => t!(lang, "Answered", "Respondida").into(),
                },
                text,
            );
        }
        InteractionPayload::Delivery { acceptance, feedback, .. } => {
            for (index, feedback) in feedback.iter().enumerate() {
                use crate::domain::Instruction;
                if let Some((number, reason)) = crate::domain::merge_failure(&feedback.body) {
                    row(format!("feedback-{index}"), Icon::History,
                        tf!(lang, "Alondra couldn't merge #{number}", "Alondra no pudo hacer merge de la #{number}", number = number),
                        Some(clip(reason, 240)));
                    continue;
                }
                let title = match feedback.instruction {
                    Instruction::Changes => t!(lang, "You asked for changes", "Pediste cambios"),
                    Instruction::Question => t!(lang, "You asked a question", "Hiciste una pregunta"),
                    Instruction::Reply => t!(lang, "You replied", "Respondiste"),
                    Instruction::Conflicts => t!(lang, "Conflicts were reported", "Se comunicaron los conflictos"),
                };
                row(format!("feedback-{index}"), Icon::History, title.into(), Some(clip(&feedback.body, 240)));
            }
            if acceptance.as_ref().is_some_and(|acceptance| acceptance.valid) {
                row("acceptance".into(), Icon::Check, t!(lang, "You accepted it", "Lo aceptaste").into(), None);
            }
        }
        _ => {}
    }
    rows
}

// ---------------------------------------------------------------------------------------------
// Pull requests
// ---------------------------------------------------------------------------------------------

pub(super) fn changes_summary(detail: &super::LivePullRequest, lang: Lang) -> String {
    summary::changes(lang, detail.changed_files, detail.additions, detail.deletions)
}

fn pr_badges(pr: &PullRequest, context: &RenderContext<'_>) -> Vec<Label> {
    let lang = context.lang;
    let live = context.live.pull_requests.iter().find(|detail| detail.id == pr.id.0);
    let mut badges = vec![];
    if live.is_some_and(|detail| detail.conflicts) {
        badges.push(Label { text: t!(lang, "Conflicts", "Conflictos").into(), tone: Tone::Warning });
    } else if live.is_some_and(|detail| detail.checks_passed.is_none()) {
        badges.push(Label { text: t!(lang, "GitHub is checking…", "GitHub está comprobando…").into(), tone: Tone::Info });
    }
    badges
}

/// A pull request's facts, checks and badges, for its card and for its own view.
pub(super) fn pr_overview(pr: &PullRequest, context: &RenderContext<'_>) -> (Facts, Option<Checks>, Vec<Label>) {
    let lang = context.lang;
    let live = context.live.pull_requests.iter().find(|detail| detail.id == pr.id.0);
    let fact = |label: &str, value: String| Fact { label: label.into(), value, tone: None, link: None };
    let mut facts = vec![
        fact(t!(lang, "Repository", "Repositorio"), format!("{} #{}", pr.repository, pr.number)),
        fact(t!(lang, "Base", "Base"), pr.base.clone()),
    ];
    if let Some(head) = live.and_then(|detail| detail.head_ref.clone()) {
        facts.push(fact(t!(lang, "Head", "Rama"), head));
    }
    if let Some(detail) = live.filter(|detail| detail.changed_files > 0) {
        facts.push(fact(t!(lang, "Changes", "Cambios"), changes_summary(detail, lang)));
    }
    if let Some(stacked) = live.and_then(|detail| detail.stacked_on.as_deref()) {
        facts.push(Fact {
            label: t!(lang, "Stacked on", "Depende de").into(),
            value: context
                .domain
                .pull_requests
                .get(&PrId(stacked.into()))
                .map_or_else(|| stacked.into(), |upstream| format!("#{}", upstream.number)),
            tone: Some(Tone::Warning),
            link: Some(Link::View { key: format!("pull_request:{stacked}") }),
        });
    }
    let checks = live
        .filter(|detail| !detail.checks.is_empty())
        .map(|detail| Checks {
            items: detail.checks.iter().map(|check| Check { name: check.name.clone(), state: check.state }).collect(),
            link: None,
        })
        .or_else(|| {
            live.and_then(|detail| detail.checks_passed).map(|passed| Checks {
                items: vec![Check {
                    name: t!(lang, "Checks", "Comprobaciones").into(),
                    state: if passed { CheckState::Pass } else { CheckState::Fail },
                }],
                link: None,
            })
        });
    (Facts { items: facts }, checks, pr_badges(pr, context))
}
