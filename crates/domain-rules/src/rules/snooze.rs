use super::*;

pub(crate) fn resurface_card(domain: &mut DomainState, id: &InteractionId, effects: &mut Vec<DomainEffect>) {
    let card = domain.interactions.get_mut(id).unwrap();
    card.snoozed_until = None;
    card.revision = domain.revision + 1;
    card.resurfaced_at_revision = Some(card.revision);
    let task_id = match &card.scope {
        Scope::Task { task_id } => Some(task_id.clone()),
        _ => None,
    };
    effects.push(DomainEffect::NotifyUser {
        task_id,
        interaction_id: id.clone(),
    });
}
pub(crate) fn reconcile_snoozed_cards(
    domain: &mut DomainState,
    before: &DomainState,
    effects: &mut Vec<DomainEffect>,
) {
    let new_activity: BTreeSet<_> = domain
        .interactions
        .values()
        .filter(|card| {
            !before.interactions.contains_key(&card.id)
                && matches!(card.author, Author::Agent { .. })
        })
        .filter_map(|card| match &card.scope {
            Scope::Task { task_id } => Some(task_id.clone()),
            _ => None,
        })
        .collect();
    let snoozed: Vec<_> = domain
        .interactions
        .values()
        .filter(|card| card.snoozed_until.is_some())
        .map(|card| card.id.clone())
        .collect();
    for id in snoozed {
        let card = &domain.interactions[&id];
        let task_ended = matches!(&card.scope, Scope::Task { task_id }
            if domain.tasks.get(task_id).is_some_and(Task::over)
                && before.tasks.get(task_id).is_some_and(|task| !task.over()));
        if task_ended && domain.open(card) {
            domain.retire(&id);
        } else if !domain.open(card) {
            let card = domain.interactions.get_mut(&id).unwrap();
            card.snoozed_until = None;
            card.revision = domain.revision + 1;
        } else if matches!(&card.scope, Scope::Task { task_id } if new_activity.contains(task_id))
        {
            resurface_card(domain, &id, effects);
        }
    }
}
