//! What each piece of the engine keeps for as long as the runtime lives. The runtime holds it
//! without naming its type: a piece declares its state as an [`Extension`] and reads it through
//! [`extension`], which creates it on first use. A piece needs only the [`Runtime`] context for
//! that, never the runtime's own type.
use crate::Runtime;
use std::any::Any;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicUsize, Ordering};

const SLOTS: usize = 16;
static TAKEN: AtomicUsize = AtomicUsize::new(0);

pub trait Extension: Any + Send + Sync + Sized {
    /// The slot of this type, the same in every runtime: `static SLOT: Slot = Slot::new(); &SLOT`.
    fn slot() -> &'static Slot;
    fn create(runtime: &dyn Runtime) -> Self;
}

/// The place of one extension type in every runtime, given the first time any runtime asks for it.
pub struct Slot(OnceLock<usize>);

impl Slot {
    pub const fn new() -> Self {
        Self(OnceLock::new())
    }

    fn index(&self) -> usize {
        *self.0.get_or_init(|| {
            let index = TAKEN.fetch_add(1, Ordering::SeqCst);
            assert!(index < SLOTS, "more than {SLOTS} extension types: raise SLOTS");
            index
        })
    }
}

impl Default for Slot {
    fn default() -> Self {
        Self::new()
    }
}

/// Declares `$state` an [`Extension`], created with `Default` or with `$create`, which is given the
/// runtime as `&dyn Runtime`.
#[macro_export]
macro_rules! extension {
    ($state:ty) => {
        $crate::extension!($state, |_: &dyn $crate::Runtime| <$state>::default());
    };
    ($state:ty, $create:expr) => {
        impl $crate::extensions::Extension for $state {
            fn slot() -> &'static $crate::extensions::Slot {
                static SLOT: $crate::extensions::Slot = $crate::extensions::Slot::new();
                &SLOT
            }
            fn create(runtime: &dyn $crate::Runtime) -> Self {
                ($create)(runtime)
            }
        }
    };
}

/// The extensions of one runtime, which it keeps and hands out through [`Runtime::extensions`].
#[derive(Default)]
pub struct Extensions([OnceLock<Box<dyn Any + Send + Sync>>; SLOTS]);

