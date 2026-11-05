#[cfg(test)]
#[path = "../../test_support.rs"]
mod test_support;

pub mod accounts;
pub mod codex_account;
pub mod codex_login;
pub mod bonjour;
pub mod home_lock;
pub mod service_config;
pub mod delivery;
pub mod activity;
pub mod operation_id;
pub mod allow_always;
pub mod allow_for_task;
pub mod cited_file;
pub mod artifact;
pub mod automerge;
pub mod config;
pub mod daemon_log;
pub mod event;
pub mod issues;
pub mod launcher;
pub mod local_network_card;
pub mod close_problem;
pub mod open_pr_delivery;
pub mod discard_worktrees;
pub mod machine_incident;
pub mod markdown;
pub mod model;
pub mod model_label;
pub mod model_decider;
pub mod model_selection;
pub mod model_sheets;
pub mod model_table;
pub mod selection_transition;
pub mod claude_account;
pub mod claude_code;
pub mod node_guard;
pub mod options;
pub mod projects_folder;
pub mod orchestrator;
pub mod orchestrator_batch;
pub mod orchestrator_cards;
pub mod pr;
pub mod request_outcome;
pub mod requested_form;
pub mod secret_destination;
pub mod sessions;
pub mod stack;
pub mod start_problem;
pub mod startup;
#[cfg(test)]
mod recorded;
pub mod states;
pub mod tasks;
pub mod task_tags;
pub mod metrics;
pub mod update;
pub mod upkeep;
pub mod usage;
pub mod view;
pub mod views;

pub use event::{DaemonObservation, Event, HookEvent, ReviewVerdict};
pub use model::*;
pub use view::View;

pub const DEFAULT_PORT: u16 = 47800;

/// How long the launch of a node can take, its Agent SDK host's start included.
pub const LAUNCH_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(60);

/// Set in every session Alondra launches, so its hooks and its tools say it is Alondra's.
pub const LAUNCHED_ENV: &str = "ALONDRA_LAUNCHED";

/// The header the plugin's hooks carry `ALONDRA_LAUNCHED` in.
pub const LAUNCHED_HEADER: &str = "x-alondra-launched";

/// Alondra's plugins as a global install enables them in every session.
pub const GLOBAL_PLUGINS: [&str; 2] = ["alondra@alondra", "alondra-orchestrator@alondra"];

/// The `--settings` of every session Alondra launches: the mark, and any global install of
/// Alondra's plugins off, since the session loads them with `--plugin-dir`.
pub fn launch_settings() -> String {
    let off: serde_json::Map<String, serde_json::Value> = GLOBAL_PLUGINS.iter().map(|id| (id.to_string(), false.into())).collect();
    serde_json::json!({"enabledPlugins": off, "env": {LAUNCHED_ENV: "1"}}).to_string()
}

pub fn now_millis() -> Millis {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

pub mod domain;
