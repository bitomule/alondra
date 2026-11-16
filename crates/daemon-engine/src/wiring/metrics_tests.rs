//! A node's turn, read by its live session and captured by the kernel's metrics.
use crate::domain_store::DomainStore;
use crate::domain_store::tests::{Fixture, seeded};
use crate::live::LiveOf as _;
use crate::metrics_capture::node_turn;
use alondra_core::domain::*;
use alondra_core::metrics::steps::StepCounts;
use serde_json::{Value, json};

fn assistant(id: &str, parent: Option<&str>, tools: Value) -> Value {
    json!({"type": "assistant", "parent_tool_use_id": parent, "session_id": "native", "message": {"id": id, "model": "claude-opus-5-5", "content": tools}})
}

fn result() -> Value {
    json!({"type": "result", "subtype": "success", "is_error": false, "session_id": "native", "result": "Done", "total_cost_usd": 1.62,
        "modelUsage": {
            "claude-opus-5-5": {"inputTokens": 120, "outputTokens": 4_000, "cacheReadInputTokens": 900_000, "cacheCreationInputTokens": 30_000, "costUSD": 1.5, "costBasis": "list"},
            "claude-haiku-4-5": {"inputTokens": 10, "outputTokens": 300, "cacheReadInputTokens": 50_000, "cacheCreationInputTokens": 2_000, "costUSD": 0.12},
            "claude-next": {"inputTokens": 5, "outputTokens": 6, "costUSD": 0.0, "costBasis": "unknown"},
        }})
}

/// Runs `test` on a daemon whose store holds task-1 running on agent-1.
fn with_a_running_task(test: impl FnOnce(&crate::domain_runtime::DomainRuntime)) {
    let fixture = Fixture::new();
    let mut store = DomainStore::open(&fixture.path()).unwrap();
    seeded(&mut store);
    let state = store.load().unwrap();
    let start = CommandEnvelope {
        protocol_version: PROTOCOL_VERSION, data_epoch: state.data_epoch.clone(), command_id: "start".into(),
        expected_revision: state.revision, command: Command::StartTask { task_id: "task-1".into() },
    };
    store.execute_authenticated(&Actor::Agent { agent_id: "agent-1".into() }, &start, None).unwrap();
    drop(store);
    test(&crate::wiring::open_at(&fixture.0, &fixture.path()).unwrap());
}

#[test]
fn a_nodes_turn_records_what_it_spent_and_its_steps_for_the_task_it_works_on() {
    with_a_running_task(|domain| {
        let agent = AgentId::from("agent-1");
        let read = json!([{"type": "tool_use", "id": "r", "name": "Read", "input": {}}]);
        for message in [
            assistant("m1", None, read.clone()),
            assistant("m1", None, json!([{"type": "tool_use", "id": "b", "name": "Bash", "input": {"command": "sleep 30"}}])),
            assistant("m2", Some("toolu_agent"), read),
            assistant("m3", None, json!([{"type": "text", "text": "Done"}])),
        ] {
            assert_eq!(domain.live().mods.apply_sdk(&agent.0, &message, 10), None);
        }
        let steps = domain.live().mods.apply_sdk(&agent.0, &result(), 11).expect("a result ends the turn");
        node_turn(domain, &agent, &result(), steps, 11);

        let store = domain.store.lock().expect("store");
        let (task, cost, tokens, complete) = store.usage_row("agent-1", "claude-opus-5-5").expect("the session's spend on opus");
        assert_eq!((task.as_deref(), cost, tokens, complete), (Some("task-1"), Some(1_500_000), [120, 4_000, 900_000, 30_000], true));
        assert_eq!(store.steps_row("agent-1"), Some((Some("task-1".into()), StepCounts { steps: 2, subagent_steps: 1, looks: 1, polls: 1, tool_calls: 3 })),
            "m1 waits, its two messages being one call; m2 is the subagent's look; m3 only wrote");
    });
}
