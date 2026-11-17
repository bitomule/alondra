//! The ports through which the pieces of the engine call each other without depending on each
//! other, the values they speak in, and the registry that holds one implementation of each. Each
//! piece that implements a port gives it to the registry in its `wire`; the root calls every
//! `wire` when it puts the daemon together, and a piece that needs a port asks the registry.
pub mod admission;
pub mod agent_host;
pub mod hosts;
pub mod launch;
pub mod machine;
pub mod orchestrator;
pub mod outbox;
mod registry;

pub use registry::{Registry, Runtime};

#[cfg(test)]
#[path = "../../test_support.rs"]
mod test_support;