/// The state `T` keeps in `runtime`, created on first use.
pub fn extension<T: Extension>(runtime: &dyn Runtime) -> &T {
    runtime.extensions().0[T::slot().index()]
        .get_or_init(|| Box::new(T::create(runtime)))
        .downcast_ref()
        .expect("a slot holds the one type it was given to")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Registry;
    use alondra_domain_rules::{CommandEnvelope, CommandOutcome, DomainError, DomainState};
    use std::path::{Path, PathBuf};
    use std::sync::Arc;

    #[derive(Default)]
    struct Context {
        home: PathBuf,
        ports: Registry,
        extensions: Extensions,
    }

    impl Runtime for Context {
        fn ports(&self) -> &Registry {
            &self.ports
        }
        fn extensions(&self) -> &Extensions {
            &self.extensions
        }
        fn home(&self) -> &Path {
            &self.home
        }
        fn state(&self) -> Arc<DomainState> {
            unreachable!("no extension here reads the state")
        }
        fn read(&self) -> Arc<alondra_domain_rules::ReadSnapshot> {
            unreachable!("no extension here reads the index")
        }
        fn live_read(&self) -> crate::journal::ReadTransaction<'_> {
            unreachable!("no extension here reads the journal")
        }
        fn store(&self) -> &alondra_foundation::runtime_mutex::RuntimeMutex<Box<dyn crate::journal::Journal>> {
            unreachable!("no extension here writes the journal")
        }
        fn subscribe(&self) -> tokio::sync::watch::Receiver<alondra_domain_rules::Revision> {
            unreachable!("no extension here waits for a commit")
        }
        fn subscribe_snapshots(&self) -> tokio::sync::watch::Receiver<u64> {
            unreachable!("no extension here waits for a snapshot")
        }
        fn subscribe_schedules(&self) -> tokio::sync::watch::Receiver<alondra_domain_rules::Revision> {
            unreachable!("no extension here waits for a schedule")
        }
        fn wake_subscribers(&self) {}
        fn execute_as_system(&self, _: CommandEnvelope) -> Result<CommandOutcome, crate::journal::StoreError> {
            unreachable!("no extension here commits")
        }
        fn execute_authorized(&self, _: &alondra_domain_rules::Actor, _: &CommandEnvelope, _: Option<crate::journal::TransportIdentity<'_>>, _: Option<&crate::journal::RequestLaunch>, _: &mut dyn FnMut() -> Result<(), crate::journal::StoreError>) -> Result<CommandOutcome, crate::journal::StoreError> {
            unreachable!("no extension here commits")
        }
        fn own_with(&self, _: alondra_store::AuthorityScope, _: &mut dyn FnMut(&mut dyn crate::journal::OwnerJournal) -> Result<(), crate::journal::StoreError>) -> Result<Vec<crate::journal::owner::StagedOutcome>, crate::journal::StoreError> {
            unreachable!("no extension here commits")
        }
        fn own_projected_with(&self, _: alondra_store::AuthorityScope, _: &mut dyn FnMut(&mut dyn crate::journal::OwnerJournal) -> Result<(), crate::journal::StoreError>, _: &mut dyn FnMut(&mut dyn crate::journal::Journal) -> anyhow::Result<()>) -> Result<Vec<crate::journal::owner::StagedOutcome>, crate::journal::StoreError> {
            unreachable!("no extension here commits")
        }
        fn take_seals(&self, _: &alondra_domain_rules::TaskId) -> alondra_store::Committed {
            unreachable!("no extension here takes seals")
        }
        fn wait_for_seals(&self, _: &alondra_domain_rules::TaskId, _: std::time::Duration, _: &dyn Fn() -> bool) -> alondra_store::Committed {
            unreachable!("no extension here takes seals")
        }
        fn pause_for_seals(&self, _: std::time::Duration, _: &dyn Fn() -> bool) {}
        fn forget_sealed_observations(&self, _: &alondra_domain_rules::TaskId) {}
        fn held(&self) -> &std::sync::Mutex<std::collections::BTreeMap<alondra_domain_rules::AgentId, Vec<String>>> {
            unreachable!("no extension here holds messages")
        }
        fn system_command(&self, _: CommandEnvelope) -> Result<CommandOutcome, DomainError> {
            unreachable!("no extension here commits")
        }
        fn wake_seals(&self) {}
        fn launch_plan(&self, _: &alondra_domain_rules::TaskId) -> Option<alondra_domain_rules::LaunchPlan> {
            None
        }
        fn bind_agent(&self, _: &str) -> Result<alondra_foundation::principal::Capability, crate::journal::StoreError> {
            unreachable!("no extension here issues capabilities")
        }
        fn revoke_agent(&self, _: &str) -> Result<(), crate::journal::StoreError> {
            unreachable!("no extension here revokes capabilities")
        }
        fn admit_change(&self, _: &str, _: &str) -> Result<alondra_foundation::principal::Principal, crate::journal::StoreError> {
            unreachable!("no extension here reads bearers")
        }
        fn resolve_bearer(&self, _: &str) -> Result<alondra_foundation::principal::Principal, crate::journal::StoreError> {
            unreachable!("no extension here reads bearers")
        }
        fn agent_capability(&self, _: &str) -> Option<[u8; 32]> {
            unreachable!("no extension here reads capabilities")
        }
        fn revoke_capability(&self, _: &[u8; 32]) -> Result<(), crate::journal::StoreError> {
            unreachable!("no extension here revokes capabilities")
        }
        fn transport_receipt(&self, _: crate::journal::TransportIdentity<'_>, _: &mut dyn FnMut() -> Result<(), crate::journal::StoreError>) -> Result<Option<CommandOutcome>, crate::journal::StoreError> {
            unreachable!("no extension here reads receipts")
        }
        fn before_acknowledge(&self) {}
        fn read_current_task_context(&self, _: &str, _: &alondra_domain_rules::TaskId, _: Option<&alondra_store::ContextRef>, _: usize) -> Result<alondra_store::ContextPage, DomainError> {
            unreachable!("no extension here reads a conversation")
        }
    }

    #[derive(Debug, PartialEq)]
    struct Counted(usize);

    crate::extension!(Counted, |runtime: &dyn Runtime| Counted(extension::<Named>(runtime).0.len() + runtime.home().as_os_str().len()));

    struct Named(String);

    crate::extension!(Named, |_: &dyn Runtime| Named("named".into()));

    #[test]
    fn each_runtime_creates_its_own_extension_once_and_one_may_read_another() {
        let one = Context { home: PathBuf::from("/a"), ..Context::default() };
        let other = Context::default();
        assert!(std::ptr::eq(extension::<Counted>(&one), extension::<Counted>(&one)));
        assert!(!std::ptr::eq(extension::<Counted>(&one), extension::<Counted>(&other)));
        assert_eq!(extension::<Counted>(&one), &Counted(7));
        assert_eq!(extension::<Counted>(&other), &Counted(5));
    }
}
