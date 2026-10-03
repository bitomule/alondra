use super::{
    Action, ActionOption, Attachment, AttachmentKind, BuildError, Card, Check, CheckState, Checks,
    Client, CommentSpec, ContentKind, ContentRef, ContextLine, Detail, Fact, Facts, Icon, Input, Item, Label, Link, LocalLink,
    Markdown, Presentation, RenderContext, Section, Stamp, Style, TimeFormat, Tone, VIEWS_PROTOCOL,
    View,
    action_id::{ActionId, Verb},
    text::{t, tf},
};
use crate::domain_v2::{
    AvailableAction, DomainState, Hold, InboxItem, InboxSubject, InteractionPayload, InteractionSubject,
    Recipient,
};

pub(super) fn build(id: &str, context: &RenderContext<'_>) -> Result<View, BuildError> {
    if !plain_id(id) {
        return Err(BuildError::InvalidKey);
    }
    let projection = context.domain.projection();
    if id == ORCHESTRATOR_DECISION {
        let card = orchestrator_decision(context, true).ok_or(BuildError::NotFound)?;
        let mut view = super::common::view("card", &format!("card:{id}"), Some(card.title.clone()), context, vec![]);
        view.sections.push(super::common::section("card", None, vec![Item::Card(card)]));
        return Ok(view);
    }
    let fallback = context
        .domain
        .interactions
        .values()
        .find(|interaction| interaction.id.0 == id && interaction.recipient == Recipient::User)
        .map(|interaction| context.domain.item(interaction));
    let item = projection
        .inbox
        .iter()
        .find(|item| public_id(item) == id)
        .or_else(|| {
            projection
                .inbox
                .iter()
                .flat_map(|item| &item.children)
                .find(|item| public_id(item) == id)
        })
        .or(fallback.as_ref())
        .ok_or(BuildError::NotFound)?;
    let card = from_item(item, context, true)?;
    let title = card.title.clone();
    let status = super::status::build(&projection, context);
    let mut sections = vec![Section {
        id: "card".into(),
        title: None,
        count: None,
        collapsed: false,
        items: vec![Item::Card(card)],
        card_ids: vec![],
        more: None,
    }];
    if let Some(revision_id) = reviewed_revision(item, context.domain)
        && let Some(revision) = context.domain.document_revisions.get(revision_id)
        && let Some(document) = context.domain.documents.get(&revision.document_id)
    {
        sections.push(Section {
            id: "body".into(),
            title: None,
            count: None,
            collapsed: false,
            items: vec![Item::ContentRef(ContentRef {
                kind: ContentKind::Document,
                title: document.title.clone(),
                size: 0,
                digest: Some(revision.content_digest.clone()),
                fetch: format!("/document-revisions/{revision_id}/content"),
                commentable: true,
            })],
            card_ids: vec![],
            more: None,
        });
    } else if let InboxSubject::PullRequest { pr_id } = &item.subject
        && let Some(pr) = context.domain.pull_requests.get(pr_id)
    {
        sections.push(Section {
            id: "diff".into(),
            title: None,
            count: None,
            collapsed: false,
            items: vec![Item::ContentRef(ContentRef {
                kind: ContentKind::Diff,
                title: t!(context.lang, "Diff", "Diferencias").into(),
                size: 0,
                digest: Some(pr.head.clone()),
                fetch: format!(
                    "/pull-requests/{}/diff",
                    super::id::path_segment(&pr.id.0)
                ),
                commentable: true,
            })],
            card_ids: vec![],
            more: None,
        });
    }
    Ok(View {
        protocol: VIEWS_PROTOCOL,
        data_epoch: context.domain.data_epoch.0.clone(),
        view: "card".into(),
        key: format!("card:{id}"),
        client: context.client,
        version: String::new(),
        cursor: context.cursor.into(),
        title: Some(title),
        status,
        sections,
        empty: None,
        notices: vec![],
    })
}
pub(super) const ORCHESTRATOR_DECISION: &str = "orchestrator-decision";

