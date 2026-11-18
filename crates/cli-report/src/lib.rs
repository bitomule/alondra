//! `alondra report`: a crash report of this Mac with everything private left out, its final check,
//! and sending it as an issue of the report repository.
#[macro_use]
extern crate alondra_client_port;

pub(crate) use alondra_client_port::client;
#[cfg(test)]
pub(crate) use alondra_client_port::i18n;

pub mod report;

pub use report::*;
pub use report::templates::install as install_catalog;

#[cfg(test)]
#[path = "../../test_support.rs"]
mod test_support;
