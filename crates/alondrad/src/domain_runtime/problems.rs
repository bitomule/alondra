//! Alondra's own cards: a failure it puts in front of the user as a Problem, an ordinary
//! interaction every client renders like any other card.
use super::*;

impl DomainRuntime {
    /// A Problem authored by Alondra. `id` names it: raising it twice leaves the first. A newer
    /// Problem with the same `cause` in the same place replaces the open one.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn raise_problem(
        &self,
        id: &str,
        cause: Option<&str>,
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
            data_epoch: state.data_epoch.clone(),
            command_id: format!("raise-{id}").into(),
            expected_revision: state.revision,
            command: Command::RaiseProblem {
                attachments: vec![],
                interaction_id: id.to_owned().into(),
                scope,
                body: diagnosis.clone(),
                options,
                card: CardSpec { cause: cause.map(str::to_owned), ..CardSpec::new(CardKind::Problem, need).why(why).content(ContentKind::Diagnosis, diagnosis) },
            },
        })
    }

    /// A Problem whose `body` holds the numbers clients word in their own language, with no
    /// content of its own. `id` names it: raising it twice leaves the first.
    pub(crate) fn raise_worded_problem(
        &self,
        id: &str,
        cause: &str,
        need: String,
        why: String,
        body: String,
        options: Vec<QuestionOption>,
        recommended: Option<&str>,
    ) -> Result<CommandOutcome, StoreError> {
        let state = self.state().map_err(StoreError::Storage)?;
        if state.interactions.contains_key(&InteractionId(id.to_owned())) {
            return Err(DomainError::new("invalid_input", "Problem already raised").into());
        }
        let mut card = CardSpec::new(CardKind::Problem, need).why(why);
        card.recommended = recommended.map(str::to_owned);
        self.execute_as_system(CommandEnvelope {
            protocol_version: PROTOCOL_VERSION,
            data_epoch: state.data_epoch.clone(),
            command_id: format!("raise-{id}").into(),
            expected_revision: state.revision,
            command: Command::RaiseProblem {
                attachments: vec![],
                interaction_id: id.to_owned().into(),
                scope: Scope::Orchestrator { request_id: None },
                body,
                options,
                card: card.cause(cause),
            },
        })
    }

    /// Something the orchestrator hears in its next batch; `id` names it, so telling it twice
    /// leaves the first.
    pub(crate) fn tell_orchestrator(&self, id: &str, body: String) -> Result<CommandOutcome, StoreError> {
        let state = self.state().map_err(StoreError::Storage)?;
        if state.interactions.contains_key(&InteractionId(id.to_owned())) {
            return Err(DomainError::new("invalid_input", "Already told").into());
        }
        self.execute_as_system(CommandEnvelope {
            protocol_version: PROTOCOL_VERSION,
            data_epoch: state.data_epoch.clone(),
            command_id: format!("tell-{id}").into(),
            expected_revision: state.revision,
            command: Command::PublishInformation {
                attachments: vec![],
                interaction_id: id.to_owned().into(),
                scope: Scope::Orchestrator { request_id: None },
                recipient: Recipient::Orchestrator,
                card: CardSpec::derived(CardKind::Know, &body),
                body,
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
            data_epoch: state.data_epoch.clone(),
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

    /// A Decide card authored by Alondra that holds nothing back. `need` and `why` are the English
    /// source; clients render the text in their own language from the id and `body`. `id` names
    /// it, so asking twice leaves the first.
    pub(crate) fn ask_user(
        &self,
        id: &str,
        need: String,
        why: String,
        body: String,
        options: Vec<QuestionOption>,
        recommended: &str,
    ) -> Result<CommandOutcome, StoreError> {
        let state = self.state().map_err(StoreError::Storage)?;
        if state.interactions.contains_key(&InteractionId(id.to_owned())) {
            return Err(DomainError::new("invalid_input", "Question already asked").into());
        }
        let mut card = CardSpec::new(CardKind::Decide, need).why(why);
        card.recommended = Some(recommended.to_owned());
        self.execute_as_system(CommandEnvelope {
            protocol_version: PROTOCOL_VERSION,
            data_epoch: state.data_epoch.clone(),
            command_id: format!("ask-{id}").into(),
            expected_revision: state.revision,
            command: Command::AskQuestion {
                attachments: vec![],
                interaction_id: id.to_owned().into(),
                scope: Scope::Orchestrator { request_id: None },
                recipient: Recipient::User,
                body,
                blocking: false,
                options,
                card,
            },
        })
    }

    /// A card of Alondra's that stopped mattering.
    pub(crate) fn withdraw_problem(&self, id: &InteractionId) -> Result<CommandOutcome, StoreError> {
        let state = self.state().map_err(StoreError::Storage)?;
        self.execute_as_system(CommandEnvelope {
            protocol_version: PROTOCOL_VERSION,
            data_epoch: state.data_epoch.clone(),
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
