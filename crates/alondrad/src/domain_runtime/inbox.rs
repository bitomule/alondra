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
}
