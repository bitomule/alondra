use super::*;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;
use std::sync::mpsc::{Receiver, Sender};

const EPOCH: alondra_core::Millis = 1_700_000_000_000;

/// A Mac whose sample waits for the test: it says when it started and ends when it is told to.
struct Held {
    started: Mutex<Sender<()>>,
    release: Mutex<Receiver<()>>,
}

impl MachineProbe for Held {
    fn sample(&self) -> Machine {
        self.started.lock().unwrap().send(()).unwrap();
        self.release.lock().unwrap().recv().ok();
        Machine {
            at: alondra_core::now_millis(),
            memory_total: 16 << 30,
            memory_available: 12 << 30,
            memory_free_percent: Some(75),
            memory_pressure: None,
            compressor: None,
            load: [0.5; 3],
            cpus: 8,
            cpu_usage: 5.0,
            disk_total: 1 << 40,
            disk_available: 1 << 39,
            disk_purgeable: 0,
            unreliable: false,
        }
    }

    fn size(&self) -> (usize, u64) {
        (8, 16 << 30)
    }
}

/// The sample that a timer woke is work the clock waits for from the moment it is submitted: the
/// scenario runner that moves virtual time waits for it (`clock::working`), so the reading of the
/// Mac lands at the instant that woke it however late the blocking pool starts it.
#[test]
fn a_sample_the_timer_woke_counts_as_work_until_it_is_published() {
    let runtime = clock::VirtualRuntime::new(EPOCH).unwrap();
    runtime.block_on(async {
        let (started, begun) = std::sync::mpsc::channel();
        let (release, released) = std::sync::mpsc::channel();
        let probe = Arc::new(Held { started: Mutex::new(started), release: Mutex::new(released) });
        let published = Arc::new(AtomicUsize::new(0));
        let counting = published.clone();
        let looping = tokio::spawn(sample_loop(probe, move |machine| {
            assert_eq!(machine.at, EPOCH);
            counting.fetch_add(1, Ordering::SeqCst);
        }));
        clock::spawn_blocking_unwatched(move || begun.recv().unwrap()).await.unwrap();
        assert_eq!(clock::working(), 1, "the sample is work while it runs");
        release.send(()).unwrap();
        while published.load(Ordering::SeqCst) == 0 {
            tokio::task::yield_now().await;
        }
        assert_eq!(clock::working(), 0, "the sample is published and nothing works");
        looping.abort();
    });
}
