use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::{Mutex, RwLock};
use std::time::Duration;

use alondra_core::config::Config;
use alondra_core::sessions::Admission;
use alondra_core::{Answer, Event, HookEvent, Machine, View, now_millis};
use anyhow::{Result, bail};
use tokio::sync::{oneshot, watch};

use crate::store::Store;

/// While a launch is under way, the hooks of this many unknown sessions are held, this many each,
/// in case the launch turns out to be theirs.
const HELD_SESSIONS: usize = 32;
const HELD_HOOKS: usize = 20;

/// Hooks of sessions Alondra does not follow, held while launches are under way.
#[derive(Default)]
struct Unclaimed {
    launches: usize,
    hooks: HashMap<String, Vec<HookEvent>>,
}

/// A launch under way: the hooks held for it are dropped once no launch is.
pub struct Launching<'a>(&'a Hub);

impl Drop for Launching<'_> {
    fn drop(&mut self) {
        let mut u = self.0.unclaimed.lock().expect("unclaimed lock poisoned");
        u.launches -= 1;
        if u.launches == 0 {
            u.hooks.clear();
        }
    }
}

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
    unclaimed: Mutex<Unclaimed>,
    own_attaches: Mutex<HashSet<String>>,
    closing: Mutex<HashSet<String>>,
    activity: Mutex<HashMap<String, i64>>,
    pub options: Mutex<Option<alondra_core::options::OptionsFile>>,
    pub issues: tokio::sync::Mutex<Option<crate::issues::IssueList>>,
    pub home: PathBuf,
    /// Claude Code's home, where it keeps its background sessions.
    pub claude_home: PathBuf,
    pub config: Config,
    pub hold_enabled: bool,
    pub pr_lookup: crate::prs::Lookup,
    /// Reads a node's report for a question hidden in it.
    pub report_judge: Box<dyn crate::classify::JudgesReports>,
    /// Held while a task is being started, so it is never started twice.
    pub starts: tokio::sync::Mutex<()>,
    pub schedules: crate::schedules::Schedules,
    /// Runs once as clients are about to be told of a change, so a test can record something then.
    #[cfg(test)]
    before_telling: Mutex<Option<Box<dyn FnOnce() + Send>>>,
}

