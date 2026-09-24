pub mod activity;
pub mod artifact;
pub mod automerge;
pub mod config;
pub mod event;
pub mod issues;
pub mod keeper;
pub mod model;
pub mod options;
pub mod pr;
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

pub fn now_millis() -> Millis {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}
