//! The daemon's engine: the domain runtime and its store, the nodes and their Agent SDK hosts,
//! admission, the orchestrator and the transports, everything below the routes and views that
//! `alondrad` serves.
#![cfg_attr(all(feature = "test-hooks", not(test)), allow(dead_code, unused_imports))]

#[cfg(any(test, feature = "test-hooks"))]
#[path = "../../test_support.rs"]
pub mod test_support;

#[macro_use]
extern crate alondra_daemon_support;

use alondra_daemon_support::{
    approval_lease,
    auth,
    blocking_work,
    codex_credentials,
    disk,
    disk_peaks,
    domain_authority,
    domain_content,
    domain_cutover,
    hook_socket,
    hook_text,
    job_state,
    launch_worktree,
    local_network,
    locks,
    log,
    node_cost,
    plugins,
    process_watch,
    prs,
    runtime_mutex,
    save,
    schedules,
    service_files,
    uploads,
    watcher,
    worktree,
};
#[cfg(target_os = "macos")]
use alondra_daemon_support::schedule_clock;

pub mod account_move;
pub mod accounts;
pub mod admission;
pub mod allow_always;
pub mod api_failures;
pub mod auto_launch;
pub mod claude_access;
pub mod claude_trust;
pub mod cleanup;
pub mod codex_live;
pub mod database_check;
pub mod domain_agent_lifecycle;
pub mod domain_database_migration;
pub mod domain_github;
pub mod open_pr_sweep;
pub mod domain_holds;
pub mod domain_merges;
pub mod domain_messages;
pub mod domain_orchestrator;
pub mod domain_orchestrator_reads;
pub mod domain_orchestrator_tools;
pub mod domain_runtime;
pub mod domain_schedule_api;
pub mod domain_schedule_runner;
pub mod domain_sessions;
pub mod domain_store;
pub mod domain_transport;
pub mod home_renames;
pub mod idle_compaction;
pub mod obligation_watch;
pub use alondra_daemon_engine_model::issues;
pub mod launch_plans;
pub mod live;
pub mod local_actions;
pub mod local_network_card;
pub mod machine;
pub mod machine_state;
pub mod machine_watch;
#[cfg(test)]
mod machine_model;
pub mod mcp_probe;
pub mod mcp_remove;
pub mod mcp_sign_in;
pub mod mcp_waits;
pub mod metrics_capture;
pub mod native_probe;
pub mod node_close;
pub mod node_push;
pub mod node_reconcile;
pub mod node_reports;
pub mod node_revival;
pub mod node_work;
pub use alondra_daemon_engine_model::options;
pub mod orchestrator_codex;
pub mod orchestrator_inbox;
pub mod orchestrator_process;
pub mod orchestrator_provider;
pub mod orchestrator_sdk;
pub mod pauses;
pub mod plan_reviews;
pub mod pr_images;
pub mod pr_viewed;
pub mod provider_transition;
pub use alondra_daemon_engine_model::reply;
pub mod retention;
pub mod sdk_handover;
pub mod sdk_live;
pub mod sdk_owner;
pub use alondra_daemon_engine_model::settings_file;
pub mod stall_context;
pub mod start_failures;
pub mod swap_disk;
pub mod cancelled_worktrees;
pub mod task_closer;
pub mod task_context;
pub mod task_worktrees;
pub mod usage_pause;
pub mod usage_probe;
