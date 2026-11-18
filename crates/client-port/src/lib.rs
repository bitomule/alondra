//! The client port: the daemon client every input adapter of the `alondra` binary talks through,
//! and the words and requests they share. The transport that carries the bytes is installed by
//! the root (`transport::install`), so no adapter depends on it or on another adapter.
#[macro_use]
pub mod i18n;
pub mod account;
pub mod client;
pub mod delivery;
pub mod doc_images;
pub mod ops;
pub mod pairing;
pub mod private_file;
pub mod report;
pub mod routes;
pub mod transport;
pub mod views;
#[cfg(test)]
mod test_transport;
#[cfg(test)]
#[path = "../../test_support.rs"]
mod test_support;

/// A request or answer of the client contract in `fixtures/client-v2`.
#[cfg(test)]
pub(crate) fn fixture(name: &str) -> serde_json::Value {
    let path = match std::env::var("RUNFILES_DIR").or_else(|_| std::env::var("TEST_SRCDIR")) {
        Ok(root) => std::path::PathBuf::from(root).join(std::env::var("TEST_WORKSPACE").unwrap_or_else(|_| "_main".into())).join("fixtures/client-v2").join(name),
        Err(_) => std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/client-v2").join(name),
    };
    serde_json::from_str(&std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()))).unwrap()
}
