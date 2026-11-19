//! GitHub through the Mac's `gh`: the adapter that implements the engine's `GitHub` port. Nothing
//! of the engine depends on it; the root registers it, and a scenario registers a GitHub of its own.
#[cfg(test)]
#[path = "../../test_support.rs"]
mod test_support;

pub mod cli;
mod diff;
#[cfg(test)]
mod diff_tests;
mod images;
mod merges;
pub mod pr_viewed;
pub mod prs;

pub use cli::GhCli;

use alondra_engine_ports::Registry;
use alondra_engine_ports::github::GitHub;
use std::sync::Arc;

/// The whole of GitHub, through `gh`.
pub fn github() -> GitHub {
    GitHub::of(Arc::new(GhCli::new()))
}

/// Gives the registry GitHub through `gh`.
pub fn wire(registry: &Registry) {
    registry.provide(Arc::new(github()));
}
