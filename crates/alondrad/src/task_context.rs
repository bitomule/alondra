use crate::domain_authority::Principal;
use crate::domain_runtime::DomainRuntime;
use alondra_core::domain::{AgentId, DomainError, TaskId};
use alondra_store::ContextRef;
use serde::Deserialize;
use serde_json::Value;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Query {
    task_id: TaskId,
    #[serde(default)]
    after: Option<ContextRef>,
    #[serde(default = "page_size")]
    limit: usize,
}
fn page_size() -> usize { 32 }

pub(crate) fn read(domain: &DomainRuntime, bearer: &str, query: Query) -> Result<Value, DomainError> {
    read_after_check(domain, bearer, query, || {})
}

fn read_after_check(domain: &DomainRuntime, bearer: &str, query: Query, after_check: impl FnOnce()) -> Result<Value, DomainError> {
    let principal = domain.resolve_bearer(bearer).map_err(|_| DomainError::new("forbidden", "A current node capability is required"))?;
    let Principal::Agent { session_id, task_binding, .. } = principal else { return Err(DomainError::new("forbidden", "Only a task's own node reads its portable conversation")); };
    let read = domain.read();
    let node = AgentId::from(session_id);
    if task_binding.as_deref().is_some_and(|task| task != query.task_id.0)
        || read.index().task_of_node(&node) != Some(&query.task_id)
    { return Err(DomainError::new("not_assigned", "This conversation belongs to another task")); }
    if !(1..=128).contains(&query.limit) || query.after.as_ref().is_some_and(|reference| reference.task != query.task_id) {
        return Err(DomainError::new("invalid_input", "Invalid task context page"));
    }
    drop(read);
    after_check();
    let page = domain.read_current_task_context(bearer, &query.task_id, query.after.as_ref(), query.limit)?;
    serde_json::to_value(page).map_err(|_| DomainError::new("storage_unavailable", "The retained task conversation could not be encoded"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use alondra_core::domain::*;
    use crate::test_support::Directory;

    #[test]
    fn current_batch_manifest_rejects_a_still_issued_bearer_after_generation_advances() {
        let home=Directory::new();std::fs::create_dir_all(&home).unwrap();let domain=DomainRuntime::open_at(&home,&home.join("domain.db")).unwrap();
        let generation=domain.read().state().orchestrator_generation;
        let old=domain.issue(Principal::Orchestrator{generation,execution_id:"old-batch".into()}).unwrap();
        assert!(domain.read_batch_outcomes(old.expose_for_child_environment(),None,true,"",7).is_ok());
        let state=domain.read();let envelope=CommandEnvelope{protocol_version:PROTOCOL_VERSION,data_epoch:state.state().data_epoch.clone(),command_id:"advance-manifest-owner".into(),expected_revision:0,command:Command::AdvanceOrchestratorGeneration{generation:generation+1}};drop(state);
        domain.execute_fixture(&Actor::System,envelope).unwrap();
        assert_eq!(domain.read_batch_outcomes(old.expose_for_child_environment(),None,true,"",7).unwrap_err().code,"stale_generation");
        let current=domain.issue(Principal::Orchestrator{generation:generation+1,execution_id:"current-batch".into()}).unwrap();
        assert!(domain.read_batch_outcomes(current.expose_for_child_environment(),None,true,"",7).is_ok());
    }
    #[test]
    fn portable_context_reader_requires_the_current_assigned_node_and_own_cursor() {
        let home = Directory::new();
        std::fs::create_dir_all(home.join("work")).unwrap();
        let domain = DomainRuntime::open_at(&home, &home.join("domain.db")).unwrap();
        let run = |actor: Actor, id: &str, command| {
            let state = domain.state().unwrap();
            domain.execute_fixture(&actor, CommandEnvelope { protocol_version: PROTOCOL_VERSION, data_epoch: state.data_epoch.clone(), command_id: id.into(), expected_revision: 0, command }).unwrap();
        };
        run(Actor::Orchestrator { generation: 0 }, "request", Command::CreateRequest { request_id: "request".into(), body: "Work".into(), auto_merge: false, maintenance: false, message: false });
        run(Actor::System, "agent", Command::RegisterAgent { agent_id: "node".into(), name: "Node".into(), at: 0 });
        run(Actor::Orchestrator { generation: 0 }, "task", Command::CreateTask {
            plan_items: None, continues: None, task_id: "task".into(), request_id: "request".into(), title: "Work".into(), agent_id: Some("node".into()), review_prs: false,
            dependencies: Default::default(), stack: None, launch: Some(LaunchPlan::new("node", home.join("work").to_str().unwrap())),
        });
        let query = || Query { task_id: "task".into(), after: None, limit: 10 };
        let first = domain.bind_agent("node").unwrap();
        assert_eq!(read(&domain, first.expose_for_child_environment(), query()).unwrap()["entries"], serde_json::json!([]));
        let other = Query { task_id: "another-task".into(), after: None, limit: 10 };
        assert_eq!(read(&domain, first.expose_for_child_environment(), other).unwrap_err().code, "not_assigned");
        let forged = Query { task_id: "task".into(), after: Some(ContextRef { task: "another-task".into(), sequence: 1, generation: 1, cursor: 1 }), limit: 10 };
        assert_eq!(read(&domain, first.expose_for_child_environment(), forged).unwrap_err().code, "invalid_input");
        let revoked_during_read = read_after_check(&domain, first.expose_for_child_environment(), query(), || {
            domain.bind_agent("node").unwrap();
        }).unwrap_err();
        assert_eq!(revoked_during_read.code, "forbidden", "authorization and the retained page share the current capability lease");
        let replacement = domain.bind_agent("node").unwrap();
        assert_eq!(read(&domain, first.expose_for_child_environment(), query()).unwrap_err().code, "forbidden");
        assert!(read(&domain, replacement.expose_for_child_environment(), query()).is_ok());
        let orchestrator = domain.issue(Principal::Orchestrator { generation: 0, execution_id: "batch".into() }).unwrap();
        assert_eq!(read(&domain, orchestrator.expose_for_child_environment(), query()).unwrap_err().code, "forbidden");
    }
}
