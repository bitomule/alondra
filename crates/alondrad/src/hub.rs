use std::collections::{HashMap, HashSet};
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
    own_attaches: Mutex<HashSet<String>>,
    activity: Mutex<HashMap<String, i64>>,
    pub home: PathBuf,
    pub config: Config,
    pub hold_enabled: bool,
    pub pr_lookup: crate::prs::Lookup,
}

impl Hub {
    pub fn open(config: Config, hold_enabled: bool) -> Result<Self> {
        let home = config.home.clone();
        std::fs::create_dir_all(home.join("files"))?;
        let store = Store::open(&home.join("alondra.db"))?;
        let (after, mut view) = store.latest_snapshot()?.unwrap_or_default();
        view.backfill_created_at();
        let n = store.replay(after, |seq, at, ev| view.apply(seq, at, &ev))?;
        log!("{n} events replayed, {} nodes", view.nodes.len());
        let (tick, _) = watch::channel(0);
        Ok(Hub {
            writer: Mutex::new(store),
            view: RwLock::new(view),
            machine: RwLock::new(Machine::default()),
            tick,
            holds: Mutex::new(HashMap::new()),
            own_attaches: Mutex::new(HashSet::new()),
            activity: Mutex::new(HashMap::new()),
            home,
            config,
            hold_enabled,
            pr_lookup: crate::prs::Lookup::new(crate::prs::GhCli),
        })
    }

    #[cfg(test)]
    pub fn with_github(self, gh: impl crate::prs::FindsPrs + 'static) -> Self {
        Hub { pr_lookup: crate::prs::Lookup::new(gh), ..self }
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

    /// Marks that Alondra itself is attached to a node (to type into it or press Esc), so that
    /// attach is not taken for the user watching.
    pub fn own_attach(&self, short: &str) -> OwnAttach<'_> {
        self.own_attaches.lock().expect("attaches lock poisoned").insert(short.to_string());
        OwnAttach { hub: self, short: short.to_string() }
    }

    /// The user did something in a client.
    pub fn touch(&self, client: &str) {
        self.activity.lock().expect("activity lock poisoned").insert(client.to_string(), now_millis());
    }

    /// When the user last did something in each client.
    pub fn activity(&self) -> HashMap<String, i64> {
        self.activity.lock().expect("activity lock poisoned").clone()
    }

    pub fn is_own_attach(&self, short: &str) -> bool {
        self.own_attaches.lock().expect("attaches lock poisoned").contains(short)
    }

    pub fn read<T>(&self, f: impl FnOnce(&View) -> T) -> T {
        f(&self.view.read().expect("view lock poisoned"))
    }

    pub fn timeline(&self, session: &str, limit: usize, before: Option<u64>) -> Result<Vec<(u64, i64, String)>> {
        self.writer.lock().expect("writer lock poisoned").session_timeline(session, limit, before)
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

}

pub struct OwnAttach<'a> {
    hub: &'a Hub,
    short: String,
}

impl Drop for OwnAttach<'_> {
    fn drop(&mut self) {
        self.hub.own_attaches.lock().expect("attaches lock poisoned").remove(&self.short);
    }
}
