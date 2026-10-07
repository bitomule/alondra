use super::{
    BuildError, ContentKind, ContentRef, Fact, Facts, Item, Notice, RenderContext, Tone, View,
    common, text::t,
};
use crate::domain::{DocumentRevisionId, PrId, PrState};

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
    if let Some(item) = super::card::item_for_revision(context.projection, &revision.id, context.domain) {
        let card = super::card::from_item(item, context, true)?;
        let actions: Vec<super::Action> = card.actions.into_iter().chain(card.more.into_iter().filter(|action| action.link.is_none())).map(Into::into).collect();
        if !actions.is_empty() { view.sections.push(common::section("actions", None, vec![Item::Actions(super::ActionBar { actions })])); }
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
    if let Some(item) = super::card::item_for_pr(context.projection, &pr.id, context.domain) {
        let card = super::card::from_item(item, context, true)?;
        let (_, checks, _) = super::card::pr_overview(pr, context);
        if let Some(checks) = checks {
            view.sections
                .push(common::section("checks", None, vec![Item::Checks(checks)]));
        }
        if let Some(body) = live
            .and_then(|details| details.body.clone())
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
