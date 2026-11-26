//! What this crate gives the ports the rest of the engine asks it through. `alondrad` puts the
//! whole engine together.
use alondra_engine_ports::Registry;
use alondra_engine_ports::hosts::SdkHosts;
use std::sync::Arc;

/// Gives the registry the owner of the SDK hosts the rest of the engine asks, and the schedules and
/// changes of provider the server asks.
pub fn wire(registry: &Registry) {
    registry.provide::<dyn SdkHosts>(Arc::new(crate::sdk_owner::Hosts));
    registry.provide::<dyn alondra_engine_ports::domain_api::SdkApi>(Arc::new(crate::sdk_api::Api));
}
