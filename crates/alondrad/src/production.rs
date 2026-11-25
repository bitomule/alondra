//! The daemon with its real edges: GitHub through the Mac's `gh`. Only the `alondrad` binary links
//! this crate, so no test of the daemon, and no scenario, links the GitHub client.
use alondrad::assemble::{GithubSeams, Seams};
use std::sync::Arc;

/// The edges the daemon runs with on the user's Mac.
pub trait Production {
    fn production() -> Self;
}

impl Production for GithubSeams {
    /// The real GitHub, through `gh`.
    fn production() -> Self {
        GithubSeams::of(Arc::new(alondra_github_gh::GhCli::new()))
    }
}

impl Production for Seams {
    fn production() -> Self {
        Seams::over(GithubSeams::production())
    }
}

/// Runs the daemon with its real edges.
pub fn main() -> anyhow::Result<()> {
    alondrad::main(Seams::production)
}
