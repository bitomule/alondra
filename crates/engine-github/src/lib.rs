//! The engine's GitHub: reading pull requests with `gh`, merging them, the images and viewed
//! files of their descriptions, and the sweep that adopts open PRs left without a live owner. It
//! sits on `engine-kernel` and names no other engine subsystem.
#![cfg_attr(all(feature = "test-hooks", not(test)), allow(dead_code, unused_imports))]

#[cfg(any(test, feature = "test-hooks"))]
#[path = "../../test_support.rs"]
pub mod test_support;

#[macro_use]
extern crate alondra_foundation;

use alondra_daemon_support::auth;

use alondra_daemon_engine_model::options;
#[cfg(any(test, feature = "test-hooks"))]
use alondra_engine_kernel::domain_database_migration;
use alondra_engine_kernel::{domain_runtime, domain_store};

/// A runtime that tells no other subsystem of its commits: this crate's tests need none of them.
#[cfg(any(test, feature = "test-hooks"))]
fn unwired(home: &std::path::Path) -> anyhow::Result<domain_runtime::DomainRuntime> {
    domain_runtime::DomainRuntime::open_unwired_at(home, &domain_database_migration::active_path(home))
}

pub mod domain_github;
pub mod domain_merges;
#[cfg(any(test, feature = "test-hooks"))]
pub mod github_world;
pub mod open_pr_sweep;
pub mod pr_images;
