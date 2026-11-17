//! The interfaces one crate of the engine offers another that may not depend on it. Each is a trait
//! of [`crate::ports`]: the crate that implements it provides it when the engine is put together, and
//! the crate that needs it asks the runtime for it.
use super::DomainRuntime;
use crate::locks::PoisonableRw as _;
use std::any::{Any, TypeId};
use std::collections::HashMap;
use std::sync::{Arc, RwLock};

/// A holder that panics leaves it whole, so it is recovered: one insert or one lookup each.
#[derive(Default)]
pub(super) struct Ports(RwLock<HashMap<TypeId, Box<dyn Any + Send + Sync>>>);

impl DomainRuntime {
    /// Makes `port` the implementation of `P` for this runtime, in place of any before it.
    pub fn provide<P: ?Sized + Send + Sync + 'static>(&self, port: Arc<P>) {
        self.ports.0.write_or_recover("the ports").insert(TypeId::of::<P>(), Box::new(port));
    }

    /// The implementation of `P` this runtime was given, if any.
    pub fn port<P: ?Sized + Send + Sync + 'static>(&self) -> Option<Arc<P>> {
        self.ports.0.read_or_recover("the ports").get(&TypeId::of::<P>())?.downcast_ref::<Arc<P>>().cloned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    trait Greets: Send + Sync {
        fn greet(&self) -> &'static str;
    }

    struct Hello;

    impl Greets for Hello {
        fn greet(&self) -> &'static str {
            "hello"
        }
    }

    #[test]
    fn a_runtime_hands_out_the_port_it_was_given_and_none_it_was_not() {
        let home = crate::test_support::Directory::new();
        std::fs::create_dir_all(&home).unwrap();
        let domain = DomainRuntime::open_unwired(&home).unwrap();
        assert!(domain.port::<dyn Greets>().is_none());
        domain.provide::<dyn Greets>(Arc::new(Hello));
        assert_eq!(domain.port::<dyn Greets>().unwrap().greet(), "hello");
    }
}
