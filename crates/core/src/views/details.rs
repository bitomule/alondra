use super::{
    BuildError, ContentKind, ContentRef, Fact, Facts, Item, Markdown, Notice, RenderContext, Tone, View,
    common, text::t,
};
use crate::domain::{DocumentRevisionId, InteractionId, PrId, PrState};

pub(super) fn document(id: &str, context: &RenderContext<'_>) -> Result<View, BuildError> {
    common::id(id)?;
    if !context.domain.document_revisions.contains_key(&DocumentRevisionId(id.into()))
        && let Some((card, part)) = id.rsplit_once('.')
        && (part == "report" || part.starts_with("file-"))
    {
        return card_document(card, part, context);
    }
    let revision = context
        .domain
        .document_revisions
        .get(&DocumentRevisionId(id.into()))
        .ok_or(BuildError::NotFound)?;
    let document = context
        .domain
        .documents
        .get(&revision.document_id)
        .ok_or(BuildError::NotFound)?;
    let mut view = common::view(
        "document",
        &format!("document:{id}"),
        Some(document.title.clone()),
        context,
        vec![],
    );
    let mut facts = vec![
        Fact {
            label: t!(context.lang, "Document", "Documento").into(),
            value: document.title.clone(),
            tone: None,
            link: None,
        },
        Fact {
            label: t!(context.lang, "Revision", "Revisión").into(),
            value: revision.number.to_string(),
            tone: None,
            link: None,
        },
    ];
    if let Some(task) = context
        .domain
        .tasks
        .values()
        .find(|task| task.documents.contains_key(&document.id))
    {
        facts.push(Fact {
            label: t!(context.lang, "Task", "Tarea").into(),
            value: task.title.clone(),
            tone: None,
            link: Some(super::Link::View {
                key: format!("task:{}", task.id),
            }),
        });
    }
    view.sections.push(common::section(
        "header",
        None,
        vec![Item::Facts(Facts { items: facts })],
    ));
    view.sections.push(common::section(
        "body",
        None,
        vec![Item::ContentRef(ContentRef {
            kind: ContentKind::Document,
            title: document.title.clone(),
            size: 0,
            digest: Some(revision.content_digest.clone()),
            fetch: format!("/document-revisions/{id}/content"),
            commentable: true,
        })],
    ));
    if let Some(item) = super::card::item_for_revision(context, &revision.id) {
        let card = super::card::from_item(&item, context, true)?;
        let actions: Vec<super::Action> = card.actions.into_iter().chain(card.more.into_iter().filter(|action| action.link.is_none())).map(Into::into).collect();
        if !actions.is_empty() { view.sections.push(common::section("actions", None, vec![Item::Actions(super::ActionBar { actions })])); }
    }
    Ok(view)
}

/// A document a card carries inside it, a cited file or the report beside a proposed plan, on its own screen.
fn card_document(card: &str, part: &str, context: &RenderContext<'_>) -> Result<View, BuildError> {
    let key = format!("document:{card}.{part}");
    let interaction = context.domain.interactions.get(&InteractionId(card.into())).ok_or(BuildError::NotFound)?;
    let document = super::card::documents(interaction, context.domain, context.lang)
        .into_iter()
        .find(|document| document.key == key)
        .ok_or(BuildError::NotFound)?;
    let markdown = document.markdown.ok_or(BuildError::NotFound)?;
    let mut view = common::view("document", &key, Some(document.title.clone()), context, vec![]);
    let mut facts = vec![Fact { label: document.label, value: document.title, tone: None, link: None }];
    if let crate::domain::Scope::Task { task_id } = &interaction.scope
        && let Some(task) = context.domain.tasks.get(task_id)
    {
        facts.push(Fact {
            label: t!(context.lang, "Task", "Tarea").into(),
            value: task.title.clone(),
            tone: None,
            link: Some(super::Link::View { key: format!("task:{}", task.id) }),
        });
    }
    view.sections.push(common::section("header", None, vec![Item::Facts(Facts { items: facts })]));
    view.sections.push(common::section("body", None, vec![Item::Markdown(Markdown { text: markdown, truncated: false, more: None, commentable: false })]));
    Ok(view)
}