/// A failed orchestrator run stays claimed until the user retries it or continues without it;
/// until then no new request is split.
pub(super) fn orchestrator_decision(context: &RenderContext<'_>, included: bool) -> Option<Card> {
    let live = context.live.orchestrator.as_ref().filter(|o| o.can_retry && o.active_run_id.is_some())?;
    let explanation = t!(context.lang,
        "Its last run failed partway, so some of its actions may already have happened. New requests wait until you choose: retry the run, or continue without repeating it.",
        "Su última ejecución falló a medias, así que puede que algunas acciones ya se hayan hecho. Las solicitudes nuevas esperan hasta que elijas: reintentarla o continuar sin repetirla.");
    let failure = super::common::orchestrator_failure(context)
        .filter(|_| live.failure.as_deref() != Some("interrupted"));
    Some(Card {
        id: ORCHESTRATOR_DECISION.into(),
        link: Some(Link::View { key: format!("card:{ORCHESTRATOR_DECISION}") }),
        icon: Icon::Warning,
        eyebrow: Some(Label { text: t!(context.lang, "Orchestrator", "Orquestador").into(), tone: Tone::Accent }),
        title: t!(context.lang, "The orchestrator needs your decision", "El orquestador necesita que decidas").into(),
        subtitle: failure.as_deref().and_then(|failure| failure.lines().next()).map(|line| line.chars().take(140).collect()),
        badges: vec![],
        context: vec![ContextLine {
            icon: Icon::Node,
            text: t!(context.lang, "Orchestrator", "Orquestador").into(),
            link: Some(Link::View { key: "node:orchestrator".into() }),
            chip: None,
            state: None,
        }],
        timestamps: vec![],
        state: None,
        detail: if included { Detail::Included } else { Detail::Reference },
        body: included.then(|| Markdown {
            text: match &failure {
                Some(failure) => format!("{explanation}\n\n{failure}"),
                None => explanation.into(),
            },
            truncated: false,
            more: None,
            commentable: false,
        }),
        attachments: vec![],
        quotes: vec![],
        children: vec![],
        facts: None,
        checks: None,
        actions: if included { super::fleet::orchestrator_decisions(context) } else { vec![] },
    })
}

