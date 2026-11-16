//! What an author reads about its own open cards, to replace or withdraw them.
use super::*;
use serde::{Deserialize, Serialize};

/// One open card its author may replace (`card.replaces`) or withdraw.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct OpenCard {
    pub id: InteractionId,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub task_id: Option<TaskId>,
    pub kind: CardKind,
    #[serde(default)]
    pub title: String,
    pub need: String,
    /// The slot it holds: a new card in it replaces this one without `replaces`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub slot: Option<CardSlot>,
    pub blocking: bool,
    /// Held by a Claude Code hook (a plan or a permission): it ends with its hook.
    pub held: bool,
    pub revision: u64,
}

impl DomainState {
    pub fn open_card_rows(&self, reader: &Actor, task: Option<&TaskId>) -> Vec<OpenCard> {
        self.open_cards(reader, task).into_iter().map(|i| OpenCard {
            id: i.id.clone(),
            task_id: match &i.scope {
                Scope::Task { task_id } => Some(task_id.clone()),
                Scope::Orchestrator { .. } => None,
            },
            kind: i.card.kind,
            title: i.card.effective_title().to_owned(),
            need: i.card.need.clone(),
            slot: CardSlot::of(&i.card, &i.payload),
            blocking: i.blocking,
            held: self.holds.contains_key(&i.id),
            revision: i.order,
        }).collect()
    }
}
