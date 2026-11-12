//! What Alondra settles itself in the orchestrator's inbox: what it handled, and what it owes past its deadline.
use super::*;

impl DomainRuntime {
    /// A request nothing is left to do for: a message acted on, or one the user chose to close.
    pub(crate) fn close_request_by_system(&self, request_id: &RequestId, withdrawn: bool) -> Result<CommandOutcome, StoreError> {
        let state = self.state().map_err(StoreError::Storage)?;
        self.execute_as_system(CommandEnvelope {
            protocol_version: PROTOCOL_VERSION,
            data_epoch: state.data_epoch.clone(),
            command_id: format!("settle-{request_id}").into(),
            expected_revision: state.revision,
            command: Command::CloseRequest { request_id: request_id.clone(), answer: None, withdrawn, title: None, headline: None, folded_into: None },
        })
    }

    /// A stopped node's case nobody settled in time: Alondra settles it itself, never asking the user.
    pub(crate) fn settle_unsettled_stall(&self, case_id: &InteractionId, task_id: &TaskId, outcome: StallOutcome) -> Result<CommandOutcome, StoreError> {
        let state = self.state().map_err(StoreError::Storage)?;
        self.execute_as_system(CommandEnvelope {
            protocol_version: PROTOCOL_VERSION,
            data_epoch: state.data_epoch.clone(),
            command_id: format!("settle-{case_id}").into(),
            expected_revision: state.revision,
            command: Command::SettleStall { case_id: case_id.clone(), task_id: task_id.clone(), outcome },
        })
    }

    /// The plan of a held task nobody reviewed in time: it starts with it, and `outcome` says why.
    pub(crate) fn settle_plan_review_by_system(&self, review_id: &InteractionId, task_id: &TaskId, outcome: PlanReviewOutcome) -> Result<CommandOutcome, StoreError> {
        let state = self.state().map_err(StoreError::Storage)?;
        self.execute_as_system(CommandEnvelope {
            protocol_version: PROTOCOL_VERSION,
            data_epoch: state.data_epoch.clone(),
            command_id: format!("settle-{review_id}-{task_id}").into(),
            expected_revision: state.revision,
            command: Command::SettlePlanReview { review_id: review_id.clone(), task_id: task_id.clone(), outcome },
        })
    }
}