/// The document revision a card is about: a document review, or a question its node linked to one.
fn reviewed_revision<'a>(item: &'a InboxItem, domain: &'a DomainState) -> Option<&'a crate::domain_v2::DocumentRevisionId> {
    match &item.subject {
        InboxSubject::DocumentReview { revision_id, .. } => Some(revision_id),
        InboxSubject::Interaction { interaction_id } => match &domain.interactions.get(interaction_id)?.subject {
            Some(InteractionSubject::DocumentReview { revision_id, .. }) => Some(revision_id),
            _ => None,
        },
        InboxSubject::PullRequest { .. } => None,
    }
}
pub(super) fn plain_id(id: &str) -> bool {
    super::id::valid(id)
}
pub(super) fn public_id(item: &InboxItem) -> &str {
    match &item.subject {
        InboxSubject::Interaction { interaction_id } => &interaction_id.0,
        InboxSubject::PullRequest { pr_id } => &pr_id.0,
        InboxSubject::DocumentReview { revision_id, .. } => &revision_id.0,
    }
}
pub(super) fn card_key(item: &InboxItem) -> String {
    format!("card:{}", public_id(item))
}
fn eyebrow(item: &InboxItem, lang: super::Lang) -> (&'static str, Icon) {
    match item.kind.as_str() {
        "action_request" => (t!(lang, "Your turn", "Te toca"), Icon::Action),
        "question" => (t!(lang, "Question", "Pregunta"), Icon::Question),
        "permission" => (t!(lang, "Permission", "Permiso"), Icon::Warning),
        "plan" => (t!(lang, "Plan", "Plan"), Icon::Document),
        "pull_request" => (
            t!(lang, "Pull request", "Solicitud de cambios"),
            Icon::PullRequest,
        ),
        "document_review" => (t!(lang, "Document", "Documento"), Icon::Document),
        "delivery" => (t!(lang, "Finished", "Terminado"), Icon::Delivery),
        _ => (t!(lang, "Notice", "Aviso"), Icon::Notice),
    }
}
fn hold_of(item: &InboxItem, domain: &DomainState) -> Option<Hold> {
    let InboxSubject::Interaction { interaction_id } = &item.subject else { return None };
    domain.holds.get(interaction_id).copied()
}
struct HeldText {
    title: String,
    subtitle: Option<String>,
    body: Option<String>,
}
/// A held card's own title and text, as 8ccd131 showed them: the plan's first heading over the
/// whole plan; a permission names the node asking, with the tool and what it is for under the
/// title and the command below; otherwise the first line (the question, the request) over the rest.
fn held_text(item: &InboxItem, hold: Option<Hold>, context: &RenderContext<'_>) -> Option<HeldText> {
    let hold = hold?;
    let body = item.body.as_deref()?;
    let first = body.lines().map(|line| line.trim_start_matches('#').trim()).find(|line| !line.is_empty())?;
    let title: String = first.chars().take(120).collect();
    if hold == Hold::Plan {
        return Some(HeldText { title, subtitle: None, body: Some(body.to_owned()) });
    }
    let rest = body.trim_start().split_once('\n').map(|(_, rest)| rest.trim().to_owned()).filter(|rest| !rest.is_empty());
    if hold == Hold::Permission {
        return Some(HeldText { title: permission_title(item, context), subtitle: Some(title), body: rest });
    }
    Some(HeldText { title, subtitle: None, body: rest })
}
fn permission_title(item: &InboxItem, context: &RenderContext<'_>) -> String {
    match &item.agent_id {
        Some(agent_id) => tf!(context.lang, "{node} needs permission", "{node} necesita permiso",
            node = super::common::agent_label(agent_id, context)),
        None => t!(context.lang, "A node needs permission", "Un nodo necesita permiso").into(),
    }
}
pub(super) fn from_item(item: &InboxItem, context: &RenderContext<'_>, included: bool) -> Result<Card, BuildError> {
    let (label, icon) = eyebrow(item, context.lang);
    let held = held_text(item, hold_of(item, context.domain), context);
    let mut context_lines = vec![];
    if let Some(agent_id) = &item.agent_id {
        let node = context.live.nodes.iter().find(|node| node.agent_id == agent_id.0);
        context_lines.push(ContextLine {
            icon: Icon::Node,
            text: super::common::agent_label(agent_id, context),
            link: Some(Link::View {
                key: format!("node:{agent_id}"),
            }),
            chip: node.and_then(|node| node.project.as_ref()).map(|project| Label {
                text: project.clone(),
                tone: Tone::Neutral,
            }),
            state: node.map(|_| super::fleet::node_state(agent_id, context)),
        });
    }
    if let Some(task_id) = &item.task_id {
        if let Some(task) = context.domain.tasks.get(task_id) {
            context_lines.push(ContextLine {
                icon: Icon::Task,
                text: task.title.clone(),
                link: Some(Link::View {
                    key: format!("task:{task_id}"),
                }),
                chip: None,
                state: None,
            });
        }
    }
    if let Some(request_id) = &item.request_id {
        if let Some(request) = context.domain.requests.get(request_id) {
            context_lines.push(ContextLine {
                icon: Icon::Mission,
                text: tf!(
                    context.lang,
                    "Mission · {title}",
                    "Misión · {title}",
                    title = request.title.as_deref().unwrap_or(&request.body)
                ),
                link: (context.client != Client::Ios).then(|| Link::View {
                    key: format!("request:{request_id}"),
                }),
                chip: None,
                state: None,
            });
        }
    }
    let timestamps = context
        .live
        .time(&item.id)
        .map(|at| {
            vec![Stamp {
                role: "created".into(),
                at: at.into(),
                format: TimeFormat::Relative,
            }]
        })
        .unwrap_or_default();
    let live_pr = match &item.subject {
        InboxSubject::PullRequest { pr_id } => context.live.pull_requests.iter().find(|detail| detail.id == pr_id.0),
        _ => None,
    };
    let held_body = held.as_ref().map(|held| held.body.as_ref());
    let body = included
        .then(|| {
            live_pr
                .and_then(|detail| detail.body.as_ref())
                .filter(|text| !text.trim().is_empty())
                .or(held_body.unwrap_or(item.body.as_ref()))
                .map(|text| Markdown {
                    text: if item.kind == "action_request" { fenced(text) } else { text.clone() },
                    truncated: false,
                    more: None,
                    commentable: false,
                })
        })
        .flatten();
    let attachments = if included {
        item_attachment_refs(item, context.domain)
    } else {
        vec![]
    };
    let mut actions = if included {
        actions(item, context)?
    } else {
        vec![]
    };
    let (facts, checks, badges) = if included {
        pr_detail(item, context, &mut actions)?
    } else {
        (None, None, vec![])
    };
    if included && context.client == Client::Tui && context.transport == super::Transport::Loopback {
        if let Some(agent) = &item.agent_id {
            if let Some(session) = context.live.nodes.iter()
                .find(|node| node.agent_id == agent.0)
                .and_then(|node| node.short_id.as_ref()) {
                actions.push(local_action(t!(context.lang, "Attach to node", "Conectar al nodo"), "a",
                    LocalLink::Attach { session: session.clone() }));
            }
        }
    }
    let quotes = if included && matches!(item.kind.as_str(), "question" | "action_request" | "plan") {
        item.agent_id
            .as_ref()
            .and_then(|agent_id| {
                context
                    .live
                    .nodes
                    .iter()
                    .find(|node| node.agent_id == agent_id.0)
            })
            .and_then(|node| {
                node.last_message
                    .as_ref()
                    .map(|text| (text.trim(), node.last_message_at.as_ref()))
            })
            .filter(|(text, _)| !text.is_empty() && !text.starts_with("mcp__"))
            .map(|(text, at)| {
                vec![super::Quote {
                    label: t!(
                        context.lang,
                        "What the node said last",
                        "Lo último que dijo el nodo"
                    )
                    .into(),
                    text: text.into(),
                    at: at.cloned(),
                }]
            })
            .unwrap_or_default()
    } else {
        vec![]
    };
    let mut children = if included {
        item.children
            .iter()
            .map(|child| from_item(child, context, true))
            .collect::<Result<Vec<_>, _>>()?
    } else {
        vec![]
    };
    if included && item.kind == "document_review" {
        for child in &mut children {
            actions.append(&mut child.actions);
        }
    }
    Ok(Card {
        id: public_id(item).into(),
        link: Some(Link::View {
            key: card_key(item),
        }),
        icon,
        eyebrow: Some(Label {
            text: label.into(),
            tone: if item.pending {
                Tone::Accent
            } else {
                Tone::Neutral
            },
        }),
        subtitle: held.as_ref().and_then(|held| held.subtitle.clone()),
        title: held
            .map(|held| held.title)
            .or_else(|| live_pr.and_then(|detail| detail.title.clone()))
            .unwrap_or_else(|| item.title.clone()),
        badges,
        context: context_lines,
        timestamps,
        state: None,
        detail: if included {
            Detail::Included
        } else {
            Detail::Reference
        },
        body,
        attachments,
        quotes,
        children,
        facts,
        checks,
        actions,
    })
}

