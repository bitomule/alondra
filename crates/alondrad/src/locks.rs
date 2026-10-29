//! A lock whose holder panicked is poisoned, and `lock().unwrap()` then panics in every later
//! reader: on 25 October that left the daemon running and answering nothing. Each lock of the
//! daemon takes one of two policies instead, and says at its declaration which and why:
//!
//! - recover: every holder leaves the value whole at each point where it could panic (one
//!   assignment, one insert or remove, a cache filled again). The next reader logs it once and
//!   goes on with what the lock holds.
//! - exit: a holder can leave it half-changed, and only a fresh start rebuilds it from the
//!   database or the hooks. The next reader logs why and the daemon exits; launchd starts it
//!   again, and the SDK hosts wait for it as they do across any exit on an error.
use std::sync::{LockResult, Mutex, MutexGuard, PoisonError, RwLock, RwLockReadGuard, RwLockWriteGuard};

use crate::runtime_mutex::RuntimeMutex;

/// EX_SOFTWARE: launchd's KeepAlive starts the job again.
pub(crate) const AFTER_PANIC: i32 = 70;

pub(crate) trait Poisonable {
    type Guard<'a> where Self: 'a;
    fn lock_or_recover(&self, what: &str) -> Self::Guard<'_>;
    fn lock_or_exit(&self, what: &str) -> Self::Guard<'_>;
}

impl<T: ?Sized> Poisonable for Mutex<T> {
    type Guard<'a> = MutexGuard<'a, T> where T: 'a;
    fn lock_or_recover(&self, what: &str) -> MutexGuard<'_, T> {
        self.lock().or_recover(what, || self.clear_poison())
    }
    fn lock_or_exit(&self, what: &str) -> MutexGuard<'_, T> {
        self.lock().or_exit(what)
    }
}

impl<T> Poisonable for RuntimeMutex<T> {
    type Guard<'a> = MutexGuard<'a, T> where T: 'a;
    fn lock_or_recover(&self, what: &str) -> MutexGuard<'_, T> {
        self.lock().or_recover(what, || self.clear_poison())
    }
    fn lock_or_exit(&self, what: &str) -> MutexGuard<'_, T> {
        self.lock().or_exit(what)
    }
}

pub(crate) trait PoisonableRw<T: ?Sized> {
    fn read_or_recover(&self, what: &str) -> RwLockReadGuard<'_, T>;
    fn write_or_recover(&self, what: &str) -> RwLockWriteGuard<'_, T>;
}

impl<T: ?Sized> PoisonableRw<T> for RwLock<T> {
    fn read_or_recover(&self, what: &str) -> RwLockReadGuard<'_, T> {
        self.read().or_recover(what, || self.clear_poison())
    }
    fn write_or_recover(&self, what: &str) -> RwLockWriteGuard<'_, T> {
        self.write().or_recover(what, || self.clear_poison())
    }
}

/// For what a `Condvar` hands back, which is poisoned like the lock it waits on.
pub(crate) trait AfterPanic<G> {
    /// `heal` clears the lock's poison, so this is logged once per panic, not once per use.
    fn or_recover(self, what: &str, heal: impl FnOnce()) -> G;
    fn or_exit(self, what: &str) -> G;
}

impl<G> AfterPanic<G> for LockResult<G> {
    fn or_recover(self, what: &str, heal: impl FnOnce()) -> G {
        self.unwrap_or_else(|poisoned: PoisonError<G>| {
            log!("{what}: a thread panicked holding it; going on with what it holds");
            heal();
            poisoned.into_inner()
        })
    }
    fn or_exit(self, what: &str) -> G {
        self.unwrap_or_else(|_| exit_after_panic(what))
    }
}

pub(crate) fn exit_after_panic(what: &str) -> ! {
    crate::exit::now(AFTER_PANIC, || log!(
        "{what}: a thread panicked holding it and may have left it half-changed; \
         exiting with {AFTER_PANIC} so launchd starts the daemon again from what it saved"
    ))
}

/// Leaves `lock` as a thread that panicked holding it does.
#[cfg(test)]
pub(crate) fn poison<T: Send + ?Sized>(lock: &Mutex<T>) {
    std::thread::scope(|scope| {
        let _ = scope.spawn(|| {
            let _held = lock.lock();
            panic!("a holder panics");
        }).join();
    });
    assert!(lock.is_poisoned());
}

