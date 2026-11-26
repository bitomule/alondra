//! The Agent SDK nodes and the launching of tasks: the owners of the SDK hosts (`sdk_owner`), the
//! moves of a task to another provider or account (`provider_transition`, `account_move`) and of
//! `claude --bg` nodes to the SDK (`bg_handover`), the automatic launch, its retries and the
//! scheduled runs (`auto_launch`, `start_retries`, `domain_schedule_runner`, `domain_schedule_api`),
//! and the closing of tasks and the release of resting hosts (`task_closer`, `idle_release`). It
//! asks the rest of the engine through the port `SdkEngine` (`engine`), and the rest of the engine
//! asks it through `SdkHosts` (`wiring`); `alondrad` puts them together. The library names no other
//! piece of the engine. Its tests that need no engine run on the doubles of `engine-doubles`
//! (`model_fixture`); the ones that open the whole engine, on the fixtures of `engine_fixtures`,
//! compile only with the feature `whole-engine`.

#[cfg(test)]
extern crate self as alondra_engine_sdk;

#[cfg(test)]
#[path = "../../test_support.rs"]
pub mod test_support;

#[macro_use]
extern crate alondra_foundation;

#[cfg(target_os = "macos")]
use alondra_daemon_support::schedule_clock;
use alondra_daemon_support::{blocking_work, files_retention, locks, schedules};
use alondra_engine_ports::plugins;
use alondra_daemon_support::watcher;
#[cfg(test)]
#[cfg(feature = "whole-engine")]
use alondra_daemon_engine::{
    accounts, allow_always, cancelled_worktrees, claude_access, domain_agent_lifecycle, domain_github, domain_holds,
    domain_messages, domain_orchestrator_reads, domain_orchestrator_tools, domain_transport, gate, host_fixtures, hosts,
    contexts, launch_plans, live, machine_state, native_probe, node_close, node_exit, node_push, node_reports,
    obligation_watch, options, sdk_handover, sdk_live, start_failures, task_worktrees, untaken, usage_pause, usage_probe,
};
#[cfg(test)]
#[cfg(feature = "whole-engine")]
use alondra_engine_admission::{admission, machine_watch};
pub use alondra_engine_kernel::{domain_runtime, domain_store, sdk_backend};

pub mod account_move;
pub mod auto_launch;
pub mod bg_handover;
pub mod domain_schedule_api;
pub mod domain_schedule_runner;
pub mod engine;
pub mod ended_hosts;
#[cfg(test)]
#[cfg(feature = "whole-engine")]
pub mod engine_assembly;
#[cfg(test)]
#[cfg(feature = "whole-engine")]
pub mod engine_fixtures;
pub mod host_folders;
#[cfg(test)]
#[cfg(feature = "whole-engine")]
mod engine_tests;
pub mod idle_release;
pub mod machine_hooks;
#[cfg(test)]
#[cfg(not(feature = "real-hosts"))]
pub mod model_fixture;
pub mod provider_transition;
pub mod spec_writes;
pub mod sdk_owner;
pub mod start_retries;
pub mod task_closer;
pub mod sdk_api;
pub mod wiring;
