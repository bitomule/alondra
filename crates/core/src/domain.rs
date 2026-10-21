//! Pure business domain. Authenticated adapters construct Actors; SQL owns persistence.
mod attention;
mod checklist;
mod cited_files;
mod command;
mod model;
mod api_failure;
mod claude_access;
mod lifecycle;
mod node_machine;
mod ownership;
mod plan;
mod projection;
mod refusal;
pub mod reads;
mod read_index;
mod rules;
mod schedule;
#[cfg(test)]
mod attention_tests;
#[cfg(test)]
mod card_tests;
#[cfg(test)]
mod checklist_tests;
#[cfg(test)]
mod cited_files_tests;
#[cfg(test)]
mod tests;
#[cfg(test)]
mod ownership_tests;
#[cfg(test)]
mod node_machine_tests;
#[cfg(test)]
mod merged_close_tests;
#[cfg(test)]
mod post_merge_tests;
#[cfg(test)]
mod worktree_close_tests;
#[cfg(test)]
mod proposed_plan_tests;
#[cfg(test)]
mod plan_phases_tests;
#[cfg(test)]
mod suggestion_tests;
#[cfg(test)]
mod plan_review_tests;
#[cfg(test)]
mod lifecycle_tests;
#[cfg(test)]
mod lifecycle_cache_tests;
#[cfg(test)]
pub(crate) mod mcp_wait_tests;
#[cfg(test)]
mod decision_tests;
#[cfg(test)]
mod plan_wait_tests;
#[cfg(test)]
mod turn_end_wait_tests;
pub use attention::*;
pub use checklist::*;
pub use cited_files::*;
pub use command::*;
pub use model::*;
pub use api_failure::*;
pub use claude_access::*;
pub use lifecycle::*;
pub use node_machine::*;
pub use ownership::*;
pub use plan::*;
pub use projection::*;
pub use read_index::*;
pub use refusal::*;
pub use schedule::*;
pub use alondra_foundation::{McpConnect, McpOrigin, McpScope, McpTraits};
pub use rules::{MCP_GO_ON_WITHOUT, MCP_SERVER_PREFIX, MCP_WAIT_PREFIX, McpServerKey, McpWaiting, MAX_MCP_SERVERS, McpNodeReading, McpServerState, McpSignIn, McpStatus, NodeMcp, OpenSignIn, ALONDRA_MCP_SERVER, PLUGIN_SOURCE, UNASKABLE, UNWRITTEN_PLAN, decision_policy, PLAN_REVIEW_KEEP_PLANS, PLAN_REVIEW_PROBLEM_PREFIX, PLAN_REVIEW_REMIND, PlanReviewProblem, ALONDRA_MERGES, ORCHESTRATOR_READER, ENDED_UNANSWERED_PREFIX, OPEN_SUGGESTIONS_MAX, SUGGESTION_ANSWERS, suggestion_options, unanswered_withdrawn_notice, cancelled_notice, delivery_withdrawn_for_questions, delivery_withdrawn_behind_pr, dependencies_done_notice, cleanup_request, post_merge_reminder, merge_failed_text, merge_failure, merged_notice};