#[cfg(test)]
pub(crate) fn poison_rw<T: Send + Sync + ?Sized>(lock: &RwLock<T>) {
    std::thread::scope(|scope| {
        let _ = scope.spawn(|| {
            let _held = lock.write();
            panic!("a holder panics");
        }).join();
    });
    assert!(lock.is_poisoned());
}

/// Runs the ignored test `name` of this binary in a child process and returns how it ended. The
/// child exits without dropping anything, so the files it writes go in `home`, which the parent
/// owns and removes.
#[cfg(test)]
pub(crate) fn in_child(name: &str, home: &std::path::Path) -> std::process::Output {
    std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--ignored", "--exact", name, "--nocapture"])
        .env("ALONDRA_POISON_CHILD", home)
        .output()
        .unwrap()
}

/// The `home` of `in_child`, only inside the child.
#[cfg(test)]
pub(crate) fn child_home() -> Option<std::path::PathBuf> {
    std::env::var_os("ALONDRA_POISON_CHILD").map(Into::into)
}

/// The child's reader reached the lock and the daemon exited for it, with the line that says
/// which lock.
#[cfg(test)]
pub(crate) fn assert_exited_for(child: &std::process::Output, what: &str) {
    let stderr = String::from_utf8_lossy(&child.stderr);
    assert_eq!(child.status.code(), Some(AFTER_PANIC), "the reader must exit the daemon, not panic: {stderr}");
    assert!(stderr.contains(&format!("{what}: a thread panicked holding it and may have left it half-changed")), "{stderr}");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_recovered_lock_keeps_what_the_panicking_holder_left_and_is_healed() {
        let lock = Mutex::new(vec![1, 2]);
        std::thread::scope(|scope| {
            let _ = scope.spawn(|| {
                let mut held = lock.lock().unwrap();
                held.push(3);
                panic!("a holder panics");
            }).join();
        });
        assert_eq!(*lock.lock_or_recover("the probe"), [1, 2, 3]);
        assert!(!lock.is_poisoned(), "healed, so the next reader neither logs nor recovers again");
    }

    #[test]
    fn a_runtime_mutex_and_a_rw_lock_recovered_after_a_panic_are_healed() {
        let mutex = RuntimeMutex::new(7);
        mutex.poison();
        assert_eq!(*mutex.lock_or_recover("the probe"), 7);
        assert!(mutex.lock().is_ok());
        let rw = RwLock::new(8);
        poison_rw(&rw);
        assert_eq!(*rw.read_or_recover("the probe"), 8);
        assert!(!rw.is_poisoned());
        poison_rw(&rw);
        *rw.write_or_recover("the probe") = 9;
        assert_eq!(*rw.read().unwrap(), 9);
    }

    #[test]
    #[ignore = "run by a_reader_of_a_lock_that_exits_after_a_panic_ends_the_process_with_its_reason"]
    fn exit_child() {
        if child_home().is_none() { return; }
        let lock = Mutex::new(0);
        poison(&lock);
        let _held = lock.lock_or_exit("the probe lock");
        unreachable!("lock_or_exit returned on a poisoned lock");
    }

    #[test]
    fn a_reader_of_a_lock_that_exits_after_a_panic_ends_the_process_with_its_reason() {
        let home = crate::test_support::Directory::new();
        assert_exited_for(&in_child("locks::tests::exit_child", &home), "the probe lock");
    }

    #[test]
    #[ignore = "run by readers_on_many_threads_of_a_lock_that_exits_after_a_panic_exit_once"]
    fn many_exits_child() {
        if child_home().is_none() { return; }
        let lock = Mutex::new(0);
        poison(&lock);
        std::thread::scope(|scope| {
            for _ in 0..8 {
                scope.spawn(|| { let _held = lock.lock_or_exit("the probe lock"); });
            }
        });
        unreachable!("lock_or_exit returned on a poisoned lock");
    }

    #[test]
    fn readers_on_many_threads_of_a_lock_that_exits_after_a_panic_exit_once() {
        let home = crate::test_support::Directory::new();
        let child = in_child("locks::tests::many_exits_child", &home);
        assert_exited_for(&child, "the probe lock");
        let stderr = String::from_utf8_lossy(&child.stderr);
        assert_eq!(stderr.matches("may have left it half-changed").count(), 1, "{stderr}");
    }
}
