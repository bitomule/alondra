//! The daemon's input adapter: the HTTP routes, the iroh and direct transports, the pushes and the
//! screens' cache the clients read. `alondrad` assembles it with the engine and the screens.
// The engine's fixtures take its `Directory`, so alondrad's tests share that one.
#[cfg(test)]
pub use alondra_daemon_engine::test_support;

#[macro_use]
extern crate alondra_foundation;

pub use alondra_engine_ports::plugins;
pub use alondra_foundation::{blocking_work, exit, locks, log, principal};
pub use alondra_engine_ports::{
    approval_lease,
    domain_cutover,
    files_retention,
    job_state,
    local_network,
    reports,
    stored_files,
    uploads,
    watcher,
};
pub mod auth;
pub mod card_language;
pub mod classify;
pub mod device_usage;
pub mod legacy_home;
pub mod open_files;
pub mod pr_heads;
pub mod pr_ids;
pub mod push;
pub mod relaunch;
pub mod result_files;
pub mod revoked_devices;
pub mod scoped_reads;
pub mod service_files;
pub mod start_guard;
pub mod updates;
pub mod workflow_state;
#[cfg(test)]
pub use alondra_engine_ports::disk;
#[cfg(test)]
pub use alondra_store_sqlite::domain_database_migration;
pub use alondra_daemon_engine::{
    accounts,
    antigravity_accounts,
    allow_always,
    claude_access,
    cleanup,
    domain_github,
    domain_holds,
    domain_merges,
    domain_messages,
    domain_orchestrator_reads,
    domain_orchestrator_tools,
    domain_runtime,
    domain_store,
    domain_transport,
    home_renames,
    issues,
    launch_plans,
    local_network_card,
    machine,
    mcp_probe,
    mcp_remove,
    mcp_waits,
    node_close,
    node_push,
    node_reports,
    node_work,
    obligation_watch,
    options,
    pr_identity,
    pr_images,
    retention,
    runtime_api,
    sdk_handover,
    sdk_live,
    task_context,
    task_worktrees,
    usage_pause,
    usage_probe,
};
#[cfg(test)]
pub use alondra_engine_sdk::{account_move, auto_launch, bg_handover, domain_schedule_api, domain_schedule_runner, idle_release, provider_transition, sdk_owner, task_closer};
#[cfg(test)]
pub use alondra_engine_admission::{admission, machine_watch, pauses};
#[cfg(test)]
pub use alondra_engine_orchestrator::domain_orchestrator;
#[cfg(test)]
pub use alondra_daemon_engine::{domain_agent_lifecycle, machine_state};

pub mod operation_journal;
pub mod direct;
pub mod claude_check;
pub mod domain_api;
pub mod iroh_terminals;
pub mod domain_bootstrap;
pub mod delivery_review;
pub mod domain_delivery;
pub mod iroh_endpoint;
pub mod reachability;
pub mod device_expiry;
pub mod notices;
pub mod public_content;
pub mod push_gate;
pub mod push_transport;
pub mod activity_push;
pub mod push_alerts;
pub mod push_retract;
pub mod service;
pub use service::scenario_phone;
pub mod settings;
pub(crate) mod spec_checks;
pub mod client_routes;
pub mod reply;
pub mod views;
#[cfg(test)]
mod engine_tests;
/// The SDK's launches on the doubled hosts the host models play, which the engine's fixtures name
/// `crate::model_fixture` as in the SDK's crate.
#[cfg(test)]
#[cfg(not(feature = "real-hosts"))]
#[path = "../../engine-sdk/src/model_fixture/launch_fixture.rs"]
mod model_fixture;

pub mod screens;

use std::sync::Arc;

/// What approving a direct pairing request does on this Mac: pin the device's keys in
/// `service.toml`, approve it in the registry, and make the running service accept it. A device
/// it could never approve (revoked here, or an id pinned with other keys) is refused before the
/// request reaches the user.
pub fn direct_approval(
    home: std::path::PathBuf,
    auth: Arc<auth::Auth>,
    service: Arc<service::Service>,
) -> direct::DeviceApproval {
    use alondra_core::service_config::{NewDevice, ServiceConfigLock, add_device, pins_other_keys};
    use auth::ServiceApprovalError;
    use direct::pairing::PairingError;
    fn keys(device: &direct::pairing::ApprovedDevice) -> NewDevice<'_> {
        NewDevice { device_id: &device.device_id, signing_key: &device.signing_key, agreement_key: &device.agreement_key }
    }
    let admit = {
        let (home, auth) = (home.clone(), auth.clone());
        Box::new(move |device: &direct::pairing::ApprovedDevice| {
            match auth.service_admits(&device.device_id, &device.signing_key) {
                Err(ServiceApprovalError::Revoked) => return Err(PairingError::Denied),
                Err(ServiceApprovalError::OtherKey) => return Err(PairingError::Conflict),
                Err(ServiceApprovalError::Refused(reason)) => return Err(PairingError::Internal(reason)),
                Ok(()) => {}
            }
            match pins_other_keys(&home, &keys(device)) {
                Ok(false) => Ok(()),
                Ok(true) => Err(PairingError::Conflict),
                Err(error) => Err(PairingError::Internal(error.to_string())),
            }
        })
    };
    let persist = Box::new(move |device: &direct::pairing::ApprovedDevice| {
        let lock = ServiceConfigLock::exclusive(&home).map_err(|error| error.to_string())?;
        add_device(&home, &lock, &keys(device)).map_err(|error| error.to_string())?;
        direct::remember_direct(&home, &device.device_id).map_err(|error| error.to_string())?;
        drop(lock);
        auth.approve_service(&device.device_id, &device.name, &device.signing_key)?;
        service.reload_devices().map_err(|error| format!("{error:#}"))
    });
    direct::DeviceApproval { admit, persist }
}
