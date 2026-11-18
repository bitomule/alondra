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
        fn system_command(&self, _: CommandEnvelope) -> Result<CommandOutcome, DomainError> {
            unreachable!("no extension here commits")
        }
        fn wake_seals(&self) {}
        fn launch_plan(&self, _: &alondra_domain_rules::TaskId) -> Option<alondra_domain_rules::LaunchPlan> {
            None
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
