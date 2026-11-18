//! The test doubles of the engine's ports. A test opens a runtime, registers in it the doubles of
//! the pieces it does not test, and runs the piece it tests against them, at run time: no piece is
//! compiled a second time for its tests.
pub mod admission;
pub mod host_root;
pub mod hosts;
pub mod orchestrator;

#[path = "../../test_support.rs"]
mod test_support;
