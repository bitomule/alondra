//! The terminal app: the screens the daemon renders, drawn and driven from the keyboard, one
//! instance per Mac this terminal reaches.
#[macro_use]
extern crate alondra_client_port;

pub(crate) use alondra_client_port::{client, delivery, doc_images, i18n, ops, report, routes};

mod pty;
mod runtime;
mod tui;

pub use tui::run;

#[cfg(test)]
#[path = "../../test_support.rs"]
mod test_support;

/// A client of the fake daemon at `base`, over the transport the binary installs.
#[cfg(test)]
pub(crate) fn test_client(base: String) -> client::Client {
    alondra_client_transport::install();
    client::Client::at(base)
}

/// A Mac paired directly that never answers: the TUI shows it by its name and id.
#[cfg(test)]
pub(crate) fn test_mac(id: &str, name: &str) -> client::Target {
    struct Silent {
        id: String,
        name: String,
    }
    impl alondra_client_port::transport::Remote for Silent {
        fn request(&self, _: &str, _: &str, _: Option<&serde_json::Value>, _: std::time::Duration, _: Option<&client::availability::ResponseObserver>) -> anyhow::Result<(u16, serde_json::Value)> {
            Err(anyhow::Error::new(client::Unsent(format!("{} is not on this network", self.name))))
        }
    }
    impl alondra_client_port::transport::DirectMac for Silent {
        fn mac_id(&self) -> &str {
            &self.id
        }
        fn name(&self) -> &str {
            &self.name
        }
        fn route_kind(&self) -> Option<alondra_client_port::transport::RouteKind> {
            None
        }
    }
    client::Target::Direct(std::sync::Arc::new(Silent { id: id.into(), name: name.into() }))
}
