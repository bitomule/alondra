//! Alondra's own cards: a failure it puts in front of the user as a Problem, an ordinary
//! interaction every client renders like any other card.
use super::*;

impl DomainRuntime {
    /// A Problem authored by Alondra. `id` names it: raising it twice leaves the first.
    pub(crate) fn raise_problem(
        &self,
        id: &str,
        scope: Scope,
        need: String,
        why: String,
        diagnosis: String,
        options: Vec<QuestionOption>,
    ) -> Result<CommandOutcome, StoreError> {
        let state = self.state().map_err(StoreError::Storage)?;
        if state.interactions.contains_key(&InteractionId(id.to_owned())) {
            return Err(DomainError::new("invalid_input", "Problem already raised").into());
        }
        self.execute_as_system(CommandEnvelope {
            protocol_version: PROTOCOL_VERSION,
            data_epoch: state.data_epoch,
            command_id: format!("raise-{id}").into(),
            expected_revision: state.revision,
            command: Command::RaiseProblem {
                attachments: vec![],
                interaction_id: id.to_owned().into(),
                scope,
                body: diagnosis.clone(),
                options,
                card: CardSpec::new(CardKind::Problem, need).why(why).content(ContentKind::Diagnosis, diagnosis),
            },
        })
    }

    /// A Know card authored by Alondra, shown to the user once: `id` names it, so publishing it
    /// again leaves the first.
    pub(crate) fn publish_notice(&self, id: &str, body: String) -> Result<CommandOutcome, StoreError> {
        let state = self.state().map_err(StoreError::Storage)?;
        if state.interactions.contains_key(&InteractionId(id.to_owned())) {
            return Err(DomainError::new("invalid_input", "Notice already published").into());
        }
        self.execute_as_system(CommandEnvelope {
            protocol_version: PROTOCOL_VERSION,
            data_epoch: state.data_epoch,
            command_id: format!("notice-{id}").into(),
            expected_revision: state.revision,
            command: Command::PublishInformation {
                attachments: vec![],
                interaction_id: id.to_owned().into(),
                scope: Scope::Orchestrator { request_id: None },
                recipient: Recipient::User,
                card: CardSpec::derived(CardKind::Know, &body),
                body,
            },
        })
    }

    /// A card of Alondra's that stopped mattering.
    pub(crate) fn withdraw_problem(&self, id: &InteractionId) -> Result<CommandOutcome, StoreError> {
        let state = self.state().map_err(StoreError::Storage)?;
        self.execute_as_system(CommandEnvelope {
            protocol_version: PROTOCOL_VERSION,
            data_epoch: state.data_epoch,
            command_id: format!("withdraw-{id}-{}", state.revision).into(),
            expected_revision: state.revision,
            command: Command::Withdraw { interaction_id: id.clone() },
        })
    }

    fn execute_as_system(&self, mut envelope: CommandEnvelope) -> Result<CommandOutcome, StoreError> {
        for _ in 0..5 {
            match self.execute(&Actor::System, &envelope, None, || Ok(())) {
                Err(StoreError::Domain(error)) if error.code == "stale_revision" => {
                    let Ok(state) = self.state() else { break };
                    envelope.expected_revision = state.revision;
                }
                result => return result,
            }
        }
        Err(DomainError::new("stale_revision", "Alondra remained busy").into())
    }
}
