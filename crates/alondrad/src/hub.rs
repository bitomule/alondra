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
    /// Moves when what clients show changes: they refetch the overview then.
    tick: watch::Sender<u64>,
    /// Moves on every event and machine sample: Alondra's own loops wake on it.
    changes: watch::Sender<u64>,
    /// What clients were last shown: the overview's digest and the machine's rounded gauges.
    shown: Mutex<(u64, u64)>,
    holds: Mutex<HashMap<u64, oneshot::Sender<Answer>>>,
    own_attaches: Mutex<HashSet<String>>,
    closing: Mutex<HashSet<String>>,
    activity: Mutex<HashMap<String, i64>>,
    pub options: Mutex<Option<alondra_core::options::OptionsFile>>,
    pub issues: tokio::sync::Mutex<Option<crate::issues::IssueList>>,
    pub home: PathBuf,
    pub config: Config,
    pub hold_enabled: bool,
    pub pr_lookup: crate::prs::Lookup,
    /// Reads a node's report for a question hidden in it.
    pub report_judge: Box<dyn crate::classify::JudgesReports>,
    /// Held while a task is being started, so it is never started twice.
    pub starts: tokio::sync::Mutex<()>,
}

impl Hub {
    pub fn open(config: Config, hold_enabled: bool) -> Result<Self> {
        let home = config.home.clone();
        std::fs::create_dir_all(home.join("files"))?;
        let mut store = Store::open(&home.join("alondra.db"))?;
        let (after, mut view) = store.latest_snapshot()?.unwrap_or_default();
        view.backfill_created_at();
        view.backfill_tasks();
        let n = store.replay(after, |seq, at, ev| view.apply(seq, at, &ev))?;
        log!("{n} events replayed, {} nodes", view.nodes.len());
        if view.board.recorded_since.is_none() {
            let at = now_millis();
            let seq = store.append(at, &Event::TasksRecorded)?;
            view.apply(seq, at, &Event::TasksRecorded);
        }
        let (tick, _) = watch::channel(0);
        let (changes, _) = watch::channel(0);
        let shown = crate::http::overview_digest(&view, now_millis());
        Ok(Hub {
            writer: Mutex::new(store),
            view: RwLock::new(view),
            machine: RwLock::new(Machine::default()),
            tick,
            changes,
            shown: Mutex::new((shown, 0)),
            holds: Mutex::new(HashMap::new()),
            own_attaches: Mutex::new(HashSet::new()),
            closing: Mutex::new(HashSet::new()),
            activity: Mutex::new(HashMap::new()),
            options: Mutex::new(None),
            issues: tokio::sync::Mutex::new(None),
            home,
            config,
            hold_enabled,
            pr_lookup: crate::prs::Lookup::new(crate::prs::GhCli),
            report_judge: Box::new(crate::classify::Jev),
            starts: tokio::sync::Mutex::new(()),
        })
    }

    #[cfg(test)]
    pub fn with_github(self, gh: impl crate::prs::FindsPrs + 'static) -> Self {
        Hub { pr_lookup: crate::prs::Lookup::new(gh), ..self }
    }

    #[cfg(test)]
    pub fn with_report_judge(self, judge: impl crate::classify::JudgesReports + 'static) -> Self {
        Hub { report_judge: Box::new(judge), ..self }
    }

    /// Records an event, and around it the task events it takes: the task a session's first card
    /// goes to before it, and what it moves after it (`View::record` without the store).
    pub fn append_with(&self, build: impl FnOnce(&View) -> Option<Event>) -> Result<Option<(u64, Event)>> {
        let recorded = self.append_all(|v| build(v).map(|e| vec![e]).ok_or(()))?;
        Ok(recorded.ok().and_then(|mut all| all.pop()))
    }

    /// Records several events built from one view, with nothing else recorded between them, or
    /// records nothing when `build` refuses.
    pub fn append_all<E>(&self, build: impl FnOnce(&View) -> std::result::Result<Vec<Event>, E>) -> Result<std::result::Result<Vec<(u64, Event)>, E>> {
        let mut store = self.writer.lock().expect("writer lock poisoned");
        let events = {
            let view = self.view.read().expect("view lock poisoned");
            match build(&view) {
                Ok(events) => events,
                Err(e) => return Ok(Err(e)),
            }
        };
        let at = now_millis();
        let mut view = self.view.write().expect("view lock poisoned");
        let mut recorded = vec![];
        for event in events {
            for e in view.tasks_before(&event) {
                let s = store.append(at, &e)?;
                view.apply(s, at, &e);
            }
            let before = view.before_tasks(&event);
            let seq = store.append(at, &event)?;
            view.apply(seq, at, &event);
            for e in view.task_moves(at, &event, before) {
                let s = store.append(at, &e)?;
                view.apply(s, at, &e);
            }
            recorded.push((seq, event));
        }
        let digest = crate::http::overview_digest(&view, at);
        drop(view);
        drop(store);
        self.changed(Some(digest), None);
        Ok(Ok(recorded))
    }

