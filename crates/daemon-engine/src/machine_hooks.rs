//! What the model of the whole machine (`crates/machine-model`) runs of the daemon's loops: the
//! decisions the obligation watch takes, called as they are. The gate's it reads from
//! `engine-admission`, the hosts' owners' from `engine-sdk`, the orchestrator's inbox from
//! `engine-orchestrator`. Only the `test-hooks` feature opens
//! these functions; the daemon keeps them crate-private.

pub mod obligation_watch {
    pub use crate::obligation_watch::{Action, Watch, World};
}

pub mod problems {
    pub use alondra_engine_kernel::domain_runtime::problems::tell_command;
}