impl Hub {
    pub fn open(config: Config, hold_enabled: bool) -> Result<Self> {
        let home = config.home.clone();
        std::fs::create_dir_all(home.join("files"))?;
        #[cfg(unix)] {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&home, std::fs::Permissions::from_mode(0o700))?;
        }
        let mut store = Store::open(&home.join("alondra.db"))?;
        let (after, mut view) = store.latest_snapshot()?.unwrap_or_default();
        view.backfill_created_at();
        view.backfill_tasks();
        let n = store.replay(after, |seq, at, ev| view.apply(seq, at, &ev))?;
        let issues = view.integrity_issues();
        if !issues.is_empty() {
            bail!("stored state failed integrity audit: {}", issues.join("; "));
        }
        log!("{n} events replayed, {} nodes", view.nodes.len());
        let record_tasks = view.board.recorded_since.is_none();
        let launched_only = view.launched_only_since.is_none();
        let bootstrap: Vec<Event> = [record_tasks.then_some(Event::TasksRecorded), launched_only.then_some(Event::LaunchedOnly)]
            .into_iter().flatten().collect();
        if !bootstrap.is_empty() {
            let at = now_millis();
            let mut next = view.clone();
            store.transaction(|tx| {
                for event in &bootstrap {
                    let seq = Store::append_in(tx, at, event)?;
                    next.apply(seq, at, event);
                }
                let issues = next.integrity_issues();
                if !issues.is_empty() {
                    bail!("bootstrap failed integrity audit: {}", issues.join("; "));
                }
                Ok(())
            })?;
            view = next;
            if launched_only {
                log!("from now on only sessions Alondra launched are followed; {} left out", view.left_out.len());
            }
        }
        let (tick, _) = watch::channel(0);
        let (changes, _) = watch::channel(0);
        let shown = crate::http::overview_digest(&view, now_millis());
        let schedules = crate::schedules::Schedules::open(&home.join("schedules.db"))?;
        Ok(Hub {
            writer: Mutex::new(store),
            view: RwLock::new(view),
            machine: RwLock::new(Machine::default()),
            tick,
            changes,
            shown: Mutex::new((shown, 0)),
            holds: Mutex::new(HashMap::new()),
            unclaimed: Mutex::new(Unclaimed::default()),
            own_attaches: Mutex::new(HashSet::new()),
            closing: Mutex::new(HashSet::new()),
            activity: Mutex::new(HashMap::new()),
            options: Mutex::new(None),
            issues: tokio::sync::Mutex::new(None),
            home,
            claude_home: crate::watcher::claude_home(),
            config,
            hold_enabled,
            pr_lookup: crate::prs::Lookup::new(crate::prs::GhCli),
            report_judge: Box::new(crate::classify::Jev),
            starts: tokio::sync::Mutex::new(()),
            schedules,
            #[cfg(test)]
            before_telling: Mutex::new(None),
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

    #[cfg(test)]
    pub fn with_claude_home(self, claude_home: PathBuf) -> Self {
        Hub { claude_home, ..self }
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
        // Do not expose a state built from writes SQLite may still roll back. The copy also
        // leaves the live view intact if an insert or the final commit fails.
        let mut next = view.clone();
        let recorded = store.transaction(|tx| {
            let mut recorded = vec![];
            for mut event in events {
                if let Event::Hook(h) = &mut event {
                    h.task_ids = next.hook_task_ids(&h.session_id, h.agent_id.as_deref(), h.task.as_deref());
                    h.task_id = (h.task_ids.len() == 1).then(|| h.task_ids[0].clone());
                }
                for e in next.tasks_before(&event) {
                    let s = Store::append_in(tx, at, &e)?;
                    next.apply(s, at, &e);
                }
                let before = next.before_tasks(&event);
                let seq = Store::append_in(tx, at, &event)?;
                next.apply(seq, at, &event);
                for e in next.task_moves(at, &event, before) {
                    let s = Store::append_in(tx, at, &e)?;
                    next.apply(s, at, &e);
                }
                recorded.push((seq, event));
            }
            let issues = next.integrity_issues();
            if !issues.is_empty() {
                bail!("state failed integrity audit; batch rolled back: {}", issues.join("; "));
            }
            Ok(recorded)
        })?;
        *view = next;
        let digest = crate::http::overview_digest(&view, at);
        drop(view);
        self.changed(Some(digest), None);
        drop(store);
        Ok(Ok(recorded))
    }

    /// Whether Alondra follows the session: it does already, or it launched it and the session
    /// joins now, with the hooks it sent before Alondra could tell it was its own. Nothing is
    /// recorded for any other session.
    pub fn admit(&self, session_id: &str, marked: bool) -> Result<bool> {
        match self.read(|v| v.admission(session_id, marked)) {
            Admission::Follows => return Ok(true),
            Admission::Outside => return Ok(false),
            Admission::Joins => {}
        }
        let early = self.unclaimed.lock().expect("unclaimed lock poisoned").hooks.remove(session_id).unwrap_or_default();
        let joined = self.append_all(|v| match v.admission(session_id, marked) {
            Admission::Joins => Ok(std::iter::once(Event::Joined { session_id: session_id.to_string() }).chain(early.into_iter().map(Event::Hook)).collect()),
            other => Err(other),
        })?;
        Ok(match joined {
            Ok(_) => true,
            Err(other) => other == Admission::Follows,
        })
    }

    /// Marks a launch under way until the guard drops.
    pub fn launching(&self) -> Launching<'_> {
        self.unclaimed.lock().expect("unclaimed lock poisoned").launches += 1;
        Launching(self)
    }

    /// Holds a hook of a session Alondra does not follow while a launch is under way: its short
    /// id is known only once `claude --bg` returns, and the session may already be talking.
    pub fn hold_unclaimed(&self, h: HookEvent) {
        let mut u = self.unclaimed.lock().expect("unclaimed lock poisoned");
        if u.launches == 0 || (u.hooks.len() >= HELD_SESSIONS && !u.hooks.contains_key(&h.session_id)) {
            return;
        }
        let held = u.hooks.entry(h.session_id.clone()).or_default();
        if held.len() < HELD_HOOKS {
            held.push(h);
        }
    }

    /// A launch printed this short id: the session it names joins if it already spoke.
    pub fn admit_launched(&self, short: &str) -> Result<()> {
        let waiting: Vec<String> = self.unclaimed.lock().expect("unclaimed lock poisoned").hooks.keys().filter(|s| s.starts_with(short)).cloned().collect();
        for sid in waiting {
            self.admit(&sid, false)?;
        }
        Ok(())
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
        let archived_for = (self.config.archived_tasks_days * 86_400_000.0) as i64;
        self.append(Event::ArchivedPruned { before: now - archived_for })?;
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
        let _store = self.writer.lock().expect("writer lock poisoned");
        let digest = self.read(|v| crate::http::overview_digest(v, now_millis()));
        self.changed(Some(digest), Some(gauges));
    }

    pub fn machine(&self) -> Machine {
        self.machine.read().expect("machine lock poisoned").clone()
    }

    /// Tells clients what they are shown now. Called with the writer held, so what one append or
    /// sample worked out never lands after what a later one did.
    fn changed(&self, overview: Option<u64>, gauges: Option<u64>) {
        #[cfg(test)]
        {
            let then = self.before_telling.lock().expect("hook lock poisoned").take();
            if let Some(then) = then {
                then();
            }
        }
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::sync::mpsc;

    fn hub(tag: &str) -> Arc<Hub> {
        let home = std::env::temp_dir().join(format!("alondra-hub-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&home);
        let config = Config::resolve(home, &Default::default(), |_| None, || Some("David".into()));
        Arc::new(Hub::open(config, false).unwrap())
    }

    fn prompt(session: &str) -> Event {
        Event::Hook(alondra_core::HookEvent { session_id: session.into(), event: "UserPromptSubmit".into(), ..Default::default() })
    }

    #[test]
    fn failed_second_write_rolls_back_both_disk_and_live_state() {
        let hub = hub("atomic-batch");
        let before = hub.read(Clone::clone);
        let db = rusqlite::Connection::open(hub.home.join("alondra.db")).unwrap();
        let count: i64 = db.query_row("SELECT COUNT(*) FROM events", [], |r| r.get(0)).unwrap();
        db.execute_batch("CREATE TRIGGER reject_second_hook BEFORE INSERT ON events WHEN NEW.session = 'second' BEGIN SELECT RAISE(ABORT, 'injected write failure'); END;").unwrap();
        assert!(hub.append_all(|_| Ok::<_, ()>(vec![prompt("first"), prompt("second")])).is_err());
        assert_eq!(hub.read(Clone::clone), before);
        let after: i64 = db.query_row("SELECT COUNT(*) FROM events", [], |r| r.get(0)).unwrap();
        assert_eq!(after, count);
    }

    #[test]
    fn hook_has_a_single_task_id_only_when_attribution_is_unique() {
        let hub = hub("hook-attribution");
        let observed = |hub: &Hub, named: Option<&str>| {
            let mut hook = alondra_core::HookEvent {
                session_id: "s".into(), event: "PreToolUse".into(),
                task: named.map(str::to_string), ..Default::default()
            };
            // Inputs may carry stale attribution; the writer derives it from its own view.
            hook.task_id = Some("stale".into());
            hook.task_ids = vec!["stale".into()];
            let (_, Event::Hook(hook)) = hub.append_with(|_| Some(Event::Hook(hook))).unwrap().unwrap() else { panic!("hook was not recorded") };
            hook
        };
        let before = observed(&hub, None);
        assert_eq!(before.task_id, None);
        assert!(before.task_ids.is_empty());
        for id in ["t-a", "t-b"] {
            hub.append(Event::TaskCreated(alondra_core::tasks::NewTask {
                id: id.into(), request: "r-1".into(), title: id.into(), brief: None,
                depends_on: vec![], options: Default::default(), session_id: Some("s".into()),
                launched: false, launched_as: None, start: None,
            })).unwrap();
            if id == "t-a" {
                assert_eq!(observed(&hub, None).task_id.as_deref(), Some("t-a"));
            }
        }
        let ambiguous = observed(&hub, None);
        assert_eq!(ambiguous.task_id, None);
        assert_eq!(ambiguous.task_ids, ["t-a", "t-b"]);
        assert_eq!(observed(&hub, Some("t-b")).task_id.as_deref(), Some("t-b"));
    }

    #[test]
    #[ignore]
    fn replay_a_copy_of_the_real_database_passes_integrity() {
        let home = std::path::PathBuf::from(std::env::var("ALONDRA_REPLAY_HOME").unwrap());
        let config = Config::resolve(home, &Default::default(), |_| None, || Some("David".into()));
        let hub = Hub::open(config, false).unwrap();
        assert!(hub.read(|v| v.integrity_issues().is_empty()));
    }

    /// Records a new node from another thread just as `hub` is about to tell clients, and gives
    /// that append the time to land there if nothing holds it back.
    fn another_append_then(hub: &Arc<Hub>) -> std::thread::JoinHandle<()> {
        let (landed, waited) = mpsc::channel();
        let (go, started) = mpsc::channel::<()>();
        let other = hub.clone();
        let handle = std::thread::spawn(move || {
            started.recv().unwrap();
            other.append(prompt("b")).unwrap();
            let _ = landed.send(());
        });
        *hub.before_telling.lock().unwrap() = Some(Box::new(move || {
            go.send(()).unwrap();
            let _ = waited.recv_timeout(Duration::from_millis(300));
        }));
        handle
    }

    fn shown_is_current(hub: &Hub) -> bool {
        hub.shown.lock().unwrap().0 == hub.read(|v| crate::http::overview_digest(v, now_millis()))
    }

    /// Round 2: the digest was handed to clients after the writer was released, so a later
    /// append could tell its digest first and this one's older digest stayed as what clients
    /// were last shown: the next change back to it moved no tick.
    #[test]
    fn what_clients_were_last_shown_is_the_last_append_even_when_two_race() {
        let hub = hub("race-append");
        let other = another_append_then(&hub);
        hub.append(prompt("a")).unwrap();
        other.join().unwrap();
        assert_eq!(hub.read(|v| v.nodes.len()), 2);
        assert!(shown_is_current(&hub), "what clients were last told is the view as it is");
    }

    #[test]
    fn a_machine_sample_never_tells_an_older_view_than_an_append_did() {
        let hub = hub("race-sample");
        hub.append(prompt("a")).unwrap();
        let other = another_append_then(&hub);
        hub.set_machine(Machine::default(), None);
        other.join().unwrap();
        assert!(shown_is_current(&hub), "what clients were last told is the view as it is");
    }
}
