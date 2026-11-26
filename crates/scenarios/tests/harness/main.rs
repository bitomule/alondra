//! The tests of the scenario runner, built against the `scenarios` library: in the library's own
//! crate they compiled every one of its sources a second time on every edit below it
//! (docs/architecture/build-graph.md).

#[path = "../../../test_support.rs"]
mod test_support;

mod checkpoint;
mod claude_host;
mod claude_peer;
mod format;
mod from_report;
mod harness;
mod http;
mod runner;
