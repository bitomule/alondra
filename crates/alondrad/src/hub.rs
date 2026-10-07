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
    pub domain: std::sync::Arc<crate::domain_runtime::DomainRuntime>,
    writer: Mutex<Store>,
    view: RwLock<View>,
    machine: RwLock<Machine>,
    /// Moves when what clients show changes: they refetch the overview then.
    tick: watch::Sender<u64>,
    /// Moves on every event and machine sample: Alondra's own loops wake on it.
    changes: watch::Sender<u64>,
    /// What clients were last shown: the overview's digest and the machine's rounded gauges.
    shown: Mutex<(u64, u64)>,
    holds: Mutex<HashMap<u64, oneshot::Sender<(Answer, u64)>>>,
    unclaimed: Mutex<Unclaimed>,
    closing: Mutex<HashMap<String, usize>>,
    activity: Mutex<HashMap<String, i64>>,
    pub options: Mutex<Option<alondra_core::options::OptionsFile>>,
    pub issues: tokio::sync::Mutex<Option<crate::issues::IssueList>>,
    pub home: PathBuf,
    pub config: Config,
    pub pr_lookup: crate::prs::Lookup,
    /// Held while a task is being started, so it is never started twice.
    pub starts: tokio::sync::Mutex<()>,
    /// Accepted orchestrator RPCs finish before lifecycle changes revoke their authority.
    pub orchestrator_commands: tokio::sync::RwLock<()>,
    pub schedules: crate::schedules::Schedules,
    /// Runs once as clients are about to be told of a change, so a test can record something then.
    #[cfg(test)]
    before_telling: Mutex<Option<Box<dyn FnOnce() + Send>>>,
}

impl Hub {
    pub fn open(config: Config) -> Result<Self> {
        let home = config.home.clone();
        std::fs::create_dir_all(home.join("files"))?;
        #[cfg(unix)] {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&home, std::fs::Permissions::from_mode(0o700))?;
        }
        let domain = std::sync::Arc::new(crate::domain_runtime::DomainRuntime::open(&home)?);
        let mut store = Store::open(&home.join("alondra.db"))?;
        let (after, mut view) = store.latest_snapshot()?.unwrap_or_default();
        view.backfill_created_at();
        view.backfill_delivery_ids();
        view.backfill_tasks();
        let n = store.replay(after, |seq, at, ev| view.apply(seq, at, &ev))?;
        let issues = view.integrity_issues();
        if !issues.is_empty() {
            bail!("stored state failed integrity audit: {}", issues.join("; "));
        }
        log!("{n} events replayed, {} nodes", view.nodes.len());
        let record_tasks = view.board.recorded_since.is_none();
        let launched_only = view.launched_only_since.is_none();
        let mut bootstrap: Vec<Event> = [record_tasks.then_some(Event::TasksRecorded), launched_only.then_some(Event::LaunchedOnly)]
            .into_iter().flatten().collect();
        if view.board.splits_released_since.is_none() {
            bootstrap.extend(released_splits(&view));
        }
        bootstrap.extend(crate::delivery::recovery_events(&view));
        if !bootstrap.is_empty() {
            let at = now_millis();
            let mut next = view.clone();
            // Orphaning a held question unblocks its task: those go through the same task
            // derivation as any runtime event.
            let (orphans, raw): (Vec<Event>, Vec<Event>) = bootstrap.into_iter()
                .partition(|e| matches!(e, Event::QuestionClosed { .. }));
            store.transaction(|tx| {
                for event in &raw {
                    let seq = Store::append_in(tx, at, event)?;
                    next.apply(seq, at, event);
                }
                Self::record_events(tx, &mut next, orphans, at)?;
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
            domain,
            writer: Mutex::new(store),
            view: RwLock::new(view),
            machine: RwLock::new(Machine::default()),
            tick,
            changes,
            shown: Mutex::new((shown, 0)),
            holds: Mutex::new(HashMap::new()),
            unclaimed: Mutex::new(Unclaimed::default()),
            closing: Mutex::new(HashMap::new()),
            activity: Mutex::new(HashMap::new()),
            options: Mutex::new(None),
            issues: tokio::sync::Mutex::new(None),
            home,
            config,
            pr_lookup: crate::prs::Lookup::new(crate::prs::GhCli),
            starts: tokio::sync::Mutex::new(()),
            orchestrator_commands: tokio::sync::RwLock::new(()),
            schedules,
            #[cfg(test)]
            before_telling: Mutex::new(None),
        })
    }

