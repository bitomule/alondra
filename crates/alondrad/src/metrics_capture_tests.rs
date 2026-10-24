use super::*;
use crate::domain_store::metrics_fold::tests::{with_a_merged_pr, with_a_running_task};
use serde_json::json;
use std::sync::atomic::{AtomicUsize, Ordering};

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

#[test]
fn a_result_says_what_each_model_spent_and_a_model_without_a_price_has_no_cost() {
    let mut usage = claude_usage(&result());
    usage.sort_by(|a, b| a.model.cmp(&b.model));
    assert_eq!(usage, [
        Usage { model: "claude-haiku-4-5".into(), cost_micros: Some(120_000), input: 10, output: 300, cache_read: 50_000, cache_write: 2_000 },
        Usage { model: "claude-next".into(), cost_micros: None, input: 5, output: 6, cache_read: 0, cache_write: 0 },
        Usage { model: "claude-opus-5-5".into(), cost_micros: Some(1_500_000), input: 120, output: 4_000, cache_read: 900_000, cache_write: 30_000 },
    ]);
    assert!(claude_usage(&json!({"type": "result", "total_cost_usd": 3.0})).is_empty(), "no per-model breakdown, nothing to split it by");
}

#[test]
fn claudes_tools_read_as_looks_shell_commands_waits_or_other_work() {
    let message = assistant("m", None, json!([
        {"type": "text", "text": "Looking"},
        {"type": "tool_use", "id": "1", "name": "Grep", "input": {}},
        {"type": "tool_use", "id": "2", "name": "Bash", "input": {"command": "git status"}},
        {"type": "tool_use", "id": "3", "name": "BashOutput", "input": {}},
        {"type": "tool_use", "id": "4", "name": "mcp__alondra__ask", "input": {}},
    ]));
    assert_eq!(claude_tools(&message), [Tool::Read, Tool::Shell("git status".into()), Tool::Wait, Tool::Other]);
    assert_eq!(claude_step(&message), Some("m"));
    assert_eq!(claude_step(&json!({"type": "assistant", "uuid": "u", "message": {}})), Some("u"));
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
            assert_eq!(domain.live.mods.apply_sdk(&agent.0, &message, 10), None);
        }
        let steps = domain.live.mods.apply_sdk(&agent.0, &result(), 11).expect("a result ends the turn");
        node_turn(domain, &agent, &result(), steps, 11);

        let store = domain.store.lock().expect("store");
        let (task, cost, tokens, complete) = store.usage_row("agent-1", "claude-opus-5-5").expect("the session's spend on opus");
        assert_eq!((task.as_deref(), cost, tokens, complete), (Some("task-1"), Some(1_500_000), [120, 4_000, 900_000, 30_000], true));
        assert_eq!(store.steps_row("agent-1"), Some((Some("task-1".into()), StepCounts { steps: 2, subagent_steps: 1, looks: 1, polls: 1, tool_calls: 3 })),
            "m1 waits, its two messages being one call; m2 is the subagent's look; m3 only wrote");
    });
}

struct Sizes {
    answers: Vec<Option<Value>>,
    asked: AtomicUsize,
}

impl crate::prs::Gh for Sizes {
    async fn view(&self, url: &str) -> Option<Value> {
        assert_eq!(url, "https://github.com/owner/repository/pull/7");
        let asked = self.asked.fetch_add(1, Ordering::SeqCst);
        self.answers.get(asked).cloned().flatten()
    }
}

#[test]
fn a_merged_prs_size_is_asked_until_github_answers_and_then_never_again() {
    with_a_merged_pr(|domain| {
        let gh = Sizes { answers: vec![None, Some(json!({"additions": 120, "deletions": 30}))], asked: AtomicUsize::new(0) };
        let pass = || tokio::runtime::Builder::new_current_thread().build().unwrap().block_on(pr_sizes(domain, &gh, alondra_core::now_millis()));
        pass();
        assert_eq!(domain.store.lock().expect("store").pr_lines("task-1"), None, "GitHub did not answer");
        let week_later = alondra_core::now_millis() + crate::domain_store::metrics_fold::SAMPLES_MS;
        assert!(domain.prs_without_size(week_later, 32).unwrap().is_empty(), "a week after the task ended, its size is no longer asked");
        pass();
        assert_eq!(domain.store.lock().expect("store").pr_lines("task-1"), Some(150));
        pass();
        assert_eq!(gh.asked.load(Ordering::SeqCst), 2, "a known size is not asked again");
    });
}
