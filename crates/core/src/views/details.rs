use super::{
    BuildError, ContentKind, ContentRef, Fact, Facts, Item, Notice, RenderContext, Tone, View,
    common, text::t,
};
use crate::domain_v2::{DocumentRevisionId, InboxSubject, PrId, PrState};

pub(super) fn document(id: &str, context: &RenderContext<'_>) -> Result<View, BuildError> {
    common::id(id)?;
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
    let projection = context.domain.projection();
    if let Some(item) = projection.inbox.iter().chain(projection.inbox.iter().flat_map(|root| root.children.iter())).find(|item| matches!(&item.subject, InboxSubject::DocumentReview { revision_id, .. } if revision_id == &revision.id)) {
        let card = super::card::from_item(item, context, true)?;
        if !card.actions.is_empty() { view.sections.push(common::section("actions", None, vec![Item::Actions(super::ActionBar { actions: card.actions })])); }
    }
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
    view.sections.push(common::section(
        "facts",
        None,
        vec![Item::Facts(Facts { items: facts })],
    ));
    let projection = context.domain.projection();
    if let Some(item) = projection.inbox.iter().find(
        |item| matches!(&item.subject, InboxSubject::PullRequest { pr_id } if pr_id == &pr.id),
    ) {
        let card = super::card::from_item(item, context, true)?;
        if let Some(checks) = card.checks {
            view.sections
                .push(common::section("checks", None, vec![Item::Checks(checks)]));
        }
        if !card.children.is_empty() {
            view.sections.push(common::section(
                "children",
                None,
                card.children.into_iter().map(Item::Card).collect(),
            ));
        }
        if let Some(body) = live
            .and_then(|details| details.body.clone())
            .map(|text| super::Markdown {
                text,
                truncated: false,
                more: None,
                commentable: false,
            })
            .or(card.body)
        {
            view.sections.push(common::section(
                "description",
                None,
                vec![Item::Markdown(body)],
            ));
        }
        if !card.actions.is_empty() {
            view.sections.push(common::section(
                "actions",
                None,
                vec![Item::Actions(super::ActionBar {
                    actions: card.actions,
                })],
            ));
        }
    }
    if !view.sections.iter().any(|section| section.id == "description")
        && let Some(text) = live.and_then(|details| details.body.clone())
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
                t!(context.lang, "Merged", "Fusionada")
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
