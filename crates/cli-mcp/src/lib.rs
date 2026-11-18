//! The MCP servers the node and orchestrator plugins start: the tools an agent calls, each one a
//! request to the daemon through the client port.
pub(crate) use alondra_client_port::{client, doc_images, i18n, routes};

mod mcp;
mod mcp_arguments;

pub use mcp::{serve_node, serve_orchestrator};

#[cfg(test)]
#[path = "../../test_support.rs"]
mod test_support;

/// A client of the fake daemon at `base`, over the transport the binary installs.
#[cfg(test)]
pub(crate) fn test_client(base: String) -> client::Client {
    alondra_client_transport::install();
    client::Client::at(base)
}
