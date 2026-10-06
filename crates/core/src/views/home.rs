use super::{
    BuildError, Client, Item, Notice, RenderContext, Section, Tone, VIEWS_PROTOCOL, View,
    card::{from_item, plain_id, public_id},
    text::t,
};
use crate::domain::{DomainProjection, InboxItem};

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
        if let Some(index) = roots.iter().position(|item| public_id(item) == focus) {
            roots.rotate_left(index);
        } else {
            return Err(BuildError::NotFound);
        }
    }
    let status = super::status::build(&projection, context);
    let (sections, empty) = match context.client {
        Client::Ios => {
            let card_ids: Vec<String> = roots.iter().map(|item| public_id(item).to_owned()).collect();
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
            let mut items = vec![];
            for item in &roots {
                items.push(Item::Card(from_item(item, context, false)?));
            }
            let mut sections = vec![Section {
                id: "waiting".into(),
                title: Some(t!(context.lang, "Waiting for You", "Te espera").into()),
                count: Some(items.len() as u32),
                collapsed: false,
                items,
                card_ids: vec![],
                more: None,
            }];
            sections.retain(|section| !section.items.is_empty());
            let detail = match roots.first() {
                Some(first) => Some(from_item(first, context, true)?),
                None => None,
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
                roots.is_empty().then(|| Notice {
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