fn fenced(text: &str) -> String {
    let longest_run = text.split(|c| c != '`').map(str::len).max().unwrap_or(0);
    let fence = "`".repeat((longest_run + 1).max(3));
    format!("{fence}\n{}\n{fence}", text.trim_end())
}
pub(super) fn changes_summary(detail: &super::LivePullRequest, lang: super::Lang) -> String {
    let files = if detail.changed_files == 1 {
        t!(lang, "1 file", "1 archivo").to_owned()
    } else {
        tf!(lang, "{count} files", "{count} archivos", count = detail.changed_files)
    };
    tf!(lang, "+{added} −{removed} in {files}", "+{added} −{removed} en {files}",
        added = detail.additions, removed = detail.deletions, files = files)
}
fn local_action(label: &str, key: &str, local: LocalLink) -> Action {
    Action {
        id: String::new(), label: label.into(), style: Style::Plain,
        presentation: Presentation::Button, options: vec![], input: Input::None,
        attachments: false, comments: None, confirm: None, enabled: true,
        disabled_reason: None, stamp: None, key: Some(key.into()),
        link: Some(Link::Local { local }),
    }
}
fn pr_detail(
    item: &InboxItem,
    context: &RenderContext<'_>,
    actions: &mut Vec<Action>,
) -> Result<(Option<Facts>, Option<Checks>, Vec<Label>), BuildError> {
    let InboxSubject::PullRequest { pr_id } = &item.subject else {
        return Ok((None, None, vec![]));
    };
    let Some(pr) = context.domain.pull_requests.get(pr_id) else {
        return Ok((None, None, vec![]));
    };
    if context.client == Client::Tui && context.transport == super::Transport::Loopback {
        actions.push(local_action(t!(context.lang, "Review local clone", "Revisar copia local"), "d",
            LocalLink::ReviewClone { repo: pr.repository.clone(), branch: pr.head.clone(), pr: pr.number, pr_id: pr.id.0.clone() }));
    }
    let live = context
        .live
        .pull_requests
        .iter()
        .find(|detail| detail.id == pr_id.0);
    let mut facts = vec![
        Fact {
            label: t!(context.lang, "Repository", "Repositorio").into(),
            value: format!("{} #{}", pr.repository, pr.number),
            tone: None,
            link: None,
        },
        Fact {
            label: t!(context.lang, "Base", "Base").into(),
            value: pr.base.clone(),
            tone: None,
            link: None,
        },
    ];
    if let Some(head) = live.and_then(|detail| detail.head_ref.clone()) {
        facts.push(Fact {
            label: t!(context.lang, "Head", "Rama").into(),
            value: head,
            tone: None,
            link: None,
        });
    }
    if let Some(detail) = live.filter(|detail| detail.changed_files > 0) {
        facts.push(Fact {
            label: t!(context.lang, "Changes", "Cambios").into(),
            value: changes_summary(detail, context.lang),
            tone: None,
            link: None,
        });
    }
    if let Some(stacked) = live.and_then(|detail| detail.stacked_on.as_deref()) {
        facts.push(Fact {
            label: t!(context.lang, "Stacked on", "Depende de").into(),
            value: context.domain.pull_requests.get(&crate::domain_v2::PrId(stacked.into()))
                .map_or_else(|| stacked.into(), |upstream| format!("#{}", upstream.number)),
            tone: Some(Tone::Warning),
            link: Some(Link::View {
                key: format!("pull_request:{stacked}"),
            }),
        });
    }
    let checks = live
        .filter(|detail| !detail.checks.is_empty())
        .map(|detail| Checks {
            items: detail
                .checks
                .iter()
                .map(|check| Check {
                    name: check.name.clone(),
                    state: check.state,
                })
                .collect(),
            link: None,
        })
        .or_else(|| live.and_then(|detail| detail.checks_passed).map(|passed| Checks {
            items: vec![Check {
                name: t!(context.lang, "Checks", "Comprobaciones").into(),
                state: if passed {
                    CheckState::Pass
                } else {
                    CheckState::Fail
                },
            }],
            link: None,
        }));
    let mut badges = vec![];
    if live.is_some_and(|detail| detail.conflicts) {
        badges.push(Label {
            text: t!(context.lang, "Conflicts", "Conflictos").into(),
            tone: Tone::Warning,
        });
    } else if live.is_some_and(|detail| detail.checks_passed.is_none()) {
        badges.push(Label {
            text: t!(
                context.lang,
                "GitHub is checking…",
                "GitHub está comprobando…"
            )
            .into(),
            tone: Tone::Info,
        });
    }
    if let Some(url) = live.and_then(|detail| detail.url.as_ref()) {
        actions.push(Action {
            id: String::new(),
            label: t!(context.lang, "Open on GitHub", "Abrir en GitHub").into(),
            style: Style::Plain,
            presentation: Presentation::Button,
            options: vec![],
            input: Input::None,
            attachments: false,
            comments: None,
            confirm: None,
            enabled: true,
            disabled_reason: None,
            stamp: None,
            key: (context.client != Client::Ios).then(|| "w".into()),
            link: Some(Link::Url { url: url.clone() }),
        });
    }
    let revision = context.domain.revision;
    {
        if let Some(id) = action_id(Verb::AskPr, &pr_id.0, revision, None) {
        actions.push(Action {
            id: id.to_string(),
            label: t!(context.lang, "Ask about the PR", "Preguntar por la PR").into(),
            style: Style::Secondary,
            presentation: Presentation::Button,
            options: vec![],
            input: Input::Text {
                placeholder: t!(context.lang, "Your question", "Tu pregunta").into(),
                required: true,
                multiline: true,
            },
            attachments: true,
            comments: None,
            confirm: None,
            enabled: true,
            disabled_reason: None,
            stamp: None,
            key: (context.client != Client::Ios).then(|| "p".into()),
            link: None,
        });
        }
    }
    {
        if let Some(id) = action_id(
        Verb::Changes,
        &pr_id.0,
        revision,
        Some(pr.head.chars().take(8).collect()),
        ) {
        actions.push(Action {
            id: id.to_string(),
            label: t!(context.lang, "Request changes", "Pedir cambios").into(),
            style: Style::Secondary,
            presentation: Presentation::Button,
            options: vec![],
            input: Input::Text {
                placeholder: t!(context.lang, "Describe the changes", "Describe los cambios")
                    .into(),
                required: false,
                multiline: true,
            },
            attachments: true,
            comments: Some(CommentSpec {
                kind: super::CommentKind::DiffLines,
            }),
            confirm: None,
            enabled: true,
            disabled_reason: None,
            stamp: None,
            key: (context.client != Client::Ios).then(|| "r".into()),
            link: Some(Link::View {
                key: format!("pull_request:{pr_id}"),
            }),
        });
        }
    }
    if live.is_some_and(|detail| detail.conflicts) {
        {
            if let Some(id) = action_id(Verb::Conflicts, &pr_id.0, revision, None) {
            actions.push(Action {
                id: id.to_string(),
                label: t!(context.lang, "Resolve conflicts", "Resolver conflictos").into(),
                style: Style::Primary,
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
            });
            }
        }
    } else if let Some((child, available)) = item
        .children
        .iter()
        .flat_map(|child| {
            child
                .available_actions
                .iter()
                .filter(|action| action.kind == "authorize_merge")
                .map(move |action| (child, action))
        })
        .next()
    {
        {
            if let Some(id) = action_id(
            Verb::Merge,
            public_id(child),
            child.revision,
            available
                .expected_head
                .as_ref()
                .map(|head| merge_extra(head, public_id(child))),
            ) {
            let stacked = live.and_then(|detail| detail.stacked_on.as_deref());
            let methods = pr
                .allowed_methods
                .iter()
                .map(|method| {
                    let id = format!("{method:?}").to_ascii_lowercase();
                    ActionOption {
                        id: id.clone(),
                        label: match id.as_str() {
                            "merge" => t!(context.lang, "Merge commit", "Commit de fusión").into(),
                            "squash" => "squash".into(),
                            _ => "rebase".into(),
                        },
                        description: None,
                        style: Style::Secondary,
                        input: Input::None,
                        key: None,
                    }
                })
                .collect();
            actions.push(Action {
                id: id.to_string(),
                label: t!(context.lang, "Merge", "Fusionar").into(),
                style: Style::Primary,
                presentation: Presentation::Menu,
                options: methods,
                input: Input::None,
                attachments: false,
                comments: None,
                confirm: Some(merge_confirm(pr, context)),
                enabled: available.enabled && stacked.is_none(),
                disabled_reason: stacked.map(|id| context.domain.pull_requests.get(&crate::domain_v2::PrId(id.into()))
                    .map_or_else(
                        || t!(context.lang, "Merge the upstream PR first", "Fusiona primero la PR anterior").into(),
                        |upstream| tf!(context.lang, "Merge #{number} first", "Fusiona primero la #{number}", number = upstream.number))),
                stamp: None,
                key: (context.client != Client::Ios).then(|| "m".into()),
                link: None,
            });
            }
        }
    }
    Ok((Some(Facts { items: facts }), checks, badges))
}

