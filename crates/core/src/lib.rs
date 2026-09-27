pub mod home_lock;
pub mod delivery;
pub mod activity;
pub mod artifact;
pub mod automerge;
pub mod config;
pub mod event;
pub mod issues;
pub mod keeper;
pub mod model;
pub mod options;
pub mod orchestrator;
pub mod pr;
pub mod sessions;
pub mod stack;
#[cfg(test)]
mod recorded;
pub mod states;
pub mod tasks;
pub mod upkeep;
pub mod usage;
pub mod view;

pub use event::{DaemonObservation, Event, HookEvent, ReviewVerdict};
pub use model::*;
pub use view::View;

pub const DEFAULT_PORT: u16 = 47800;

/// How long alondrad waits for `claude --bg` to launch a node.
pub const LAUNCH_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(60);

/// Set in every session Alondra launches, so its hooks and its tools say it is Alondra's.
pub const LAUNCHED_ENV: &str = "ALONDRA_LAUNCHED";

/// The header the plugin's hooks carry `ALONDRA_LAUNCHED` in.
pub const LAUNCHED_HEADER: &str = "x-alondra-launched";

/// Alondra's plugins as a global install enables them in every session.
pub const GLOBAL_PLUGINS: [&str; 2] = ["alondra@alondra", "alondra-orchestrator@alondra"];

/// The `--settings` of every session Alondra launches: the mark, any global install of Alondra's
/// plugins off, since the session loads them with `--plugin-dir`, and `alondra statusline`, which
/// tells Alondra how full the session's context is and then shows the user's own status line.
pub fn launch_settings(alondra: Option<&std::path::Path>) -> String {
    let off: serde_json::Map<String, serde_json::Value> = GLOBAL_PLUGINS.iter().map(|id| (id.to_string(), false.into())).collect();
    let mut settings = serde_json::json!({"enabledPlugins": off, "env": {LAUNCHED_ENV: "1"}});
    if let Some(bin) = alondra {
        let quoted = format!("'{}'", bin.display().to_string().replace('\'', r"'\''"));
        settings["statusLine"] = serde_json::json!({"type": "command", "command": format!("{quoted} statusline")});
    }
    settings.to_string()
}

pub fn now_millis() -> Millis {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

pub mod domain_v2;
