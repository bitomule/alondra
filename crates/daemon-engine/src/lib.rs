//! The daemon's engine: the domain runtime and its store, the nodes and their Agent SDK hosts and
//! the transports, everything below the admission gate and the routes and views that `alondrad`
//! serves. It asks the gate through the kernel's port (`gate`).
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
    files_retention,
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

pub use alondra_engine_kernel::{
    domain_database_migration,
    domain_runtime,
    domain_sessions,
    domain_store,
    extension,
    metrics_capture,
    ports,
    retention,
    sdk_backend,
    settings_projection,
};

pub use alondra_engine_github::{domain_github, domain_merges, open_pr_sweep, pr_images, pr_viewed};
pub use alondra_engine_live::{codex_live, contexts, node_reports, node_work, sdk_live};
pub use alondra_engine_mcp::{mcp_probe, mcp_remove, mcp_sign_in, mcp_waits};
pub use alondra_engine_worktrees::{cancelled_worktrees, home_renames, task_worktrees};
#[cfg(any(test, feature = "test-hooks"))]
pub use alondra_engine_github::github_world;

pub mod accounts;
pub mod allow_always;
pub mod api_failures;
pub mod claude_access;
pub mod claude_trust;
pub mod cleanup;
pub mod database_check;
pub mod domain_agent_lifecycle;
pub mod domain_holds;
pub mod domain_messages;
pub mod domain_orchestrator_reads;
pub mod domain_orchestrator_tools;
pub mod domain_transport;
pub mod gate;
#[cfg(any(test, feature = "test-hooks"))]
pub mod host_fixtures;
pub mod hosts;
pub mod idle_compaction;
pub mod obligation_watch;
pub use alondra_daemon_engine_model::issues;
pub mod launch_plans;
pub mod live;
pub mod local_actions;
pub mod local_network_card;
pub mod machine;
pub use alondra_daemon_engine_machine::machine_state;
#[cfg(feature = "test-hooks")]
pub mod machine_hooks;
pub use alondra_daemon_engine_machine::native_probe;
pub mod node_close;
pub mod node_exit;
pub mod node_push;
pub use alondra_daemon_engine_model::options;
pub mod plan_reviews;
pub use alondra_daemon_engine_model::reply;
pub mod sdk_handover;
pub use alondra_daemon_engine_model::settings_file;
pub mod start_failures;
pub use alondra_daemon_engine_machine::swap_disk;
pub mod task_context;
pub mod untaken;
pub mod usage_pause;
pub mod usage_probe;
pub mod wiring;
