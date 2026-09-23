use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Mutex, RwLock};
use std::time::Duration;

use alondra_core::config::Config;
use alondra_core::{Answer, Event, Machine, View, now_millis};
use anyhow::Result;
use tokio::sync::{oneshot, watch};

use crate::store::Store;

pub struct Hub {
    writer: Mutex<Store>,
    view: RwLock<View>,
    machine: RwLock<Machine>,
    tick: watch::Sender<u64>,
    holds: Mutex<HashMap<u64, oneshot::Sender<Answer>>>,
    inbox_waiters: Mutex<HashMap<String, u64>>,
    pub home: PathBuf,
    pub config: Config,
    pub hold_enabled: bool,
}

impl Hub {
    pub fn open(config: Config, hold_enabled: bool) -> Result<Self> {
        let home = config.home.clone();
        std::fs::create_dir_all(home.join("files"))?;
        let store = Store::open(&home.join("alondra.db"))?;
        let (after, mut view) = store.latest_snapshot()?.unwrap_or_default();
        let n = store.replay(after, |seq, at, ev| view.apply(seq, at, &ev))?;
        eprintln!("alondrad: {n} events replayed, {} nodes", view.nodes.len());
        let (tick, _) = watch::channel(0);
        Ok(Hub {
            writer: Mutex::new(store),
            view: RwLock::new(view),
            machine: RwLock::new(Machine::default()),
            tick,
            holds: Mutex::new(HashMap::new()),
            inbox_waiters: Mutex::new(HashMap::new()),
            home,
            config,
            hold_enabled,
        })
    }

    pub fn append_with(&self, build: impl FnOnce(&View) -> Option<Event>) -> Result<Option<(u64, Event)>> {
        let mut store = self.writer.lock().expect("writer lock poisoned");
        let event = {
            let view = self.view.read().expect("view lock poisoned");
            match build(&view) {
                Some(e) => e,
                None => return Ok(None),
            }
        };
        let at = now_millis();
        let seq = store.append(at, &event)?;
        self.view.write().expect("view lock poisoned").apply(seq, at, &event);
        drop(store);
        self.bump();
        Ok(Some((seq, event)))
    }

    pub fn compact(&self, retention: std::time::Duration) -> Result<usize> {
        let now = alondra_core::now_millis();
        let before = now - retention.as_millis() as i64;
        self.append(Event::Pruned { before })?;
        let mut store = self.writer.lock().expect("writer lock poisoned");
        let view = self.view.read().expect("view lock poisoned").clone();
        store.compact(&view, now, before)
    }

    pub fn append(&self, event: Event) -> Result<u64> {
        Ok(self.append_with(|_| Some(event))?.map(|(s, _)| s).unwrap_or(0))
    }

    pub fn read<T>(&self, f: impl FnOnce(&View) -> T) -> T {
        f(&self.view.read().expect("view lock poisoned"))
    }

    pub fn timeline(&self, session: &str, limit: usize) -> Result<Vec<(u64, i64, String)>> {
        self.writer.lock().expect("writer lock poisoned").session_timeline(session, limit)
    }

    pub fn set_machine(&self, m: Machine) {
        *self.machine.write().expect("machine lock poisoned") = m;
        self.bump();
    }

    pub fn machine(&self) -> Machine {
        self.machine.read().expect("machine lock poisoned").clone()
    }

    fn bump(&self) {
        self.tick.send_modify(|t| *t += 1);
    }

    pub fn tick(&self) -> u64 {
        *self.tick.borrow()
    }

    pub async fn wait_change(&self, after: u64, wait: Duration) -> u64 {
        let mut rx = self.tick.subscribe();
        if *rx.borrow_and_update() > after {
            return *rx.borrow();
        }
        let _ = tokio::time::timeout(wait, rx.changed()).await;
        *rx.borrow()
    }

    pub async fn wait_event(&self, wait: Duration) {
        let mut rx = self.tick.subscribe();
        rx.borrow_and_update();
        let _ = tokio::time::timeout(wait, rx.changed()).await;
    }

    pub fn register_hold(&self, question: u64) -> oneshot::Receiver<Answer> {
        let (tx, rx) = oneshot::channel();
        self.holds.lock().expect("holds lock poisoned").insert(question, tx);
        rx
    }

    pub fn release_hold(&self, question: u64, answer: Answer) -> bool {
        match self.holds.lock().expect("holds lock poisoned").remove(&question) {
            Some(tx) => tx.send(answer).is_ok(),
            None => false,
        }
    }

    pub fn drop_hold(&self, question: u64) {
        self.holds.lock().expect("holds lock poisoned").remove(&question);
    }

    pub fn holds_count(&self) -> usize {
        self.holds.lock().expect("holds lock poisoned").len()
    }

    pub fn claim_inbox(&self, session: &str) -> u64 {
        let mut w = self.inbox_waiters.lock().expect("inbox lock poisoned");
        let generation = w.get(session).copied().unwrap_or(0) + 1;
        w.insert(session.to_string(), generation);
        drop(w);
        self.bump();
        generation
    }

    pub fn inbox_current(&self, session: &str, generation: u64) -> bool {
        self.inbox_waiters.lock().expect("inbox lock poisoned").get(session).copied() == Some(generation)
    }

    pub fn release_inbox(&self, session: &str, generation: u64) {
        let mut w = self.inbox_waiters.lock().expect("inbox lock poisoned");
        if w.get(session).copied() == Some(generation) {
            w.remove(session);
        }
    }

    pub fn inbox_listening(&self) -> Vec<String> {
        self.inbox_waiters.lock().expect("inbox lock poisoned").keys().cloned().collect()
    }
}