fn item_attachment_refs(item: &InboxItem, domain: &DomainState) -> Vec<Attachment> {
    let InboxSubject::Interaction { interaction_id } = &item.subject else {
        return vec![];
    };
    domain
        .interactions
        .get(interaction_id)
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

fn action_id(verb: Verb, subject: &str, revision: u64, extra: Option<String>) -> Option<ActionId> {
    match ActionId::new(verb, subject, revision, extra) {
        Ok(id) => Some(id),
        Err(_) => {
            eprintln!("Alondra omitted an action with an invalid identifier ({verb:?})");
            None
        }
    }
}

fn actions(item: &InboxItem, context: &RenderContext<'_>) -> Result<Vec<Action>, BuildError> {
    let mut out = vec![];
    for available in &item.available_actions {
        if let Some(action) = action_for(item, available, context)? {
            out.push(action);
        }
    }
    out.extend(review_actions(item, context));
    if let InboxSubject::Interaction { interaction_id } = &item.subject {
        if let Some(interaction) = context.domain.interactions.get(interaction_id) {
            let (verb, label, placeholder, key) = match &interaction.payload {
                InteractionPayload::Question { .. } | InteractionPayload::ActionRequest { .. }
                    if item.pending && hold_of(item, context.domain).is_none() =>
                {
                    (
                        Verb::Ctx,
                        t!(context.lang, "Need more context", "Necesito más contexto"),
                        t!(context.lang, "What is missing?", "¿Qué falta?").to_owned(),
                        "c",
                    )
                }
                InteractionPayload::Information { .. } => (
                    Verb::Reply,
                    t!(context.lang, "Reply", "Responder"),
                    t!(context.lang, "Write a reply", "Escribe una respuesta").to_owned(),
                    "r",
                ),
                InteractionPayload::Delivery { .. } if item.pending => (
                    Verb::Reply,
                    t!(context.lang, "Send", "Enviar"),
                    reply_placeholder(item, context),
                    "w",
                ),
                _ => return Ok(out),
            };
            let delivery = matches!(interaction.payload, InteractionPayload::Delivery { .. });
            if let Some(id) = action_id(verb, &interaction_id.0, interaction.revision, None) {
                out.push(Action {
                    id: id.to_string(),
                    label: label.into(),
                    style: Style::Secondary,
                    presentation: Presentation::Button,
                    options: vec![],
                    input: Input::Text {
                        placeholder: placeholder.into(),
                        required: verb != Verb::Ctx,
                        multiline: true,
                    },
                    attachments: verb == Verb::Reply,
                    comments: (verb == Verb::Reply && !delivery).then_some(CommentSpec {
                        kind: super::CommentKind::Blocks,
                    }),
                    confirm: None,
                    enabled: true,
                    disabled_reason: None,
                    stamp: delivery.then(|| t!(context.lang, "Sent", "Enviado").into()),
                    key: (context.client != Client::Ios).then(|| key.into()),
                    link: None,
                });
            }
        }
    }
    Ok(out)
}
fn action_for(
    item: &InboxItem,
    available: &AvailableAction,
    context: &RenderContext<'_>,
) -> Result<Option<Action>, BuildError> {
    let (verb, label, style, presentation, input, key) = match available.kind.as_str() {
        "answer_question" => (
            Verb::Ans,
            t!(context.lang, "Answer", "Responder"),
            Style::Primary,
            Presentation::Options,
            Input::Text {
                placeholder: t!(context.lang, "Answer in your own words…", "Responde con tus palabras…").into(),
                required: false,
                multiline: true,
            },
            None,
        ),
        "mark_read" => (
            Verb::Read,
            t!(context.lang, "Mark as read", "Leído"),
            Style::Primary,
            Presentation::Button,
            Input::None,
            Some("x"),
        ),
        "accept_delivery" => (
            Verb::Accept,
            if is_document_review(item, context.domain) {
                t!(context.lang, "Approve", "Aprobar")
            } else {
                t!(context.lang, "Accept", "Aceptar")
            },
            Style::Primary,
            Presentation::Button,
            Input::None,
            Some("v"),
        ),
        "submit_feedback" if carries_artifact(item) || is_document_review(item, context.domain) => (
            Verb::Changes,
            t!(context.lang, "Request changes", "Pedir cambios"),
            Style::Secondary,
            Presentation::Button,
            Input::Text {
                placeholder: t!(context.lang, "Describe the changes", "Describe los cambios")
                    .into(),
                required: true,
                multiline: true,
            },
            Some("r"),
        ),
        "submit_feedback" => return Ok(None),
        "authorize_merge" => (
            Verb::Merge,
            t!(context.lang, "Merge", "Fusionar"),
            Style::Primary,
            Presentation::Menu,
            Input::None,
            Some("m"),
        ),
        _ => return Ok(None),
    };
    let subject = public_id(item);
    let revision = match &item.subject {
        InboxSubject::Interaction { interaction_id } => context
            .domain
            .interactions
            .get(interaction_id)
            .map_or(item.revision, |i| i.revision),
        _ => item.revision,
    };
    let extra = if verb == Verb::Merge {
        available
            .expected_head
            .as_ref()
            .map(|head| merge_extra(head, subject))
    } else {
        None
    };
    let Some(id) = action_id(verb, subject, revision, extra).map(|id| id.to_string()) else { return Ok(None); };
    let options = if verb == Verb::Ans {
        question_options(item, context)
    } else {
        vec![]
    };
    let merge_pr = if verb == Verb::Merge {
        match &item.subject {
            InboxSubject::Interaction { interaction_id } => context
                .domain
                .interactions
                .get(interaction_id)
                .and_then(|interaction| match &interaction.payload {
                    InteractionPayload::Delivery { targets, .. } => {
                        targets.iter().find_map(|target| {
                            if let crate::domain_v2::DeliveryTarget::PullRequest { pr_id, .. } =
                                target
                            {
                                context.domain.pull_requests.get(pr_id)
                            } else {
                                None
                            }
                        })
                    }
                    _ => None,
                }),
            _ => None,
        }
    } else {
        None
    };
    let options = if let Some(pr) = merge_pr {
        pr.allowed_methods
                .iter()
                .enumerate()
                .map(|(index, method)| {
                    let name = format!("{method:?}").to_ascii_lowercase();
                    ActionOption {
                        id: name.clone(),
                        label: match name.as_str() {
                            "merge" => t!(context.lang, "Merge commit", "Commit de fusión").into(),
                            "squash" => "squash".into(),
                            _ => "rebase".into(),
                        },
                        description: None,
                        style: Style::Secondary,
                        input: Input::None,
                        key: (context.client != Client::Ios && index < 9)
                            .then(|| (index + 1).to_string()),
                    }
                })
                .collect()
    } else {
        options
    };
    let confirm = merge_pr.map(|pr| merge_confirm(pr, context));
    let stamp = match verb {
        Verb::Ans => Some(t!(context.lang, "Answered", "Respondida")),
        Verb::Read => Some(t!(context.lang, "Read", "Leída")),
        Verb::Accept => Some(if is_document_review(item, context.domain) {
            t!(context.lang, "Approved", "Aprobada")
        } else {
            t!(context.lang, "Accepted", "Aceptada")
        }),
        Verb::Changes => Some(t!(context.lang, "Changes requested", "Cambios solicitados")),
        Verb::Merge => Some(t!(context.lang, "Merged", "Fusionada")),
        _ => None,
    }
    .map(str::to_owned);
    Ok(Some(Action {
        id,
        label: label.into(),
        style,
        presentation,
        options,
        input,
        attachments: matches!(verb, Verb::Ans | Verb::Changes),
        comments: (verb == Verb::Changes).then_some(CommentSpec {
            kind: super::CommentKind::Blocks,
        }),
        confirm,
        enabled: available.enabled,
        disabled_reason: available
            .disabled_reason
            .as_deref()
            .map(|code| disabled_reason(code, context.lang).to_owned()),
        stamp,
        key: key.map(str::to_owned),
        link: None,
    }))
}

fn merge_confirm(pr: &crate::domain_v2::PullRequest, context: &RenderContext<'_>) -> super::Confirm {
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

/// 8ccd131's review of a held plan or of a question about a document: Request changes sends the
/// comments on the text's blocks as the answer, and a plan is approved from the same bar.
fn review_actions(item: &InboxItem, context: &RenderContext<'_>) -> Vec<Action> {
    let InboxSubject::Interaction { interaction_id } = &item.subject else { return vec![] };
    let Some(interaction) = context.domain.interactions.get(interaction_id) else { return vec![] };
    let plan = hold_of(item, context.domain) == Some(Hold::Plan);
    if !plan && !matches!(interaction.subject, Some(InteractionSubject::DocumentReview { .. })) {
        return vec![];
    }
    let Some(answer) = item.available_actions.iter().find(|action| action.kind == "answer_question") else {
        return vec![];
    };
    let lang = context.lang;
    let action = |verb: Verb, label: &str, style: Style, input: Input, stamp: &str, key: Option<&str>| {
        let comments = verb == Verb::Changes;
        action_id(verb, &interaction_id.0, interaction.revision, None).map(|id| Action {
            id: id.to_string(),
            label: label.into(),
            style,
            presentation: Presentation::Button,
            options: vec![],
            input,
            attachments: comments,
            comments: comments.then_some(CommentSpec { kind: super::CommentKind::Blocks }),
            confirm: None,
            enabled: answer.enabled,
            disabled_reason: answer.disabled_reason.as_deref().map(|code| disabled_reason(code, lang).to_owned()),
            stamp: Some(stamp.into()),
            key: key.filter(|_| context.client != Client::Ios).map(str::to_owned),
            link: None,
        })
    };
    let changes = action(
        Verb::Changes,
        t!(lang, "Request changes", "Pedir cambios"),
        Style::Secondary,
        Input::Text {
            placeholder: t!(lang, "Describe the changes", "Describe los cambios").into(),
            required: true,
            multiline: true,
        },
        t!(lang, "Changes requested", "Cambios solicitados"),
        Some("r"),
    );
    let approve = if plan {
        action(Verb::Accept, t!(lang, "Approve", "Aprobar"), Style::Primary, Input::None,
            t!(lang, "Approved", "Aprobado"), None)
    } else {
        None
    };
    changes.into_iter().chain(approve).collect()
}

fn carries_artifact(item: &InboxItem) -> bool {
    !crate::artifact::urls_in(item.body.as_deref().unwrap_or_default()).is_empty()
}

fn reply_placeholder(item: &InboxItem, context: &RenderContext<'_>) -> String {
    let node = item.agent_id.as_ref().map_or_else(
        || t!(context.lang, "this node", "este nodo").to_owned(),
        |agent_id| super::common::agent_label(agent_id, context),
    );
    tf!(context.lang, "Reply to {node}…", "Responder a {node}…", node = node)
}

fn is_document_review(item: &InboxItem, domain: &DomainState) -> bool {
    if matches!(&item.subject, InboxSubject::DocumentReview { .. }) {
        return true;
    }
    let InboxSubject::Interaction { interaction_id } = &item.subject else {
        return false;
    };
    domain.interactions.get(interaction_id).is_some_and(|interaction| {
        matches!(&interaction.payload, InteractionPayload::Delivery { targets, .. }
            if targets.iter().any(|target| matches!(target,
                crate::domain_v2::DeliveryTarget::DocumentRevision { .. })))
    })
}

fn question_options(item: &InboxItem, context: &RenderContext<'_>) -> Vec<ActionOption> {
    let InboxSubject::Interaction { interaction_id } = &item.subject else {
        return vec![];
    };
    let Some(interaction) = context.domain.interactions.get(interaction_id) else {
        return vec![];
    };
    let options = match &interaction.payload {
        InteractionPayload::Question { options, .. }
        | InteractionPayload::ActionRequest { options, .. } => options,
        _ => return vec![],
    };
    if let Some(hold) = context.domain.holds.get(interaction_id).filter(|hold| **hold != Hold::Question) {
        return held_options(*hold, options, context);
    }
    options
        .iter()
        .enumerate()
        .map(|(index, option)| ActionOption {
            id: option.id.clone(),
            label: option.label.clone(),
            description: option.description.clone(),
            style: if index == 0 {
                Style::Primary
            } else {
                Style::Secondary
            },
            input: if option.inputs.is_empty() {
                Input::None
            } else {
                Input::Text {
                    placeholder: t!(context.lang, "Add a note", "Añade una nota").into(),
                    required: option.inputs.iter().any(|input| input.required),
                    multiline: true,
                }
            },
            key: (context.client != Client::Ios && index < 9).then(|| (index + 1).to_string()),
        })
        .collect()
}

/// Allow/Deny and Approve/Request changes in the reader's language; on a plan the terminal
/// keeps 8ccd131's `a` approve and `p` request changes.
fn held_options(hold: Hold, options: &[crate::domain_v2::QuestionOption], context: &RenderContext<'_>) -> Vec<ActionOption> {
    let lang = context.lang;
    options
        .iter()
        .enumerate()
        .map(|(index, option)| {
            let yes = hold.approves(&option.id);
            let (label, placeholder, key) = match (hold, yes) {
                (Hold::Permission, true) => (t!(lang, "Allow", "Permitir"), "", "1"),
                (Hold::Permission, false) => (t!(lang, "Deny", "Denegar"), t!(lang, "Write the reason", "Escribe el motivo"), "2"),
                (Hold::Plan, true) => (t!(lang, "Approve", "Aprobar"), "", "a"),
                (Hold::Plan, false) => (t!(lang, "Request changes", "Pedir cambios"), t!(lang, "Write what to change", "Escribe qué cambiar"), "p"),
                (_, true) => (t!(lang, "Accept", "Aceptar"),
                    option.description.as_deref().unwrap_or(t!(lang, "Write your answer", "Escribe tu respuesta")), "1"),
                (_, false) => (t!(lang, "Decline", "Rechazar"), "", "2"),
            };
            ActionOption {
                id: option.id.clone(),
                label: label.into(),
                description: option.description.clone().filter(|_| hold == Hold::Elicitation),
                style: match (index, yes) {
                    (_, true) => Style::Primary,
                    (_, false) if hold == Hold::Permission => Style::Destructive,
                    _ => Style::Secondary,
                },
                input: if option.inputs.is_empty() {
                    Input::None
                } else {
                    Input::Text { placeholder: placeholder.into(), required: false, multiline: true }
                },
                key: (context.client != Client::Ios).then(|| key.into()),
            }
        })
        .collect()
}

pub(super) fn disabled_reason(code: &str, lang: super::Lang) -> &'static str {
    match code {
        "already_read" => t!(lang, "Already read", "Ya leído"),
        "withdrawn" => t!(lang, "Withdrawn", "Retirada"),
        "forbidden" => t!(lang, "Unavailable", "No disponible"),
        "task_not_active" => t!(
            lang,
            "Task is no longer active",
            "La tarea ya no está activa"
        ),
        "invalid_action_for_kind" => t!(lang, "Already answered", "Ya respondida"),
        "stale_revision" => t!(lang, "This card has changed", "Esta tarjeta ha cambiado"),
        _ => t!(lang, "No longer available", "Ya no está disponible"),
    }
}

fn merge_extra(head: &str, presentation_id: &str) -> String {
    format!(
        "{}-{presentation_id}",
        head.chars().take(8).collect::<String>()
    )
}
