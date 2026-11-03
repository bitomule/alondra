//! What Alondra settles itself in the orchestrator's inbox once it handled what it was handed.
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
            command: Command::CloseRequest { request_id: request_id.clone(), answer: None, withdrawn, title: None, headline: None },
        })
    }

    /// A stopped node's case the orchestrator left unsettled after its reminder: Alondra closes
    /// the task itself, never asking the user, or leaves a delivered one with the user.
    pub(crate) fn close_unsettled_stall(&self, case_id: &InteractionId, task_id: &TaskId) -> Result<CommandOutcome, StoreError> {
        let state = self.state().map_err(StoreError::Storage)?;
        self.execute_as_system(CommandEnvelope {
            protocol_version: PROTOCOL_VERSION,
            data_epoch: state.data_epoch.clone(),
            command_id: format!("close-{case_id}").into(),
            expected_revision: state.revision,
            command: Command::SettleStall { case_id: case_id.clone(), task_id: task_id.clone(), outcome: StallOutcome::Unsettled },
        })
    }
}
