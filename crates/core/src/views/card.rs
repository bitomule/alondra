//! One card per interaction: what the user must do (`need`), why, one line of content, at most
//! three buttons and a reply. The detail adds the whole content, its context and its history.

use super::{
    Action, ActionOption, Attachment, AttachmentKind, BlockChip, BuildError, Card, Check, CheckState,
    Checks, Client, CommentSpec, ContentRef, ContentRow, ContextLine, Detail, Fact, Facts, Icon, Input, Item,
    Label, Lang, Link, LocalLink, Markdown, Presentation, RenderContext, Role, Row, Stamp, Style,
    TimeFormat, Tone, VIEWS_PROTOCOL, View,
    action_id::{ActionId, Verb},
    summary,
    text::{t, tf},
};
use crate::domain_v2::{
    AvailableAction, Author, CardKind, CardSpec, ClarificationReason, ContentKind as DomainContent,
    DeliveryTarget, DomainProjection, DomainState, Hold, InboxItem, InboxSubject, Interaction, InteractionId,
    InteractionPayload, InteractionSubject, PrId, PullRequest, QuestionGroup, QuestionOption, Recipient, Scope, clip,
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
) -> Option<&'a crate::domain_v2::DocumentRevisionId> {
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
/// A delivery that carries a pull request or a document: the node waits for a review of it.
pub(super) fn is_review_delivery(item: &InboxItem, domain: &DomainState) -> bool {
    interaction_of(item, domain).is_some_and(|interaction| {
        matches!(interaction.payload, InteractionPayload::Delivery { .. })
            && (pr_target(interaction, domain).is_some() || reviewed_revision(interaction).is_some())
    })
}
/// The pending card that stands for a pull request, whichever way it is addressed.
pub(super) fn item_for_pr<'a>(projection: &'a DomainProjection, pr_id: &PrId, domain: &DomainState) -> Option<&'a InboxItem> {
    projection.inbox.iter().find(|item| {
        interaction_of(item, domain).is_some_and(|i| {
            targets_of(i).iter().any(|target| matches!(target, DeliveryTarget::PullRequest { pr_id: id, .. } if id == pr_id))
        })
    })
}
pub(super) fn item_for_revision<'a>(
    projection: &'a DomainProjection,
    revision_id: &crate::domain_v2::DocumentRevisionId,
    domain: &DomainState,
) -> Option<&'a InboxItem> {
    projection
        .inbox
        .iter()
        .find(|item| interaction_of(item, domain).and_then(reviewed_revision) == Some(revision_id))
}

pub(super) fn build(id: &str, context: &RenderContext<'_>) -> Result<View, BuildError> {
    if !plain_id(id) {
        return Err(BuildError::InvalidKey);
    }
    let key = format!("card:{id}");
    let domain = context.domain;
    let projection = domain.projection();
    let fallback;
    let item = match projection.inbox.iter().find(|item| public_id(item) == id) {
        Some(item) => item,
        None => {
            let direct = domain.interactions.get(&InteractionId(id.into())).filter(|i| i.recipient == Recipient::User);
            let found = direct.or_else(|| alias(domain, id)).ok_or(BuildError::NotFound)?;
            fallback = projection
                .inbox
                .iter()
                .find(|item| public_id(item) == found.id.0)
                .cloned()
                .unwrap_or_else(|| domain.item(found));
            &fallback
        }
    };
    let card = from_item(item, context, true)?;
    let title = card.title.clone();
    let mut sections = vec![super::common::section("card", None, vec![Item::Card(card)])];
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
        status: super::status::build(&projection, context),
        sections,
        empty: None,
        notices: vec![],
    })
}

