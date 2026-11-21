//! What this crate gives the ports the rest of the engine asks it through. `alondrad` puts the
//! whole engine together.
use alondra_engine_ports::Registry;
use alondra_engine_ports::hosts::SdkHosts;
use std::sync::Arc;

/// Gives the registry the owner of the SDK hosts the rest of the engine asks.
pub fn wire(registry: &Registry) {
    registry.provide::<dyn SdkHosts>(Arc::new(crate::sdk_owner::Hosts));
}
