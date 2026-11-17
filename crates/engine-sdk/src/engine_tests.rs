//! The tests of the engine's and the gate's modules that start nodes through the SDK, so they run
//! with this crate: each file is named after the module it tests.
mod accounts_tests;
pub mod orchestrator_tools_tests;
mod owner_tests;
mod pauses_tests;
mod system_command_tests;

/// Opens every runtime the engine's fixtures open as the daemon does, with this crate's observers
/// and ports.
pub fn wired() {
    alondra_daemon_engine::wiring::TEST_OPEN.get_or_init(|| crate::wiring::open_at);
}