/// The card a pull request or a document revision is about, for old links to them: its
/// current delivery, else a question its node linked to it.
fn alias<'a>(domain: &'a DomainState, id: &str) -> Option<&'a Interaction> {
    let names = |target: &DeliveryTarget| match target {
        DeliveryTarget::PullRequest { pr_id, .. } => pr_id.0 == id,
        DeliveryTarget::DocumentRevision { revision_id, .. } => revision_id.0 == id,
        _ => false,
    };
    let delivery = domain.interactions.values().find(|interaction| {
        let current = match &interaction.scope {
            Scope::Task { task_id } => {
                domain.tasks.get(task_id).is_some_and(|t| t.final_presentation.as_ref() == Some(&interaction.id))
            }
            _ => false,
        };
        current && targets_of(interaction).iter().any(names)
    });
    delivery.or_else(|| {
        domain.interactions.values().find(|interaction| {
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
        CardKind::Know => t!(lang, "FYI", "Saber"),
        CardKind::Provide => t!(lang, "Provide", "Aportar"),
        CardKind::Test => t!(lang, "Test", "Probar"),
        CardKind::Do => t!(lang, "Do", "Hacer"),
        CardKind::Problem => t!(lang, "Problem", "Problema"),
        CardKind::Propose => t!(lang, "Proposal", "Propuesta"),
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
        None => t!(context.lang, "the orchestrator", "el orquestador").into(),
    }
}

/// What a delivery or a card stands for in another card's chip: "PR #649", a document title, or
/// the card's own need.
fn target_label(domain: &DomainState, id: &InteractionId) -> String {
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
    clip(&interaction.card.need, 50)
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
        .map(|id| chip(id, tf!(lang, "blocks {target}", "bloquea {target}", target = target_label(context.domain, id))))
        .collect();
    let blocked_by = item
        .blocked_by
        .iter()
        .map(|id| chip(id, tf!(lang, "blocked by {target}", "bloqueada por {target}", target = target_label(context.domain, id))))
        .collect();
    (blocks, blocked_by)
}

/// Why a delivery cannot be accepted or merged yet, in the user's words.
fn blocked_reason(item: &InboxItem, context: &RenderContext<'_>) -> Option<String> {
    let first = item.blocked_by.first()?;
    let domain = context.domain;
    let blocker = domain.interactions.get(first)?;
    Some(match pr_target(blocker, domain) {
        Some(pr) => tf!(context.lang, "Merge #{number} first", "Fusiona primero la #{number}", number = pr.number),
        None => tf!(context.lang, "Answer “{need}” first", "Responde primero «{need}»", need = clip(&blocker.card.need, 50)),
    })
}

fn meta_line(item: &InboxItem, interaction: Option<&Interaction>, context: &RenderContext<'_>) -> Option<String> {
    let lang = context.lang;
    let who: String = match interaction.map(|i| &i.author) {
        Some(Author::System) => "Alondra".into(),
        Some(Author::Orchestrator { .. }) => t!(lang, "Orchestrator", "Orquestador").into(),
        _ => match &item.agent_id {
            Some(agent_id) => super::common::agent_label(agent_id, context),
            None => t!(lang, "Orchestrator", "Orquestador").into(),
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

fn context_lines(item: &InboxItem, context: &RenderContext<'_>) -> Vec<ContextLine> {
    let Some(agent_id) = &item.agent_id else { return vec![] };
    let node = context.live.nodes.iter().find(|node| node.agent_id == agent_id.0);
    vec![ContextLine {
        icon: Icon::Node,
        text: super::common::agent_label(agent_id, context),
        link: Some(Link::View { key: format!("node:{agent_id}") }),
        chip: node.and_then(|node| node.project.as_ref()).map(|project| Label { text: project.clone(), tone: Tone::Neutral }),
        state: node.map(|_| super::fleet::node_state(agent_id, context)),
    }]
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
    let need = humanize_task_ids(&spec.need, domain);
    let why = Some(humanize_task_ids(spec.why.trim(), domain)).filter(|why| !why.is_empty());
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
    card.reply = reply_action(item, interaction, context);
    card.actions = visible;
    card.more = more;
    Ok(card)
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
    let spec: &CardSpec = &interaction.card;
    let attachments = interaction.attachments.len();
    let live = pr_target(interaction, domain)
        .and_then(|pr| context.live.pull_requests.iter().find(|detail| detail.id == pr.id.0));
    let (kind, summary_text, others) = match &spec.content {
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

fn button(id: String, label: &str, role: Role) -> Action {
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
    }
}

fn with_availability(mut action: Action, available: Option<&AvailableAction>, lang: Lang) -> Action {
    if let Some(available) = available {
        action.enabled = available.enabled;
        action.disabled_reason = available.disabled_reason.as_deref().map(|code| disabled_reason(code, lang).to_owned());
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

/// What an option reads and does in the user's language; Claude Code's own prompts keep their
/// fixed answers.
fn held_option(hold: Hold, option: &QuestionOption, lang: Lang) -> (String, Input) {
    let yes = hold.approves(&option.id);
    let (label, placeholder): (&str, &str) = match (hold, yes) {
        (Hold::Permission, true) => (t!(lang, "Allow", "Permitir"), ""),
        (Hold::Permission, false) if option.id == Hold::ALWAYS => (t!(lang, "Always for this", "Siempre para esto"), ""),
        (Hold::Permission, false) => (t!(lang, "Deny", "Denegar"), t!(lang, "Write the reason", "Escribe el motivo")),
        (Hold::Plan, true) => (t!(lang, "Approve", "Aprobar"), ""),
        (Hold::Plan, false) => (t!(lang, "Request changes", "Pedir cambios"), t!(lang, "Write what to change", "Escribe qué cambiar")),
        (Hold::Elicitation, true) => (
            t!(lang, "Accept", "Aceptar"),
            option.description.as_deref().unwrap_or(t!(lang, "Write your answer", "Escribe tu respuesta")),
        ),
        (Hold::Elicitation, false) => (t!(lang, "Decline", "Rechazar"), ""),
        (Hold::Question, _) => (option.label.as_str(), t!(lang, "Add a note", "Añade una nota")),
    };
    let input = if option.inputs.is_empty() {
        Input::None
    } else {
        Input::Text {
            placeholder: placeholder.into(),
            required: option.inputs.iter().any(|input| input.required),
            multiline: true,
        }
    };
    (label.to_owned(), input)
}

/// The words of an option the system writes itself, keyed by the id and English text it was
/// built with, in the viewer's language. An option someone worded keeps its own label.
fn fixed_option_text(option: &QuestionOption, lang: Lang) -> (String, Option<String>) {
    let (label, description): (&str, Option<&str>) = match (option.id.as_str(), option.label.as_str()) {
        ("done", "Done") => (t!(lang, "Done", "Hecho"), None),
        ("cannot", "I can't do it") => (t!(lang, "I can't do it", "No puedo hacerlo"), None),
        ("works", "Works") => (t!(lang, "Works", "Funciona"), None),
        ("fails", "Doesn't work") => (t!(lang, "Doesn't work", "No funciona"), None),
        ("do_it", "Do it") => (t!(lang, "Do it", "Hazlo"), None),
        ("not_now", "Not now") => (t!(lang, "Not now", "Ahora no"), None),
        ("skip", "I can't provide it") => (t!(lang, "I can't provide it", "No puedo dárselo"), None),
        ("provide", "Send the file") => (t!(lang, "Send the file", "Enviar el archivo"), None),
        ("provide", "Send the text") => (t!(lang, "Send the text", "Enviar el texto"), None),
        ("provide", "Send the secret") => (t!(lang, "Send the secret", "Enviar el secreto"), None),
        ("retry", "Retry the batch") => (
            t!(lang, "Retry the batch", "Reintentar el lote"),
            Some(t!(lang, "Run the same batch again; check its effects first", "Ejecuta el mismo lote otra vez; revisa antes sus efectos")),
        ),
        ("continue", "Continue without it") => (
            t!(lang, "Continue without it", "Continuar sin él"),
            Some(t!(lang, "Do not replay it; the orchestrator is told it was interrupted", "No se repite; al orquestador se le avisa de que se interrumpió")),
        ),
        ("stop", "Stop it") => (
            t!(lang, "Stop it", "Detenerlo"),
            Some(t!(lang, "Drop the batch and leave the orchestrator idle", "Descarta el lote y deja al orquestador en reposo")),
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
    let mut more: Vec<Action> = vec![];
    if options.is_empty() {
        if let Some(id) = action_id(Verb::Ans, &interaction.id.0, revision, None) {
            let mut action = button(id.to_string(), t!(lang, "Send my answer", "Enviar mi respuesta"), Role::Advance);
            action.input = text_input(t!(lang, "Your answer", "Tu respuesta"), true);
            action.attachments = true;
            action.stamp = Some(t!(lang, "Answered", "Respondida").into());
            buttons.push(with_availability(action, available, lang));
        }
    } else {
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
            let (fixed_label, fixed_description) = fixed_option_text(option, lang);
            let (label, input) = match hold.filter(|hold| *hold != Hold::Question) {
                Some(hold) => held_option(hold, option, lang),
                None => (
                    if recommended { format!("★ {fixed_label}") } else { fixed_label },
                    if option.inputs.is_empty() {
                        Input::None
                    } else {
                        Input::Text {
                            placeholder: t!(lang, "Add a note", "Añade una nota").into(),
                            required: option.inputs.iter().any(|input| input.required),
                            multiline: true,
                        }
                    },
                ),
            };
            let role = match hold {
                Some(hold) if hold.approves(&option.id) => if kind == CardKind::Authorize { Role::Risk } else { Role::Advance },
                Some(Hold::Question) | None if kind == CardKind::Decide => if recommended { Role::Advance } else { Role::Neutral },
                Some(Hold::Question) | None => if index == 0 { Role::Advance } else { Role::Neutral },
                Some(_) => Role::Neutral,
            };
            let mut action = button(id.to_string(), &label, role);
            action.option_id = Some(option.id.clone());
            action.description = fixed_description.filter(|_| matches!(hold, None | Some(Hold::Question)));
            action.input = input;
            action.attachments = option.inputs.iter().any(|input| input.kind == crate::domain_v2::InputKind::Text);
            action.stamp = Some(t!(lang, "Answered", "Respondida").into());
            buttons.push(with_availability(action, available, lang));
        }
        if kind == CardKind::Decide && hold.is_none() {
            if let Some(id) = action_id(Verb::Ans, &interaction.id.0, revision, None) {
                let mut action = button(id.to_string(), t!(lang, "Write my own answer", "Escribir mi respuesta"), Role::Neutral);
                action.input = text_input(t!(lang, "Answer in your own words…", "Responde con tus palabras…"), true);
                action.attachments = true;
                action.stamp = Some(t!(lang, "Answered", "Respondida").into());
                more.push(with_availability(action, available, lang));
            }
        }
    }
    if matches!(interaction.subject, Some(InteractionSubject::DocumentReview { .. })) {
        buttons.extend(request_changes(Verb::Changes, &interaction.id.0, revision, None, true, available, context));
    }
    (buttons, more)
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
    let blocked = blocked_reason(item, context);
    let hold_back = |mut action: Action| {
        if let Some(reason) = &blocked {
            action.enabled = false;
            action.disabled_reason = Some(reason.clone());
        }
        action
    };
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
    let accept = |role: Role, label: &str| {
        let id = action_id(Verb::Accept, &interaction.id.0, interaction.revision, None)?;
        let mut action = button(id.to_string(), label, role);
        action.stamp = Some(if document { t!(lang, "Approved", "Aprobada") } else { t!(lang, "Accepted", "Aceptada") }.into());
        Some(hold_back(with_availability(action, find("accept_delivery"), lang)))
    };
    let mut visible = vec![];
    let mut more = vec![];
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
    if live.is_some_and(|detail| detail.conflicts) {
        if let Some(id) = action_id(Verb::Conflicts, &pr.id.0, revision, None) {
            let mut action = button(id.to_string(), t!(lang, "Resolve conflicts", "Resolver conflictos"), Role::Advance);
            action.stamp = Some(t!(lang, "Conflicts requested", "Conflictos comunicados").into());
            visible.push(action);
        }
    } else if let Some(available) = find("authorize_merge") {
        let head = available.expected_head.as_ref().map(|head| merge_extra(head, &interaction.id.0));
        if let Some(id) = action_id(Verb::Merge, &interaction.id.0, interaction.revision, head) {
            let stacked = live.and_then(|detail| detail.stacked_on.as_deref());
            let methods = pr
                .allowed_methods
                .iter()
                .enumerate()
                .map(|(index, method)| {
                    let name = format!("{method:?}").to_ascii_lowercase();
                    ActionOption {
                        id: name.clone(),
                        label: match name.as_str() {
                            "merge" => t!(lang, "Merge commit", "Commit de fusión").into(),
                            "squash" => "squash".into(),
                            _ => "rebase".into(),
                        },
                        description: None,
                        style: Style::Secondary,
                        input: Input::None,
                        key: (context.client != Client::Ios && index < 9).then(|| (index + 1).to_string()),
                    }
                })
                .collect();
            let mut merge = button(id.to_string(), t!(lang, "Merge", "Fusionar"), Role::Advance);
            merge.presentation = Presentation::Menu;
            merge.options = methods;
            merge.confirm = Some(merge_confirm(pr, context));
            merge.stamp = Some(t!(lang, "Merged", "Fusionada").into());
            merge.enabled = available.enabled && stacked.is_none();
            merge.disabled_reason = stacked
                .map(|id| {
                    domain.pull_requests.get(&PrId(id.into())).map_or_else(
                        || t!(lang, "Merge the upstream PR first", "Fusiona primero la PR anterior").into(),
                        |upstream| tf!(lang, "Merge #{number} first", "Fusiona primero la #{number}", number = upstream.number),
                    )
                })
                .or_else(|| available.disabled_reason.as_deref().map(|code| disabled_reason(code, lang).to_owned()));
            visible.push(hold_back(merge));
        }
    } else {
        visible.extend(accept(Role::Advance, accept_label));
    }
    let head: String = pr.head.chars().take(8).collect();
    visible.extend(request_changes(
        Verb::Changes, &pr.id.0, revision, Some(head), false, None, context,
    ));
    if let Some(action) = visible.last_mut().filter(|action| action.id.starts_with("changes:")) {
        action.comments = Some(CommentSpec { kind: super::CommentKind::DiffLines });
        action.link = Some(Link::View { key: format!("pull_request:{}", pr.id) });
    }
    if find("authorize_merge").is_some() || live.is_some_and(|detail| detail.conflicts) {
        let label = t!(lang, "Accept without merging", "Aceptar sin fusionar");
        more.extend(accept(Role::Neutral, label));
    }
    (visible, more)
}

fn merge_confirm(pr: &PullRequest, context: &RenderContext<'_>) -> super::Confirm {
    let summary = context
        .live
        .pull_requests
        .iter()
        .find(|detail| detail.id == pr.id.0)
        .and_then(|detail| detail.title.clone())
        .unwrap_or_else(|| pr.repository.clone());
    super::Confirm {
        title: tf!(context.lang, "Merge PR #{number}?", "¿Fusionar la PR #{number}?", number = pr.number),
        message: summary,
        button: t!(context.lang, "Merge", "Fusionar").into(),
    }
}

/// Always behind "More": open the PR, ask the author to say it again, and the terminal's local
/// shortcuts.
/// "Change answers": while the rest of the questions asked together are still open, an answer
/// already given can be taken back from any card still waiting; its card comes back to the stack.
fn change_answers(interaction: &Interaction, context: &RenderContext<'_>) -> Option<Action> {
    let lang = context.lang;
    let domain = context.domain;
    interaction.card.group.as_ref()?;
    if !domain.open(interaction) || domain.group_complete(interaction) {
        return None;
    }
    let answered: Vec<ActionOption> = domain.group_members(interaction).into_iter().filter_map(|member| {
        let InteractionPayload::Question { options, answer: Some(answer), .. } = &member.payload else { return None };
        let said = options.iter().find(|option| option.id == answer.option_id).map(|option| option.label.clone())
            .or_else(|| answer.inputs.values().find_map(|value| match value {
                crate::domain_v2::InputValue::Text(text) if !text.trim().is_empty() => Some(clip(text, 40)),
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
        if let Some(session) = item.agent_id.as_ref().and_then(|agent| {
            context.live.nodes.iter().find(|node| node.agent_id == agent.0).and_then(|node| node.short_id.clone())
        }) {
            out.push(local_action(t!(lang, "Attach to node", "Conectar al nodo"), "a", LocalLink::Attach { session }));
        }
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

/// Text to the node (or the orchestrator) the card came from; every card has it.
fn reply_action(item: &InboxItem, interaction: Option<&Interaction>, context: &RenderContext<'_>) -> Option<Action> {
    let lang = context.lang;
    let interaction = interaction?;
    let (id, placeholder) = if matches!(interaction.scope, Scope::Task { .. }) {
        (
            action_id(Verb::Reply, &interaction.id.0, interaction.revision, None)?,
            tf!(lang, "Reply to {node}…", "Responder a {node}…", node = node_name(item, context)),
        )
    } else {
        (
            action_id(Verb::Msg, "view", context.domain.revision, None)?,
            t!(lang, "Reply to the orchestrator…", "Responder al orquestador…").into(),
        )
    };
    let mut action = button(id.to_string(), t!(lang, "Reply", "Responder"), Role::Neutral);
    action.input = text_input(&placeholder, true);
    action.attachments = true;
    action.stamp = Some(t!(lang, "Sent", "Enviado").into());
    action.key = (context.client != Client::Ios).then(|| "r".into());
    action.enabled = !interaction.withdrawn;
    action.disabled_reason = interaction.withdrawn.then(|| disabled_reason("withdrawn", lang).to_owned());
    Some(action)
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
    let content = interaction.card.content.as_ref().filter(|content| !content.body.trim().is_empty());
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
    out
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
    let content = interaction.card.content.as_ref().filter(|content| content.kind != DomainContent::Diff);
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
            let label = options.iter().find(|option| option.id == answer.option_id).map(|option| fixed_option_text(option, lang).0);
            let text = answer.inputs.values().find_map(|value| match value {
                crate::domain_v2::InputValue::Text(text) => Some(clip(text, 240)),
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
                use crate::domain_v2::Instruction;
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

pub(super) fn disabled_reason(code: &str, lang: Lang) -> &'static str {
    match code {
        "already_read" => t!(lang, "Already read", "Ya leído"),
        "withdrawn" => t!(lang, "Withdrawn", "Retirada"),
        "forbidden" => t!(lang, "Unavailable", "No disponible"),
        "task_not_active" => t!(lang, "Task is no longer active", "La tarea ya no está activa"),
        "invalid_action_for_kind" => t!(lang, "Already answered", "Ya respondida"),
        "stale_revision" => t!(lang, "This card has changed", "Esta tarjeta ha cambiado"),
        "blocked" => t!(lang, "Another card holds it back", "Otra tarjeta la retiene"),
        "clarification_pending" => t!(lang, "Waiting for the clarification", "Esperando la aclaración"),
        _ => t!(lang, "No longer available", "Ya no está disponible"),
    }
}
