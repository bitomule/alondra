use super::*;
use alondra_core::domain::*;
use anyhow::Result;
use rusqlite::{Connection, OptionalExtension, Transaction, params};
use serde_json::{Value, json};
use std::{path::Path, sync::Arc};
use alondra_agent_application::{InputMetadata, ObservationNormalizer, ObservedFact};
use alondra_domain::{RuntimeFact, TaskLifecycle, Trigger};
use alondra_core::usage::{Share, UsageReading, WindowKind, WindowReading};
use alondra_foundation::ExecutionStamp;
use alondra_provider_contract::{AdapterError, Capability, McpServerReport, McpSignInReport, OrderedObservation, Placement, PreparedCommand, ProviderBinding, TurnOutcome};
use alondra_store::{
    AdapterState, AuthorityScope, CommandEvidence, Committed, CommittedCommand, CommittedControl, ControlAttempt,
    ControlPermit, DispatchAttempt, DispatchPermit, DispatchReceipt, InputPayload, LifecycleTransaction,
    ObservationAcceptance, OwnerGrant, UnresolvedCommand, UnresolvedControl,
};

pub(super) use alondra_daemon_engine_store::testing::ObservationRefusal;
#[cfg(any(test, feature = "test-hooks"))]
pub fn admit_through_sdk(runtime: &crate::domain_runtime::DomainRuntime, task: &TaskId, agent: &str) {
    admit_through_sdk_with_host(runtime, task, agent, "fixture-service");
}

#[cfg(any(test, feature = "test-hooks"))]
pub fn admit_through_sdk_with_host(runtime: &crate::domain_runtime::DomainRuntime, task: &TaskId, agent: &str, host: &str) {
    let state = runtime.state().unwrap();
    let admit = CommandEnvelope {
        protocol_version: PROTOCOL_VERSION, data_epoch: state.data_epoch.clone(), command_id: format!("admit-{task}").into(),
        expected_revision: state.revision,
        command: Command::AdmitExecution { provider: alondra_core::accounts::Provider::Claude,
            task_id: task.clone(), agent_id: agent.into(), conversation: format!("conversation-{task}"), instance: format!("resource-{task}"),
            host: host.into(), model: None, account: None, leaves_limit: None,
        },
    };
    let binding = ProviderBinding::new("composite-binding".into(), host.into(), "fixture-instance".into(), "fixture-config".into(),
        1, 1, std::collections::BTreeSet::from([Capability::OfferInput]), "fixture-credential-reference".into(), 1).unwrap();
    runtime.own(AuthorityScope::new(state.data_epoch.clone()).unwrap(), |owner| {
        owner.stage_command(&Actor::System, &admit, Attached::None)?;
        let journal = owner.journal();
        let pending = journal.load(task)?.pending_inputs()[0].clone();
        journal.give_input_text(task, InputPayload::new(pending, "Fixture input").unwrap())?;
        journal.stage_launch(task, &Placement::RemoteWorkspace("fixture/workspace".into()), &binding)?;
        let execution = journal.load(task)?.execution().unwrap().clone();
        journal.reserve_owner(&execution, 0)?;
        Ok(())
    }).unwrap();
}

#[cfg(test)]
#[path = "owner_tests.rs"]
pub(super) mod tests;
