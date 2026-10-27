use super::{
    BuildError, Client, Item, Notice, RenderContext, Section, Tone, VIEWS_PROTOCOL, View,
    card::{self, from_item, plain_id, public_id},
    text::t,
};
use crate::domain::InboxItem;

pub(super) fn build(key: &str, context: &RenderContext<'_>) -> Result<View, BuildError> {
    if !matches!(context.client, Client::Ios | Client::Tui) {
        return Err(BuildError::UnsupportedClient);
    }
    let focus = key.strip_prefix("home?focus=");
    if focus.is_some_and(|id| !plain_id(id)) {
        return Err(BuildError::InvalidKey);
    }
    let mut roots: Vec<&InboxItem> = context.waiting().iter().collect();
    if let Some(focus) = focus {
        if let Some(index) = roots.iter().position(|item| public_id(item) == focus) {
            roots.rotate_left(index);
        } else {
            return Err(BuildError::NotFound);
        }
    }
    let status = super::status::build(context);
    let first_cards = match context.client {
        Client::Ios => card::first_cards(roots.iter().map(|item| public_id(item)), context)?,
        _ => vec![],
    };
    let (sections, empty) = match context.client {
        Client::Ios => {
            let card_ids: Vec<String> = roots.iter().map(|item| public_id(item).to_owned()).collect();
            let card_waits = roots.iter().map(|item| waits_for(item).to_owned()).collect();
            let count = card_ids.len() as u32;
            (
                vec![Section {
                    id: "queue".into(),
                    title: Some(t!(context.lang, "Waiting for You", "Te espera").into()),
                    count: Some(count),
                    collapsed: false,
                    items: vec![],
                    card_ids,
                    card_waits,
                    more: None, style: None, actions: vec![],
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
                card_waits: vec![],
                more: None, style: None, actions: vec![],
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
                    card_waits: vec![],
                    more: None, style: None, actions: vec![],
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
        service: None,
        account_configuration: None,
        first_cards,
        summary: None,
    })
}
/// What a waiting card asks of the user: an answer ("question") or only to be read ("read").
pub(super) fn waits_for(item: &InboxItem) -> &'static str {
    if matches!(item.kind.as_str(), "question" | "action_request" | "permission" | "plan") { "question" } else { "read" }
}
