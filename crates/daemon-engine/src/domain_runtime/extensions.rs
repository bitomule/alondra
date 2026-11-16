//! What each subsystem keeps for as long as the runtime lives. The runtime holds it without
//! naming its type: a subsystem declares its state as an [`Extension`] and reads it through
//! [`DomainRuntime::extension`], which creates it on first use.
use super::DomainRuntime;
use std::any::Any;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicUsize, Ordering};

const SLOTS: usize = 16;
static TAKEN: AtomicUsize = AtomicUsize::new(0);

pub trait Extension: Any + Send + Sync + Sized {
    /// The slot of this type, the same in every runtime: `static SLOT: Slot = Slot::new(); &SLOT`.
    fn slot() -> &'static Slot;
    fn create(domain: &DomainRuntime) -> Self;
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

/// Declares `$state` an [`Extension`], created with `Default` or with `$create`.
#[macro_export]
macro_rules! extension {
    ($state:ty) => {
        $crate::extension!($state, |_| <$state>::default());
    };
    ($state:ty, $create:expr) => {
        impl $crate::domain_runtime::Extension for $state {
            fn slot() -> &'static $crate::domain_runtime::Slot {
                static SLOT: $crate::domain_runtime::Slot = $crate::domain_runtime::Slot::new();
                &SLOT
            }
            fn create(domain: &$crate::domain_runtime::DomainRuntime) -> Self {
                ($create)(domain)
            }
        }
    };
}

#[derive(Default)]
pub(super) struct Extensions([OnceLock<Box<dyn Any + Send + Sync>>; SLOTS]);

impl DomainRuntime {
    /// The state `T` keeps in this runtime, created on first use.
    pub fn extension<T: Extension>(&self) -> &T {
        self.extensions.0[T::slot().index()]
            .get_or_init(|| Box::new(T::create(self)))
            .downcast_ref()
            .expect("a slot holds the one type it was given to")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug, PartialEq)]
    struct Counted(usize);

    crate::extension!(Counted, |domain: &DomainRuntime| Counted(domain.extension::<Named>().0.len()));

    struct Named(String);

    crate::extension!(Named, |_| Named("named".into()));

    #[test]
    fn each_runtime_creates_its_own_extension_once_and_one_may_read_another() {
        let first = crate::test_support::Directory::new();
        let second = crate::test_support::Directory::new();
        std::fs::create_dir_all(&first).unwrap();
        std::fs::create_dir_all(&second).unwrap();
        let one = DomainRuntime::open_observed(&first, &first.join("domain.db"), vec![]).unwrap();
        let other = DomainRuntime::open_observed(&second, &second.join("domain.db"), vec![]).unwrap();
        assert!(std::ptr::eq(one.extension::<Counted>(), one.extension::<Counted>()));
        assert!(!std::ptr::eq(one.extension::<Counted>(), other.extension::<Counted>()));
        assert_eq!(one.extension::<Counted>(), &Counted(5));
    }
}
