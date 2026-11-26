use super::*;

impl DomainState {
    pub(super) fn snooze_guard(
        &self,
        actor: &Actor,
        id: &InteractionId,
        until: i64,
        now: i64,
        holds: &dyn Holds,
    ) -> Result<()> {
        user(actor)?;
        let card = self.interaction(id)?;
        if until <= now {
            return refuse("invalid_input", "until", "Choose a time in the future");
        }
        if matches!(&card.scope, Scope::Task { task_id } if self.tasks.get(task_id).is_some_and(|task| task.archived))
            || card.recipient != Recipient::User
            || !self.open(card)
            || !self.inbox_pending(card, |task| self.attention_given(task, holds))
        {
            return fail(
                "not_waiting",
                "Only an open card in Waiting for You can be snoozed",
            );
        }
        Ok(())
    }

    pub(super) fn unsnooze_guard(&self, actor: &Actor, id: &InteractionId) -> Result<()> {
        user(actor)?;
        let card = self.interaction(id)?;
        if card.recipient != Recipient::User || !self.open(card) || card.snoozed_until.is_none() {
            return fail("not_snoozed", "Only an open snoozed card can return now");
        }
        Ok(())
    }

    pub(super) fn resurface_card(&mut self, id: &InteractionId, effects: &mut Vec<DomainEffect>) {
        let card = self.interactions.get_mut(id).unwrap();
        card.snoozed_until = None;
        card.revision = self.revision + 1;
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

    pub(super) fn reconcile_snoozed_cards(
        &mut self,
        before: &Self,
        effects: &mut Vec<DomainEffect>,
    ) {
        let new_activity: BTreeSet<_> = self
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
        let snoozed: Vec<_> = self
            .interactions
            .values()
            .filter(|card| card.snoozed_until.is_some())
            .map(|card| card.id.clone())
            .collect();
        for id in snoozed {
            let card = &self.interactions[&id];
            let task_ended = matches!(&card.scope, Scope::Task { task_id }
                if self.tasks.get(task_id).is_some_and(Task::over)
                    && before.tasks.get(task_id).is_some_and(|task| !task.over()));
            if task_ended && self.open(card) {
                self.retire(&id);
            } else if !self.open(card) {
                let card = self.interactions.get_mut(&id).unwrap();
                card.snoozed_until = None;
                card.revision = self.revision + 1;
            } else if matches!(&card.scope, Scope::Task { task_id } if new_activity.contains(task_id))
            {
                self.resurface_card(&id, effects);
            }
        }
    }
}
