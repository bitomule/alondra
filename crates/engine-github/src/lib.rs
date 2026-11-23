//! The engine's GitHub: reading pull requests with `gh`, merging them, the images and viewed
//! files of their descriptions, and the sweep that adopts open PRs left without a live owner. It
//! sits on `engine-kernel` and names no other engine subsystem.
#[cfg(test)]
#[path = "../../test_support.rs"]
mod test_support;

#[macro_use]
extern crate alondra_foundation;

use alondra_daemon_support::auth;

use alondra_daemon_engine_model::options;
use alondra_engine_kernel::{domain_runtime, domain_store};

/// A runtime that tells no other subsystem of its commits: this crate's tests need none of them.
#[cfg(test)]
fn unwired(home: &std::path::Path) -> anyhow::Result<domain_runtime::DomainRuntime> {
    #[cfg(test)]
    alondra_engine_kernel::TEST_JOURNAL.get_or_init(|| &alondra_store_sqlite::Sqlite);
    let runtime = domain_runtime::DomainRuntime::open_unwired_at(home, &alondra_engine_ports::journal::active_path(home))?;
    alondra_engine_doubles::machine::wire(runtime.ports(), alondra_daemon_support::watcher::claude_home);
    Ok(runtime)
}

pub mod domain_github;
pub mod domain_merges;
pub mod github_world;
pub mod open_pr_sweep;
pub mod pr_identity;
pub mod pr_images;