    #[cfg(test)]
    pub fn with_github(self, gh: impl crate::prs::FindsPrs + 'static) -> Self {
        Hub { pr_lookup: crate::prs::Lookup::new(gh), ..self }
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
            Self::record_events(tx, &mut next, events, at)
        })?;
        *view = next;
        let digest = crate::http::overview_digest(&view, at);
        drop(view);
        self.changed(Some(digest), None);
        drop(store);
        Ok(Ok(recorded))
    }

    fn record_events(
        tx: &rusqlite::Transaction<'_>,
        next: &mut View,
        events: Vec<Event>,
        at: i64,
    ) -> Result<Vec<(u64, Event)>> {
        let mut recorded = vec![];
        for mut event in events {
            if let Event::Hook(h) = &mut event {
                h.task_ids =
                    next.hook_task_ids(&h.session_id, h.agent_id.as_deref(), h.task.as_deref());
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
            bail!(
                "state failed integrity audit; batch rolled back: {}",
                issues.join("; ")
            );
        }
        Ok(recorded)
    }

    /// The event batch and its stable service result are one SQLite commit.
    /// A replay reads the result without rebuilding or publishing any events.
    pub(crate) fn local_command(
        &self, identity: Option<(&str, &str, &[u8; 32])>,
        build: impl FnOnce(&View) -> std::result::Result<Vec<Event>, serde_json::Value>,
        finish: impl FnOnce(&View, &[(u64, Event)]) -> serde_json::Value,
    ) -> Result<serde_json::Value> {
        let mut store = self.writer.lock().expect("writer lock poisoned");
        let mut view = self.view.write().expect("view lock poisoned");
        let mut next = view.clone();
        let at = now_millis();
        let mut publish = false;
        let result = store.transaction(|tx| {
            if let Some((sender, id, digest)) = identity {
                if let Some(result) = Store::command_result(tx, sender, id, digest)? { return Ok(result); }
            }
            let result = match build(&next) {
                Ok(events) => {
                    let recorded = Self::record_events(tx, &mut next, events, at)?;
                    publish = !recorded.is_empty();
                    finish(&next, &recorded)
                }
                Err(result) => result,
            };
            if let Some((sender, id, digest)) = identity {
                Store::save_command_result(tx, sender, id, digest, &result)?;
            }
            Ok(result)
        })?;
        if publish { *view = next; }
        let digest = publish.then(|| crate::http::overview_digest(&view, at));
        drop(view);
        if publish { self.changed(digest, None); }
        Ok(result)
    }

    /// Whether Alondra follows the session: it does already, or it launched it and the session
    /// joins now, with the hooks it sent before Alondra could tell it was its own. Nothing is
    /// recorded for any other session.
    pub fn admit(&self, session_id: &str, marked: bool) -> Result<bool> {
        if self.read(|v| v.orchestrator_migration.as_ref().is_some_and(|m| m.session_id == session_id)
            || v.orchestrator.as_ref().is_some_and(|r| r.session_id.as_deref() == Some(session_id))) { return Ok(false); }
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

    /// The user did something in a client.
    pub fn touch(&self, client: &str) {
        self.activity.lock().expect("activity lock poisoned").insert(client.to_string(), now_millis());
    }

    /// When the user last did something in each client.
    pub fn activity(&self) -> HashMap<String, i64> {
        self.activity.lock().expect("activity lock poisoned").clone()
    }

    /// Marks that Alondra is closing a node, whose process `claude rm` stops on the way: that
    /// exit is not one to restart.
    pub fn closing(&self, session_id: &str) -> Closing<'_> {
        *self.closing.lock().expect("closing lock poisoned").entry(session_id.to_string()).or_default() += 1;
        Closing { hub: self, session_id: session_id.to_string() }
    }

    pub fn is_closing(&self, session_id: &str) -> bool {
        self.closing.lock().expect("closing lock poisoned").contains_key(session_id)
    }

    pub fn read<T>(&self, f: impl FnOnce(&View) -> T) -> T {
        f(&self.view.read().expect("view lock poisoned"))
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

    /// Publish the question and its receiver together to answer callers.
    pub fn register_question_hold(&self, build: impl FnOnce(&View) -> alondra_core::Question) -> Result<(u64, oneshot::Receiver<(Answer, u64)>)> {
        let mut holds = self.holds.lock().expect("holds lock poisoned");
        let (_, Event::QuestionAsked(question)) = self.append_with(|v| Some(Event::QuestionAsked(build(v))))?
            .expect("question builder always records") else { unreachable!() };
        let (tx, rx) = oneshot::channel();
        holds.insert(question.id, tx);
        Ok((question.id, rx))
    }

    pub fn has_hold(&self, question: u64) -> bool {
        self.holds.lock().expect("holds lock poisoned").contains_key(&question)
    }

    pub fn release_hold(&self, question: u64, answer: Answer, attempt: u64) -> bool {
        match self.holds.lock().expect("holds lock poisoned").remove(&question) {
            Some(tx) => tx.send((answer, attempt)).is_ok(),
            None => false,
        }
    }

    pub fn drop_hold(&self, question: u64) {
        self.holds.lock().expect("holds lock poisoned").remove(&question);
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
/// What `new_task` gave its opener before it asked, and nobody touched: back to no node, once.
fn released_splits(v: &View) -> Vec<Event> {
    let ids = v.untouched_splits();
    let mut events: Vec<Event> = ids.iter().map(|id| Event::TaskReleased { task: id.clone() }).collect();
    events.push(Event::SplitsReleased);
    if !ids.is_empty() {
        log!("{} tasks new_task gave their opener untouched go back to no node: {}", ids.len(), ids.join(", "));
        events.extend(crate::orchestrate::orchestrator_message(v, crate::orchestrate::unstarted_notice(v, &ids)));
    }
    events
}

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

pub struct Closing<'a> {
    hub: &'a Hub,
    session_id: String,
}

impl Drop for Closing<'_> {
    fn drop(&mut self) {
        let mut closing = self.hub.closing.lock().expect("closing lock poisoned");
        if let Some(count) = closing.get_mut(&self.session_id) {
            *count -= 1;
            if *count == 0 { closing.remove(&self.session_id); }
        }
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
        Arc::new(Hub::open(config).unwrap())
    }

    fn prompt(session: &str) -> Event {
        Event::Hook(alondra_core::HookEvent { session_id: session.into(), event: "UserPromptSubmit".into(), ..Default::default() })
    }

    /// 28-sep 20:15: the first 0.1.12 start orphaned the held cards blocking t-67464 and
    /// t-67482 without unblocking the tasks, failed its integrity audit and never started.
    #[test]
    fn restart_unblocks_the_task_a_held_card_was_blocking() {
        let hub = hub("held-card-restart");
        hub.append(prompt("s")).unwrap();
        let held: alondra_core::Question = serde_json::from_value(serde_json::json!({
            "id": 0, "session_id": "s", "node_name": null, "title": "Allow Bash?", "blocking": true,
            "origin": "permission", "tool_use_id": "toolu_1", "status": "open", "asked_at": 1})).unwrap();
        hub.register_question_hold(|_| held).unwrap();
        let blocked = |hub: &Hub| hub.read(|v| v.board.tasks.values().any(|t| t.blocker.is_some()));
        assert!(blocked(&hub));
        let config = hub.config.clone();
        drop(hub);
        let hub = Hub::open(config).unwrap();
        assert!(!blocked(&hub));
        assert!(hub.read(|v| v.questions.values().all(|q| q.status == alondra_core::QuestionStatus::Orphaned)));
    }

    #[test]
    fn overlapping_close_leases_keep_node_unavailable_until_last_owner_finishes() {
        let hub = hub("overlapping-close");
        let first = hub.closing("node");
        let second = hub.closing("node");
        let unrelated = hub.closing("other");
        drop(first);
        assert!(hub.is_closing("node"), "one completed close must not admit new work during another close");
        drop(second);
        assert!(!hub.is_closing("node"));
        assert!(hub.is_closing("other"));
        drop(unrelated);
        assert!(!hub.is_closing("other"));
    }

    #[test]
    fn local_command_result_survives_restart_without_rebuilding_events() {
        let hub = hub("local-command-restart");
        let result = hub.local_command(Some(("client-a", "operation-1", &[1u8;32])),
            |_| Ok(vec![prompt("local-command")]),
            |v, _| serde_json::json!({"status":200,"body":{"seq":v.seq}})).unwrap();
        let committed = hub.read(Clone::clone);
        let restarted = Hub::open(hub.config.clone()).unwrap();
        let recovered = restarted.local_command(Some(("client-a", "operation-1", &[1u8;32])),
            |_| panic!("recovery must not rebuild the command"), |_,_| panic!("recovery must not recalculate result")).unwrap();
        assert_eq!(recovered, result);
        assert_eq!(restarted.read(Clone::clone), committed);
        assert!(restarted.local_command(Some(("client-a", "operation-1", &[2u8;32])),
            |_| panic!("digest mismatch must be rejected first"), |_,_| panic!()).is_err());
        assert_eq!(restarted.read(Clone::clone), committed);
        let other = restarted.local_command(Some(("client-b", "operation-1", &[1u8;32])),
            |_| Ok(vec![prompt("other-client")]), |_,_| serde_json::json!({"status":201})).unwrap();
        assert_eq!(other["status"], 201, "request identity is scoped to the approved client");
    }

    #[test]
    fn local_command_receipt_failure_rolls_back_events_and_can_retry() {
        let hub = hub("local-command-rollback");
        let before = hub.read(Clone::clone);
        let db = rusqlite::Connection::open(hub.home.join("alondra.db")).unwrap();
        db.execute_batch("CREATE TRIGGER fail_result BEFORE INSERT ON local_command_results BEGIN SELECT RAISE(ABORT,'injected receipt failure'); END;").unwrap();
        assert!(hub.local_command(Some(("client-a", "operation-1", &[1u8;32])),
            |_| Ok(vec![prompt("rolled-back")]), |_,_| serde_json::json!({"status":200})).is_err());
        assert_eq!(hub.read(Clone::clone), before);
        assert_eq!(Hub::open(hub.config.clone()).unwrap().read(Clone::clone), before);
        db.execute_batch("DROP TRIGGER fail_result;").unwrap();
        hub.local_command(Some(("client-a", "operation-1", &[1u8;32])),
            |_| Ok(vec![prompt("committed")]), |_,_| serde_json::json!({"status":200})).unwrap();
        let count: i64 = db.query_row("SELECT COUNT(*) FROM local_command_results", [], |r|r.get(0)).unwrap();
        assert_eq!(count, 1);
        assert!(hub.read(|v| v.nodes.contains_key("committed")));
        assert!(!hub.read(|v| v.nodes.contains_key("rolled-back")));
    }

    #[test]
    fn failed_second_write_rolls_back_both_disk_and_live_state() {
        let hub = hub("atomic-batch");
        let before = hub.read(Clone::clone);
        let db = rusqlite::Connection::open(hub.home.join("alondra.db")).unwrap();
        let count: i64 = db.query_row("SELECT COUNT(*) FROM events", [], |r| r.get(0)).unwrap();
        db.execute_batch("CREATE TRIGGER reject_second_hook BEFORE INSERT ON event_log WHEN NEW.session = 'second' BEGIN SELECT RAISE(ABORT, 'injected write failure'); END;").unwrap();
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
        let hub = Hub::open(config).unwrap();
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
