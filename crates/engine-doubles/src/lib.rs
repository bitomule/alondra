//! The test doubles of the engine's ports. A test opens a runtime, registers in it the doubles of
//! the pieces it does not test, and runs the piece it tests against them, at run time: no piece is
//! compiled a second time for its tests.
pub mod accounts;
pub mod admission;
pub mod agent_host;
pub mod contract;
pub mod disk;
pub mod gate;
pub mod github;
pub mod host_root;
pub mod hosts;
pub mod machine;
pub mod orchestrator;
pub mod plugins;
pub mod processes;
pub mod runtime;
pub mod sdk;
pub mod session_worktrees;
pub mod sign_in_returns;
pub mod worktrees;

#[path = "../../test_support.rs"]
mod test_support;
