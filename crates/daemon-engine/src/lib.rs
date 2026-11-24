//! The daemon's engine: the domain runtime and its store, the nodes and their Agent SDK hosts and
//! the transports, everything below the admission gate and the routes and views that `alondrad`
//! serves. It asks the gate through the port of engine-ports (`gate`).

#[path = "../../test_support.rs"]
pub mod test_support;

#[macro_use]
extern crate alondra_foundation;
#[cfg(test)]
extern crate self as alondra_daemon_engine;

use alondra_daemon_support::{
    approval_lease,
    auth,
    blocking_work,
    domain_authority,
    domain_content,
    files_retention,
    domain_cutover,
    hook_text,
    job_state,
    local_network,
    locks,
    log,
    node_cost,
    runtime_mutex,
    save,
    schedules,
    service_files,
    uploads,
    watcher,
};
#[cfg(target_os = "macos")]
use alondra_daemon_support::schedule_clock;

pub use alondra_engine_kernel::TEST_JOURNAL;
pub use alondra_engine_kernel::{
    domain_runtime,
    domain_sessions,
    domain_store,
    extension,
    metrics_capture,
    retention,
    sdk_backend,
    settings_projection,
};

pub use alondra_engine_github::{domain_github, domain_merges, domain_verdicts, open_pr_sweep, pr_identity, pr_images};
pub use alondra_engine_live::{codex_live, contexts, node_reports, node_work, sdk_live};
pub use alondra_engine_mcp::{mcp_probe, mcp_remove, mcp_sign_in, mcp_waits};
pub use alondra_engine_worktrees::{cancelled_worktrees, home_renames, task_worktrees};
pub use alondra_engine_github::github_world;

pub mod accounts;
pub mod antigravity_accounts;
pub mod allow_always;
pub mod api_failures;
pub mod claude_access;
pub use alondra_daemon_engine_model::claude_trust;
pub mod cleanup;
pub mod domain_agent_lifecycle;
pub mod domain_holds;
pub mod domain_messages;
pub mod domain_orchestrator_reads;
pub mod domain_orchestrator_tools;
pub mod domain_transport;
pub mod gate;
pub mod host_fixtures;
pub mod hosts;
pub mod obligation_watch;
pub use alondra_daemon_engine_model::issues;
pub mod launch_plans;
pub mod live;
pub mod local_actions;
pub mod local_network_card;
pub mod machine;
pub use alondra_daemon_engine_machine::machine_state;
pub mod machine_hooks;
pub use alondra_daemon_engine_machine::native_probe;
pub mod node_close;
pub mod node_exit;
pub mod node_push;
pub use alondra_daemon_engine_model::options;
pub mod plan_reviews;
pub mod report_held_card;
pub use alondra_daemon_engine_model::reply;
pub mod sdk_handover;
pub use alondra_daemon_engine_model::settings_file;
pub mod start_failures;
pub use alondra_daemon_engine_machine::swap_disk;
pub mod task_context;
pub use alondra_engine_kernel::untaken;
pub mod usage_pause;
pub mod usage_probe;
pub mod wiring;
