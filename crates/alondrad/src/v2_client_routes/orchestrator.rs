//! Recovery of the orchestrator's interrupted run: stop it, continue without repeating it, or
//! run it again. The runner that owns the run answers; this module only carries the call.
use super::{Client, Reply};
use alondra_core::domain_v2::DomainError;
use serde::Deserialize;
use serde_json::{Value, json};
use std::sync::Arc;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Step {
    Cancel,
    Acknowledge,
    Retry,
}

/// What the runner of the orchestrator offers the daemon's clients.
pub(crate) trait OrchestratorControl: Send + Sync + 'static {
    /// The public status of the orchestrator's run, as `/v2/runtime` shows it.
    fn status(&self) -> Option<Value>;
    fn recover(&self, step: Step, run_id: &str) -> Result<(), DomainError>;
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Body {
    run_id: String,
}

impl Client {
    pub(crate) fn attach_orchestrator(&self, control: Arc<dyn OrchestratorControl>) {
        *self.orchestrator.lock().expect("orchestrator control poisoned") = Some(control);
    }

    fn orchestrator_control(&self) -> Option<Arc<dyn OrchestratorControl>> {
        self.orchestrator.lock().expect("orchestrator control poisoned").clone()
    }

    pub(super) fn orchestrator_status(&self) -> Option<Value> {
        self.orchestrator_control()?.status()
    }

    pub(super) fn recover_orchestrator(&self, step: Step, body: &Value) -> Reply {
        let Ok(Body { run_id }) = serde_json::from_value(body.clone()) else {
            return Reply::error(DomainError::new("invalid_input", "invalid orchestrator recovery"));
        };
        let Some(control) = self.orchestrator_control() else {
            return Reply::error(DomainError::new("orchestrator_unavailable", "the orchestrator is not running on this daemon"));
        };
        match control.recover(step, &run_id) {
            Ok(()) => Reply::ok(json!({})),
            Err(error) => Reply::error(error),
        }
    }
}
