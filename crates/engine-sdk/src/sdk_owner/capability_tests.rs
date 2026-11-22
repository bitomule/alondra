//! A node's capability lives as long as its host: every way a host ends under this daemon retires
//! it, and only a daemon that dies whole leaves it for the host the next daemon adopts. Before this,
//! a host never took its capability with it, and the registry filled up on 17 November.
use crate::domain_runtime::DomainRuntime;
use crate::sdk_owner::SdkOwners as _;
use super::*;

fn holds_capability(domain: &DomainRuntime, agent: &AgentId) -> bool {
    let registry: serde_json::Value = serde_json::from_slice(&std::fs::read(domain.home().join("domain-capabilities.json")).unwrap()).unwrap();
    registry["entries"].as_array().unwrap().iter().any(|entry| entry["principal"]["session_id"] == agent.0.as_str())
}

#[test]
fn a_host_the_daemon_stops_takes_its_nodes_capability_with_it() {
    let slice = Slice::new();
    let mut driver = slice.launch();
    slice.settle(&mut driver);
    let agent = driver.agent().clone();
    assert!(holds_capability(&slice.domain, &agent), "the running node holds its capability");
    slice.domain.add_sdk_owner(Owner::spawn(driver));
    slice.domain.stop_sdk_owners();
    assert!(!holds_capability(&slice.domain, &agent), "the stopped host's capability is retired");
}

#[test]
fn a_daemon_that_dies_leaves_the_capability_for_the_adopted_host_which_takes_it_when_it_stops() {
    let slice = Slice::new();
    let mut driver = slice.launch();
    slice.settle(&mut driver);
    let agent = driver.agent().clone();
    slice.domain.add_sdk_owner(Owner::spawn(driver));
    slice.domain.crash_sdk_owners();
    assert!(holds_capability(&slice.domain, &agent), "the host runs on, and its Claude keeps reaching Alondra");
    let restarted = slice.restart();
    assert!(holds_capability(&restarted, &agent), "the next daemon keeps the capability of the host it adopts");
    let adopted = adopt(restarted.clone(), &slice.backend, TASK.into(), Some(&slice.plugin)).unwrap();
    restarted.add_sdk_owner(Owner::spawn(adopted));
    restarted.stop_sdk_owners();
    assert!(!holds_capability(&restarted, &agent));
}

#[test]
fn a_task_whose_execution_was_lost_keeps_no_capability_for_its_node() {
    let slice = Slice::new();
    let mut driver = slice.launch();
    slice.settle(&mut driver);
    let agent = driver.agent().clone();
    slice.domain.add_sdk_owner(Owner::spawn(driver));
    slice.domain.crash_sdk_owners();
    let restarted = slice.restart();
    assert!(holds_capability(&restarted, &agent));
    lost(&*restarted, &TASK.into(), "its host could not be taken over");
    assert!(!holds_capability(&restarted, &agent), "a node no host runs keeps no capability");
    let adopted = adopt(restarted.clone(), &slice.backend, TASK.into(), Some(&slice.plugin)).unwrap();
    restarted.add_sdk_owner(Owner::spawn(adopted));
    restarted.stop_sdk_owners();
}
