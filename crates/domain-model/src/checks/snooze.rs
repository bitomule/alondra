use super::*;

impl DomainState {
    pub fn snooze_guard(
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

    pub fn unsnooze_guard(&self, actor: &Actor, id: &InteractionId) -> Result<()> {
        user(actor)?;
        let card = self.interaction(id)?;
        if card.recipient != Recipient::User || !self.open(card) || card.snoozed_until.is_none() {
            return fail("not_snoozed", "Only an open snoozed card can return now");
        }
        Ok(())
    }


}
