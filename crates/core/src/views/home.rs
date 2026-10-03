use super::{
    BuildError, Client, Item, Link, Notice, RenderContext, Row, Section, TimeFormat, Tone,
    Trailing, VIEWS_PROTOCOL, View,
    card::{card_key, from_item, orchestrator_decision, plain_id, public_id},
    text::t,
};
use crate::domain_v2::{DomainProjection, InboxItem};

pub(super) fn build(key: &str, context: &RenderContext<'_>) -> Result<View, BuildError> {
    if !matches!(context.client, Client::Ios | Client::Tui) {
        return Err(BuildError::UnsupportedClient);
    }
    let focus = key.strip_prefix("home?focus=");
    if focus.is_some_and(|id| !plain_id(id)) {
        return Err(BuildError::InvalidKey);
    }
    let projection = context.domain.projection();
    let mut roots = waiting(&projection);
    if let Some(focus) = focus {
        if let Some(index) = roots.iter().position(|item| {
            public_id(item) == focus || item.children.iter().any(|child| public_id(child) == focus)
        }) {
            roots.rotate_left(index);
        } else {
            return Err(BuildError::NotFound);
        }
    }
    let status = super::status::build(&projection, context);
    let decision = orchestrator_decision(context, false);
    let (sections, empty) = match context.client {
        Client::Ios => {
            let card_ids: Vec<String> = decision.iter().map(|card| card.id.clone())
                .chain(roots.iter().map(|item| public_id(item).to_owned()))
                .collect();
            let count = card_ids.len() as u32;
            (
                vec![Section {
                    id: "queue".into(),
                    title: Some(t!(context.lang, "Waiting for You", "Te espera").into()),
                    count: Some(count),
                    collapsed: false,
                    items: vec![],
                    card_ids,
                    more: None,
                }],
                (count == 0).then(|| Notice {
                    text: t!(
                        context.lang,
                        "Nothing is waiting for you",
                        "Nada espera tu respuesta"
                    )
                    .into(),
                    tone: Tone::Neutral,
                    action: None,
                }),
            )
        }
        Client::Tui => {
            let mut sections = [
                Section {
                    id: "waiting".into(),
                    title: Some(t!(context.lang, "Waiting for You", "Te espera").into()),
                    count: None,
                    collapsed: false,
                    items: vec![],
                    card_ids: vec![],
                    more: None,
                },
                Section {
                    id: "pull_requests".into(),
                    title: Some(t!(context.lang, "Pull requests", "Solicitudes de cambios").into()),
                    count: None,
                    collapsed: false,
                    items: vec![],
                    card_ids: vec![],
                    more: None,
                },
                Section {
                    id: "to_read".into(),
                    title: Some(t!(context.lang, "To Read", "Por leer").into()),
                    count: None,
                    collapsed: false,
                    items: vec![],
                    card_ids: vec![],
                    more: None,
                },
            ];
            for item in &roots {
                let index = if item.kind == "pull_request" {
                    1
                } else if item.kind == "information" {
                    2
                } else {
                    0
                };
                sections[index].items.push(Item::Row(row(item, context, 0)?));
            }
            if let Some(card) = &decision {
                sections[0].items.insert(0, Item::Row(Row {
                    id: card.id.clone(), link: card.link.clone(), icon: card.icon, title: card.title.clone(),
                    subtitle: card.subtitle.clone(), badges: vec![], trailing: None, depth: 0, struck: false,
                    children: vec![], actions: vec![],
                }));
            }
            let mut sections: Vec<Section> = sections.into_iter().filter(|section| !section.items.is_empty()).collect();
            let detail = match roots.first() {
                Some(first) if focus.is_some() || decision.is_none() => Some(from_item(first, context, true)?),
                _ => orchestrator_decision(context, true),
            };
            if let Some(detail) = detail {
                sections.push(Section {
                    id: "detail".into(),
                    title: None,
                    count: None,
                    collapsed: false,
                    items: vec![Item::Card(detail)],
                    card_ids: vec![],
                    more: None,
                });
            }
            (
                sections,
                (roots.is_empty() && decision.is_none()).then(|| Notice {
                    text: t!(
                        context.lang,
                        "Nothing is waiting for you",
                        "Nada espera tu respuesta"
                    )
                    .into(),
                    tone: Tone::Neutral,
                    action: None,
                }),
            )
        }
        Client::Mac => unreachable!(),
    };
    Ok(View {
        protocol: VIEWS_PROTOCOL,
        data_epoch: context.domain.data_epoch.0.clone(),
        view: "home".into(),
        key: key.into(),
        client: context.client,
        version: String::new(),
        cursor: context.cursor.into(),
        title: None,
        status,
        sections,
        empty,
        notices: vec![],
    })
}
/// What waits for the user: the home queue, its count and the app icon badge. The inbox only
/// holds what is addressed to the user, the orchestrator's answers and questions included.
pub(super) fn waiting(projection: &DomainProjection) -> Vec<&InboxItem> {
    projection.inbox.iter().filter(|item| item.pending).collect()
}
fn row(item: &InboxItem, context: &RenderContext<'_>, depth: u8) -> Result<Row, BuildError> {
    let card = from_item(item, context, false)?;
    Ok(Row {
        id: card.id,
        link: Some(Link::View {
            key: card_key(item),
        }),
        icon: card.icon,
        title: card.title,
        subtitle: card.subtitle,
        badges: card.badges,
        trailing: context.live.time(&item.id).map(|at| Trailing::Time {
            at: at.into(),
            format: TimeFormat::Relative,
        }),
        depth,
        struck: false,
        children: item
            .children
            .iter()
            .map(|child| row(child, context, depth + 1))
            .collect::<Result<Vec<_>, _>>()?,
        actions: vec![],
    })
}