    /// Reads the view once nothing is being recorded: whatever an append started before this
    /// call has landed.
    pub fn read_settled<T>(&self, f: impl FnOnce(&View) -> T) -> T {
        let _store = self.writer.lock().expect("writer lock poisoned");
        f(&self.view.read().expect("view lock poisoned"))
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

    /// Marks that Alondra is closing a node, whose process `claude rm` stops on the way: that
    /// exit is not one to restart.
    pub fn closing(&self, session_id: &str) -> Closing<'_> {
        self.closing.lock().expect("closing lock poisoned").insert(session_id.to_string());
        Closing { hub: self, session_id: session_id.to_string() }
    }

    pub fn is_closing(&self, session_id: &str) -> bool {
        self.closing.lock().expect("closing lock poisoned").contains(session_id)
    }

    pub fn read<T>(&self, f: impl FnOnce(&View) -> T) -> T {
        f(&self.view.read().expect("view lock poisoned"))
    }

    pub fn timeline(&self, session: &str, limit: usize, before: Option<u64>) -> Result<Vec<(u64, i64, String)>> {
        self.writer.lock().expect("writer lock poisoned").session_timeline(session, limit, before)
    }

    /// A machine sample, with the subscription's usage read with it. Clients hear of it only when
    /// a gauge crosses a 5 % step; the overview is looked at again too, for what time alone moved.
    pub fn set_machine(&self, m: Machine, usage: Option<&alondra_core::usage::Usage>) {
        let gauges = gauges_digest(&m, usage);
        *self.machine.write().expect("machine lock poisoned") = m;
        let digest = self.read(|v| crate::http::overview_digest(v, now_millis()));
        self.changed(Some(digest), Some(gauges));
    }

    pub fn machine(&self) -> Machine {
        self.machine.read().expect("machine lock poisoned").clone()
    }

    fn changed(&self, overview: Option<u64>, gauges: Option<u64>) {
        self.changes.send_modify(|c| *c += 1);
        let mut shown = self.shown.lock().expect("shown lock poisoned");
        let before = *shown;
        shown.0 = overview.unwrap_or(shown.0);
        shown.1 = gauges.unwrap_or(shown.1);
        if *shown != before {
            self.tick.send_modify(|t| *t += 1);
        }
    }

    /// The clients' tick: it moves when what they show changes.
    pub fn tick(&self) -> u64 {
        *self.tick.borrow()
    }

    pub async fn wait_change(&self, after: u64, wait: Duration) -> u64 {
        wait_past(&self.tick, after, wait).await
    }

    /// Every event and machine sample so far.
    pub fn changes(&self) -> u64 {
        *self.changes.borrow()
    }

    pub async fn wait_changes(&self, after: u64, wait: Duration) -> u64 {
        wait_past(&self.changes, after, wait).await
    }

    pub async fn wait_event(&self, wait: Duration) {
        let mut rx = self.changes.subscribe();
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

async fn wait_past(counter: &watch::Sender<u64>, after: u64, wait: Duration) -> u64 {
    let mut rx = counter.subscribe();
    if *rx.borrow_and_update() > after {
        return *rx.borrow();
    }
    let _ = tokio::time::timeout(wait, rx.changed()).await;
    *rx.borrow()
}

/// The machine's gauges and the subscription's usage as clients read them: in 5 % steps.
fn gauges_digest(m: &Machine, usage: Option<&alondra_core::usage::Usage>) -> u64 {
    use std::hash::{Hash, Hasher};
    let step = |part: f64, whole: f64| if whole > 0.0 { (part / whole * 20.0).floor() as i64 } else { -1 };
    let percent = |p: f64| (p / 5.0).floor() as i64;
    let mut h = std::collections::hash_map::DefaultHasher::new();
    (
        step(m.memory_available as f64, m.memory_total as f64),
        m.memory_free_percent.map(|p| p / 5),
        step(m.load[0], m.cpus as f64),
        percent(m.cpu_usage as f64),
        step(m.disk_available as f64, m.disk_total as f64),
        m.unreliable,
        m.strained(),
    )
        .hash(&mut h);
    if let Some(u) = usage {
        (percent(u.week_percent), u.five_hour_percent.map(percent), u.today_spent.map(percent), u.over_pace).hash(&mut h);
    }
    h.finish()
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

pub struct Closing<'a> {
    hub: &'a Hub,
    session_id: String,
}

impl Drop for Closing<'_> {
    fn drop(&mut self) {
        self.hub.closing.lock().expect("closing lock poisoned").remove(&self.session_id);
    }
}
