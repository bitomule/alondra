pub mod artifact;
pub mod config;
pub mod event;
pub mod keeper;
pub mod model;
pub mod pr;
pub mod stack;
#[cfg(test)]
mod recorded;
pub mod states;
pub mod usage;
pub mod view;

pub use event::{DaemonObservation, Event, HookEvent};
pub use model::*;
pub use view::View;

pub const DEFAULT_PORT: u16 = 47800;

pub fn now_millis() -> Millis {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}