pub(super) fn pull_request(id: &str, context: &RenderContext<'_>) -> Result<View, BuildError> {
    common::id(id)?;
    let pr = context
        .domain
        .pull_requests
        .get(&PrId(id.into()))
        .ok_or(BuildError::NotFound)?;
    let live = context.live.pull_requests.iter().find(|details| details.id == id);
    let mut view = common::view(
        "pull_request",
        &format!("pull_request:{id}"),
        Some(live.and_then(|details| details.title.clone()).unwrap_or_else(|| format!("PR #{}", pr.number))),
        context,
        vec![],
    );
    let mut facts = vec![
        Fact {
            label: t!(context.lang, "Repository", "Repositorio").into(),
            value: pr.repository.clone(),
            tone: None,
            link: None,
        },
        Fact {
            label: t!(context.lang, "Base", "Base").into(),
            value: pr.base.clone(),
            tone: None,
            link: None,
        },
        Fact {
            label: t!(context.lang, "Head", "Rama").into(),
            value: live.and_then(|details| details.head_ref.clone()).unwrap_or_else(|| {
                t!(context.lang, "Branch unavailable", "Rama no disponible").into()
            }),
            tone: None,
            link: None,
        },
    ];
    if let Some(live) = live.filter(|live| live.changed_files > 0) {
        facts.push(Fact {
            label: t!(context.lang, "Changes", "Cambios").into(),
            value: super::card::changes_summary(live, context.lang),
            tone: None,
            link: None,
        });
    }
    let status = super::pr_status::label(super::pr_status::status(pr, live), context.lang);
    view.sections.push(common::section(
        "status",
        None,
        vec![Item::Facts(Facts {
            items: vec![Fact { label: t!(context.lang, "State", "Estado").into(), value: status.text, tone: Some(status.tone), link: None }],
        })],
    ));
    view.sections.push(common::section(
        "facts",
        None,
        vec![Item::Facts(Facts { items: facts })],
    ));
    if let Some(item) = super::card::item_for_pr(context, &pr.id) {
        let card = super::card::from_item(&item, context, true)?;
        let (_, checks) = super::card::pr_overview(pr, context);
        if let Some(checks) = checks {
            view.sections
                .push(common::section("checks", None, vec![Item::Checks(checks)]));
        }
        if let Some(body) = live
            .and_then(|details| Some(super::pr_images::show(details.body.as_deref()?, &details.images, context.lang)))
            .map(|text| super::Markdown {
                text,
                truncated: false,
                more: None,
                commentable: false,
            })
        {
            view.sections.push(common::section(
                "description",
                None,
                vec![Item::Markdown(body)],
            ));
        }
        let actions: Vec<super::Action> = card.actions.into_iter().chain(card.reply).chain(card.more).map(Into::into).collect();
        if !actions.is_empty() {
            view.sections.push(common::section(
                "actions",
                None,
                vec![Item::Actions(super::ActionBar { actions })],
            ));
        }
    }
    if !view.sections.iter().any(|section| section.id == "description")
        && let Some(text) = live.and_then(|details| Some(super::pr_images::show(details.body.as_deref()?, &details.images, context.lang)))
    {
        view.sections.push(common::section(
            "description",
            None,
            vec![Item::Markdown(super::Markdown {
                text,
                truncated: false,
                more: None,
                commentable: false,
            })],
        ));
    }
    view.sections.push(common::section(
        "diff",
        None,
        vec![Item::ContentRef(ContentRef {
            kind: ContentKind::Diff,
            title: t!(context.lang, "Diff", "Diferencias").into(),
            size: 0,
            digest: Some(pr.head.clone()),
            fetch: format!("/pull-requests/{}/diff", super::id::path_segment(id)),
            commentable: true,
        })],
    ));
    if pr.state != PrState::Open {
        view.notices.push(Notice {
            text: if pr.state == PrState::Merged {
                t!(context.lang, "Merged", "Mergeada")
            } else {
                t!(context.lang, "Closed", "Cerrada")
            }
            .into(),
            tone: Tone::Neutral,
            action: None,
        });
    }
    Ok(view)
}
