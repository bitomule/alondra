//! Each piece of the engine keeps its state in the runtime as an extension of `engine-ports`; the
//! runtime only holds them.
use super::DomainRuntime;
pub use alondra_engine_ports::extensions::{Extension, Extensions, Slot};

impl DomainRuntime {
    /// The state `T` keeps in this runtime, created on first use.
    pub fn extension<T: Extension>(&self) -> &T {
        alondra_engine_ports::extension(self)
    }
}
