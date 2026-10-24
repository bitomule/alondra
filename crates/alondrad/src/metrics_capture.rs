//! What Claude says it spent and did, read into the metrics' own terms, and recorded as each turn
//! ends. A `result` carries `modelUsage`: per model, the running totals of the whole call,
//! subagents included, and of the session's earlier calls when it resumed one. Each step is one
//! assistant message id (`message.id`), which parallel tool calls share.
use crate::domain_runtime::DomainRuntime;
use crate::domain_store::metrics_fold::Spent;
use alondra_core::Millis;
use alondra_core::domain::AgentId;
use alondra_core::metrics::steps::{StepCounts, Tool};
use alondra_core::metrics::{Role, Usage, micros};
use serde_json::Value;

pub(crate) const CLAUDE: &str = "claude";
const PR_SIZES_PER_PASS: usize = 32;

/// What a `result` says each model spent so far. A model Claude had no price for (`costBasis`
/// `unknown`) has its tokens and no cost.
pub(crate) fn claude_usage(result: &Value) -> Vec<Usage> {
    let Some(models) = result["modelUsage"].as_object() else { return vec![] };
    let count = |entry: &Value, field: &str| entry[field].as_i64().filter(|count| *count >= 0).unwrap_or(0);
    models.iter().filter(|(model, entry)| !model.is_empty() && entry.is_object()).map(|(model, entry)| Usage {
        model: model.clone(),
        cost_micros: entry["costUSD"].as_f64().filter(|_| entry["costBasis"] != "unknown").and_then(micros),
        input: count(entry, "inputTokens"),
        output: count(entry, "outputTokens"),
        cache_read: count(entry, "cacheReadInputTokens"),
        cache_write: count(entry, "cacheCreationInputTokens"),
    }).collect()
}

/// The tool calls of an assistant message.
pub(crate) fn claude_tools(message: &Value) -> Vec<Tool> {
    let blocks = message["message"]["content"].as_array().map_or(&[][..], Vec::as_slice);
    blocks.iter().filter(|block| block["type"] == "tool_use").map(|block| match block["name"].as_str().unwrap_or_default() {
        "Read" | "Grep" | "Glob" | "LS" => Tool::Read,
        "Bash" => Tool::Shell(block["input"]["command"].as_str().unwrap_or_default().to_owned()),
        "BashOutput" | "TaskOutput" => Tool::Wait,
        _ => Tool::Other,
    }).collect()
}

/// The model call an assistant message belongs to.
pub(crate) fn claude_step(message: &Value) -> Option<&str> {
    message["message"]["id"].as_str().or_else(|| message["uuid"].as_str()).filter(|id| !id.is_empty())
}

/// A node's turn ended with `result`: what it spent, for the task the node works on.
pub(crate) fn node_turn(domain: &DomainRuntime, agent: &AgentId, result: &Value, steps: StepCounts, now: Millis) {
    let usage = claude_usage(result);
    let snapshot = domain.read();
    let task_id = snapshot.index().task_of_node(agent).map(|task| task.0.as_str());
    let spent = Spent { session_id: &agent.0, task_id, role: Role::Node, provider: CLAUDE, usage: &usage, steps, at: now };
    if let Err(error) = domain.record_spend(&spent) {
        log!("could not record what session {} spent: {error:#}", agent.0);
    }
}

/// An orchestrator batch ended: what its session spent so far.
pub(crate) fn orchestrator_batch(domain: &DomainRuntime, session: &str, usage: &[Usage], now: Millis) {
    let spent = Spent { session_id: session, task_id: None, role: Role::Orchestrator, provider: CLAUDE, usage, steps: StepCounts::default(), at: now };
    if let Err(error) = domain.record_spend(&spent) {
        log!("could not record what the orchestrator's session {session} spent: {error:#}");
    }
}

/// Asks GitHub once per pass for the size of each merged PR folded without it.
pub(crate) async fn pr_sizes(domain: &DomainRuntime, gh: &impl crate::prs::Gh, now: Millis) {
    let prs = match domain.prs_without_size(now, PR_SIZES_PER_PASS) {
        Ok(prs) => prs,
        Err(error) => {
            log!("could not read the merged PRs without a size: {error:#}");
            return;
        }
    };
    for (task, url) in prs {
        let Some(view) = gh.view(&url).await else { continue };
        let (Some(additions), Some(deletions)) = (view["additions"].as_i64(), view["deletions"].as_i64()) else { continue };
        if let Err(error) = domain.record_pr_lines(&task, additions + deletions) {
            log!("could not record the size of {url}: {error:#}");
        }
    }
}

#[cfg(test)]
#[path = "metrics_capture_tests.rs"]
mod tests;
