//! The engine's tests that need the gate itself, over the engine's test-hooks fixtures: every runtime
//! they open has the gate. The ones that also start nodes through the SDK run with `engine-sdk`.
pub use crate::AdmissionOf as _;
pub use alondra_daemon_engine::gate::GateOf as _;

mod transport_tests;

/// Gives every runtime the engine's fixtures open the gate.
pub fn wired() {
    alondra_daemon_engine::wiring::TEST_OPEN.get_or_init(|| |home, database| {
        let domain = alondra_daemon_engine::wiring::open_engine_at(home, database)?;
        crate::wire(domain.ports());
        Ok(domain)
    });
}
