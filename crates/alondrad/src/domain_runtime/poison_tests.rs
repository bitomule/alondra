use super::*;
use crate::locks::{assert_exited_for, child_home, in_child, poison_rw};
use crate::test_support::Directory;

fn open(home: &Path) -> Arc<DomainRuntime> {
    std::fs::create_dir_all(home).unwrap();
    Arc::new(DomainRuntime::open(home).unwrap())
}

fn register(runtime: &DomainRuntime, agent: &str) -> Result<CommandOutcome, StoreError> {
    let state = runtime.state().unwrap();
    runtime.execute(&Actor::System, &CommandEnvelope {
        protocol_version: PROTOCOL_VERSION,
        data_epoch: state.data_epoch.clone(),
        command_id: agent.into(),
        expected_revision: state.revision,
        command: Command::RegisterAgent { agent_id: agent.into(), name: "A".into(), at: 0 },
    }, None, || Ok(()))
}

#[test]
#[ignore = "run by a_command_after_a_panic_holding_the_domain_store_exits_the_daemon"]
fn store_child() {
    let Some(home) = child_home() else { return };
    let runtime = open(&home);
    runtime.store.poison();
    let _ = register(&runtime, "agent-a");
    unreachable!("a command ran on a poisoned domain store");
}

#[test]
fn a_command_after_a_panic_holding_the_domain_store_exits_the_daemon() {
    let home = Directory::new();
    assert_exited_for(&in_child("domain_runtime::poison_tests::store_child", &home), "the domain store");
}

#[test]
fn reads_and_commits_go_on_after_a_panic_holding_the_snapshot() {
    let home = Directory::new();
    let runtime = open(&home);
    poison_rw(&runtime.snapshot);
    let before = runtime.state().unwrap().revision;
    register(&runtime, "agent-a").unwrap();
    let after = runtime.state().unwrap();
    assert_eq!(after.revision, before + 1);
    assert!(after.agents.contains_key(&AgentId::from("agent-a")));
}

#[test]
fn capabilities_are_issued_and_resolved_after_a_panic_holding_the_authority() {
    let home = Directory::new();
    let runtime = open(&home);
    let earlier = runtime.bind_orchestrator("run-1").unwrap();
    runtime.authority.poison();
    assert!(runtime.resolve_bearer(earlier.expose_for_child_environment()).is_ok());
    let later = runtime.bind_orchestrator("run-2").unwrap();
    assert!(runtime.resolve_bearer(later.expose_for_child_environment()).is_ok());
}
