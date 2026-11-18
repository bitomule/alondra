//! What the adapters look for in a view the daemon rendered.
use alondra_view_model::{Action, Item, View};

/// The first action of `view` that `predicate` picks, in a card, a bar, a row or a form.
pub fn view_action(view: &View, predicate: impl Fn(&Action) -> bool) -> Option<Action> {
    fn scan(items: &[Item], predicate: &impl Fn(&Action) -> bool) -> Option<Action> {
        for item in items {
            match item {
                Item::Card(card) => {
                    if let Some(action) = card.actions.iter().find(|a| predicate(a)) { return Some(Action::from(action.clone())); }
                }
                Item::Actions(bar) => if let Some(action) = bar.actions.iter().find(|a| predicate(a)) { return Some(action.clone()); },
                Item::Row(row) => {
                    if let Some(action) = row.actions.iter().find(|a| predicate(a)) { return Some(action.clone()); }
                }
                Item::Form(form) => if predicate(&form.submit) { return Some(form.submit.clone()); },
                _ => {}
            }
        }
        None
    }
    view.sections.iter().find_map(|section| scan(&section.items, &predicate))
}
