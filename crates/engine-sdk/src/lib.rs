//! The Agent SDK nodes and the launching of tasks: the owners of the SDK hosts (`sdk_owner`), the
//! moves of a task to another provider or account (`provider_transition`, `account_move`) and of
//! `claude --bg` nodes to the SDK (`bg_handover`), the automatic launch, its retries and the
//! scheduled runs (`auto_launch`, `start_retries`, `domain_schedule_runner`, `domain_schedule_api`),
//! the closing of tasks and the release of resting hosts (`task_closer`, `idle_release`), and
//! where the whole engine is put together (`wiring`). It sits on top of `daemon-engine` and
//! `engine-admission`, which ask it through the port `SdkHosts`; only `alondrad` sees it.
#![cfg_attr(all(feature = "test-hooks", not(test)), allow(dead_code, unused_imports))]

#[cfg(any(test, feature = "test-hooks"))]
#[path = "../../test_support.rs"]
pub mod test_support;

#[macro_use]
extern crate alondra_daemon_support;

use alondra_daemon_engine::{
    accounts, allow_always, api_failures, cancelled_worktrees, claude_trust, codex_live, domain_agent_lifecycle,
    domain_github, domain_holds, domain_messages, domain_orchestrator_tools, domain_transport, gate, hosts,
    idle_compaction, launch_plans, live, node_close, node_exit, node_push, options, sdk_handover, sdk_live,
    start_failures, task_worktrees, usage_pause,
};
#[cfg(target_os = "macos")]
use alondra_daemon_support::schedule_clock;
use alondra_daemon_support::{auth, blocking_work, disk, files_retention, launch_worktree, locks, plugins, schedules, worktree};
#[cfg(any(test, feature = "test-hooks"))]
use alondra_daemon_engine::{claude_access, domain_orchestrator_reads, host_fixtures, machine_state, native_probe, node_reports, obligation_watch, untaken, usage_probe};
#[cfg(any(test, feature = "test-hooks"))]
use alondra_daemon_support::watcher;
#[cfg(any(test, feature = "test-hooks"))]
use alondra_engine_admission::{admission, machine_watch};
pub use alondra_engine_kernel::{domain_database_migration, domain_runtime, domain_store, ports, sdk_backend};

pub mod account_move;
pub mod auto_launch;
pub mod bg_handover;
pub mod domain_schedule_api;
pub mod domain_schedule_runner;
pub mod ended_hosts;
#[cfg(any(test, feature = "test-hooks"))]
pub mod engine_tests;
pub mod idle_release;
#[cfg(any(test, feature = "test-hooks"))]
pub mod machine_hooks;
pub mod provider_transition;
pub mod sdk_owner;
pub mod start_retries;
pub mod task_closer;
pub mod wiring;
