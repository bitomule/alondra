//! The `alondra` commands: the command line, setting up and checking this Mac, the account, the
//! devices and Macs a terminal uses, and the one-shot commands that act on the daemon.
#[macro_use]
extern crate alondra_client_port;

pub(crate) use alondra_client_port::{client, i18n, ops, routes};

pub mod account;
pub mod args;
mod claude_auth;
mod cleanup;
mod cli;
mod commands;
mod delivery;
pub mod doctor;
mod files;
mod handover;
mod help;
mod metrics;
mod node;
mod provider_accounts;
mod report_wait;
mod safe_permissions;
mod service_pair;
mod service_setup;
mod setup;
mod setup_projects;
pub mod spec;
mod state;
mod update;
mod version;

#[cfg(test)]
#[path = "../../test_support.rs"]
mod test_support;

/// A client of the fake daemon at `base`, over the transport the binary installs.
#[cfg(test)]
pub(crate) fn test_client(base: String) -> client::Client {
    alondra_client_transport::install();
    client::Client::at(base)
}
