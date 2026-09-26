use std::path::Path;

use alondra_core::{Event, Millis, View};
use anyhow::{Context, Result};
use rusqlite::{Connection, OptionalExtension, Transaction, params};

pub struct Store {
    conn: Connection,
}

impl Store {
    pub fn open(path: &Path) -> Result<Self> {
        let conn = Connection::open(path).with_context(|| format!("abriendo {}", path.display()))?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "synchronous", "FULL")?;
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS events (
                seq INTEGER PRIMARY KEY,
                at INTEGER NOT NULL,
                kind TEXT NOT NULL,
                session TEXT,
                body TEXT NOT NULL
            );
            CREATE INDEX IF NOT EXISTS events_session ON events(session);
            CREATE TABLE IF NOT EXISTS local_command_results (
                sender TEXT NOT NULL, request_id TEXT NOT NULL, digest BLOB NOT NULL,
                result TEXT NOT NULL, PRIMARY KEY(sender, request_id)
            );
            CREATE TABLE IF NOT EXISTS snapshots (
                seq INTEGER PRIMARY KEY,
                at INTEGER NOT NULL,
                body TEXT NOT NULL
            );",
        )?;
        Ok(Store { conn })
    }

    pub(crate) fn command_result(tx: &Transaction<'_>, sender: &str, id: &str, digest: &[u8; 32]) -> Result<Option<serde_json::Value>> {
        let stored: Option<(Vec<u8>, String)> = tx.query_row(
            "SELECT digest,result FROM local_command_results WHERE sender=?1 AND request_id=?2",
            params![sender, id], |r| Ok((r.get(0)?, r.get(1)?))).optional()?;
        stored.map(|(previous, result)| {
            anyhow::ensure!(previous == digest, "request ID reused with different encrypted bytes");
            Ok(serde_json::from_str(&result)?)
        }).transpose()
    }

    pub(crate) fn save_command_result(tx: &Transaction<'_>, sender: &str, id: &str, digest: &[u8; 32], result: &serde_json::Value) -> Result<()> {
        tx.execute("INSERT INTO local_command_results(sender,request_id,digest,result) VALUES(?1,?2,?3,?4)",
            params![sender, id, digest.as_slice(), serde_json::to_string(result)?])?;
        Ok(())
    }

    pub fn append(&mut self, at: Millis, event: &Event) -> Result<u64> {
        Self::insert(&self.conn, at, event)
    }

    fn insert(conn: &Connection, at: Millis, event: &Event) -> Result<u64> {
        let body = serde_json::to_string(event)?;
        // Compaction can delete every event while keeping a snapshot. SQLite's implicit
        // rowid would then restart at 1, below that snapshot, and replay would miss writes.
        conn.execute(
            "INSERT INTO events (seq, at, kind, session, body)
             VALUES ((SELECT MAX(seq) + 1 FROM (
                 SELECT COALESCE(MAX(seq), 0) AS seq FROM events
                 UNION ALL SELECT COALESCE(MAX(seq), 0) FROM snapshots
             )), ?1, ?2, ?3, ?4)",
            params![at, event.kind(), event.session_id(), body],
        )?;
        Ok(conn.last_insert_rowid() as u64)
    }

    /// A source event and every task event it causes are one durable fact. A failed closure or
    /// commit rolls the whole batch back, including any earlier events in the same API call.
    pub fn transaction<T>(&mut self, f: impl FnOnce(&Transaction<'_>) -> Result<T>) -> Result<T> {
        let tx = self.conn.transaction()?;
        let result = f(&tx)?;
        tx.commit()?;
        Ok(result)
    }

    pub fn append_in(tx: &Transaction<'_>, at: Millis, event: &Event) -> Result<u64> {
        Self::insert(tx, at, event)
    }

    pub fn latest_snapshot(&self) -> Result<Option<(u64, View)>> {
        let row = self.conn.query_row("SELECT seq, body FROM snapshots ORDER BY seq DESC LIMIT 1", [], |r| {
            Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?))
        });
        match row {
            Ok((seq, body)) => Ok(Some((seq as u64, serde_json::from_str(&body).context("unreadable snapshot")?))),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    pub fn compact(&mut self, view: &View, at: Millis, before: Millis) -> Result<usize> {
        let body = serde_json::to_string(view)?;
        let tx = self.conn.transaction()?;
        tx.execute("INSERT OR REPLACE INTO snapshots (seq, at, body) VALUES (?1, ?2, ?3)", params![view.seq as i64, at, body])?;
        tx.execute("DELETE FROM snapshots WHERE seq < ?1", params![view.seq as i64])?;
        let deleted = tx.execute("DELETE FROM events WHERE seq <= ?1 AND at < ?2", params![view.seq as i64, before])?;
        tx.commit()?;
        self.conn.execute_batch("VACUUM; PRAGMA wal_checkpoint(TRUNCATE);")?;
        Ok(deleted)
    }

    pub fn replay(&self, after: u64, mut f: impl FnMut(u64, Millis, Event)) -> Result<u64> {
        let mut stmt = self.conn.prepare("SELECT seq, at, body FROM events WHERE seq > ?1 ORDER BY seq")?;
        let mut rows = stmt.query(params![after as i64])?;
        let mut count = 0;
        while let Some(row) = rows.next()? {
            let seq: i64 = row.get(0)?;
            let at: i64 = row.get(1)?;
            let body: String = row.get(2)?;
            let ev = serde_json::from_str::<Event>(&body)
                .with_context(|| format!("event {seq} is unreadable; replay stopped before state can diverge"))?;
            f(seq as u64, at, ev);
            count += 1;
        }
        Ok(count)
    }

    /// The session's newest `limit` events before `before` (all of them when it is `None`), newest first.
    pub fn session_timeline(&self, session: &str, limit: usize, before: Option<u64>) -> Result<Vec<(u64, Millis, String)>> {
        let mut stmt = self.conn.prepare(
            "SELECT seq, at, body FROM events WHERE session = ?1 AND seq < ?3 ORDER BY seq DESC LIMIT ?2",
        )?;
        let before = before.map_or(i64::MAX, |b| b as i64);
        let rows = stmt.query_map(params![session, limit as i64, before], |r| {
            Ok((r.get::<_, i64>(0)? as u64, r.get::<_, i64>(1)?, r.get::<_, String>(2)?))
        })?;
        Ok(rows.filter_map(Result::ok).collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alondra_core::{HookEvent, states, upkeep};
    use std::collections::BTreeSet;

    #[test]
    fn append_then_replay_rebuilds_same_events() {
        let dir = std::env::temp_dir().join(format!("alondra-store-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("t.db");
        let _ = std::fs::remove_file(&path);
        let mut s = Store::open(&path).unwrap();
        let ev = Event::Hook(HookEvent { session_id: "s".into(), event: "Stop".into(), ..Default::default() });
        let seq = s.append(5, &ev).unwrap();
        let mut seen = vec![];
        s.replay(0, |q, at, e| seen.push((q, at, e))).unwrap();
        assert_eq!(seen, vec![(seq, 5, ev)]);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn sequence_stays_above_snapshot_after_all_events_are_compacted() {
        let dir = std::env::temp_dir().join(format!("alondra-sequence-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("t.db");
        let _ = std::fs::remove_file(&path);
        let mut s = Store::open(&path).unwrap();
        let ev = Event::Hook(HookEvent { session_id: "s".into(), event: "Stop".into(), ..Default::default() });
        let mut view = View::default();
        for at in 1..=3 {
            let seq = s.append(at, &ev).unwrap();
            view.apply(seq, at, &ev);
        }
        let snapshot = view.seq;
        assert_eq!(s.compact(&view, 100, 100).unwrap(), 3);
        drop(s);
        let mut s = Store::open(&path).unwrap();
        let seq = s.append(101, &ev).unwrap();
        assert!(seq > snapshot);
        let mut seen = vec![];
        s.replay(snapshot, |seq, _, _| seen.push(seq)).unwrap();
        assert_eq!(seen, vec![seq]);
        drop(s);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn failed_batch_does_not_leave_a_source_event_without_its_task_move() {
        let dir = std::env::temp_dir().join(format!("alondra-rollback-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("t.db");
        let _ = std::fs::remove_file(&path);
        let mut s = Store::open(&path).unwrap();
        let ev = Event::Hook(HookEvent { session_id: "s".into(), event: "Stop".into(), ..Default::default() });
        let result: Result<()> = s.transaction(|tx| {
            Store::append_in(tx, 1, &ev)?;
            anyhow::bail!("simulated derived event failure")
        });
        assert!(result.is_err());
        assert_eq!(s.replay(0, |_, _, _| {}).unwrap(), 0);
        drop(s);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_session_timeline_pages_back_from_a_sequence_number() {
        let dir = std::env::temp_dir().join(format!("alondra-timeline-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("t.db");
        let _ = std::fs::remove_file(&path);
        let mut s = Store::open(&path).unwrap();
        let hook = |session: &str| Event::Hook(HookEvent { session_id: session.into(), event: "Stop".into(), ..Default::default() });
        let mut mine = vec![];
        for i in 0..5 {
            mine.push(s.append(i, &hook("s")).unwrap());
            s.append(i, &hook("other")).unwrap();
        }
        let seqs = |limit, before| s.session_timeline("s", limit, before).unwrap().into_iter().map(|(seq, _, _)| seq).collect::<Vec<_>>();
        assert_eq!(seqs(2, None), vec![mine[4], mine[3]], "newest first");
        assert_eq!(seqs(2, Some(mine[3])), vec![mine[2], mine[1]]);
        assert_eq!(seqs(10, Some(mine[1])), vec![mine[0]]);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn compaction_leaves_the_write_ahead_log_empty() {
        let dir = std::env::temp_dir().join(format!("alondra-wal-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("t.db");
        let _ = std::fs::remove_file(&path);
        let mut s = Store::open(&path).unwrap();
        let mut view = View::default();
        for i in 0..2_000 {
            let ev = Event::Hook(HookEvent { session_id: format!("s{i}"), event: "Stop".into(), last_message: Some("x".repeat(500)), ..Default::default() });
            let seq = s.append(i, &ev).unwrap();
            view.apply(seq, i, &ev);
        }
        s.compact(&view, 10_000, 5_000).unwrap();
        let wal = std::fs::metadata(dir.join("t.db-wal")).map_or(0, |m| m.len());
        assert_eq!(wal, 0, "WAL still holds {wal} bytes after compaction");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    fn summary(view: &View) -> String {
        let mut out = String::new();
        let mut states: std::collections::BTreeMap<&str, usize> = std::collections::BTreeMap::new();
        for t in view.board.tasks.values() {
            *states.entry(t.state.name()).or_default() += 1;
        }
        let requests = view.board.requests.keys().map(|id| format!("{:?}", view.board.request_state(id)));
        let mut by_state: std::collections::BTreeMap<String, usize> = std::collections::BTreeMap::new();
        for r in requests {
            *by_state.entry(r).or_default() += 1;
        }
        out += &format!("tasks: {} {states:?}\nrequests: {} {by_state:?}\n", view.board.tasks.len(), view.board.requests.len());
        out += &format!("anomalies: {} {:?}\n", view.board.anomalies.values().sum::<u64>(), view.board.anomalies);
        let mut first: Vec<(&str, &alondra_core::tasks::Anomaly)> = view.board.tasks.values().flat_map(|t| t.anomalies.iter().map(move |a| (t.title.as_str(), a))).collect();
        first.sort_by_key(|(_, a)| a.at);
        for (title, a) in first.iter().take(10) {
            out += &format!("  {} {title}: {} + {} — {}\n", a.at, a.state.name(), serde_json::to_string(&a.event).unwrap_or_default(), a.reason);
        }
        let mut moves: std::collections::BTreeMap<String, usize> = std::collections::BTreeMap::new();
        for h in view.board.tasks.values().flat_map(|t| &t.history) {
            *moves.entry(format!("{}->{} {}", h.from.name(), h.to.name(), h.event.name())).or_default() += 1;
        }
        out += &format!("transitions: {moves:#?}\n");
        let history: usize = view.board.tasks.values().map(|t| serde_json::to_string(&t.history).map_or(0, |s| s.len())).sum();
        let briefs: usize = view.board.tasks.values().map(|t| t.brief.as_ref().map_or(0, String::len)).sum();
        out += &format!("board JSON: {} bytes ({history} of history, {briefs} of briefs)\n", serde_json::to_string(&view.board).map_or(0, |s| s.len()));
        out
    }

    /// A node shows done while a task of it is open, waits for a review nobody has to give, is
    /// stalled with nothing to move, or shows a delivered result over a task in review or finished.
    fn contradicts(v: &View, n: &alondra_core::Node, now: Millis) -> bool {
        use alondra_core::NodeState;
        use alondra_core::tasks::TaskState;
        let tasks: Vec<_> = v.tasks_of(&n.session_id).collect();
        let any = |states: &[TaskState]| tasks.iter().any(|t| states.contains(&t.state));
        let finished = !tasks.is_empty() && tasks.iter().all(|t| t.state.finished());
        match n.state {
            NodeState::Done => !tasks.is_empty() && !finished,
            NodeState::WaitingMerge => {
                let due = v.merge_notice_due(&n.session_id).is_some() || v.review_reply_due(&n.session_id).is_some();
                let waits = tasks.iter().any(|t| v.auto_merge_pending(t, now)) || states::awaits_merge(v, n, now).is_some();
                !due && !any(&[TaskState::InReview, TaskState::WaitingDependency]) && !waits
            }
            NodeState::Stalled => n.evidence.as_ref().is_none_or(|e| e.signal != alondra_core::Signal::CallStuck) && !any(&[TaskState::Running, TaskState::Closing]),
            NodeState::Delivered => any(&[TaskState::InReview]) || finished,
            _ => false,
        }
    }

    /// Replays a copy of the real log and prints the tasks it gives. Never point it at the live
    /// file: `cp ~/.alondra/alondra.db* /tmp/` and
    /// `ALONDRA_REPLAY_DB=/tmp/alondra.db cargo test --release -p alondrad replay_a_copy_of_the_real_log -- --ignored --nocapture`.
    /// In release, as the daemon runs: the log of 22-24 Sep trips the debug-only limbo check of
    /// `View::apply` on two nodes, with or without tasks.
    #[test]
    #[ignore]
    fn replay_a_copy_of_the_real_log() {
        let path = std::env::var("ALONDRA_REPLAY_DB").expect("ALONDRA_REPLAY_DB names a copy of alondra.db");
        let s = Store::open(Path::new(&path)).unwrap();
        let first: i64 = s.conn.query_row("SELECT COALESCE(MIN(seq), 0) FROM events", [], |r| r.get(0)).unwrap();
        let mut sessions = std::collections::BTreeSet::new();
        let mut full = View::default();
        let (mut idle, mut resumes, mut closable, mut threads) = (BTreeSet::new(), BTreeSet::new(), BTreeSet::new(), 0usize);
        let (mut shown, mut entered) = (std::collections::HashMap::new(), std::collections::BTreeMap::<&str, usize>::new());
        let mut contradicted = std::collections::BTreeMap::<&str, usize>::new();
        let two_hours = 2 * 3600 * 1000;
        let last_at: i64 = s.conn.query_row("SELECT COALESCE(MAX(at), 0) FROM events", [], |r| r.get(0)).unwrap();
        let hour_from = last_at - 3600 * 1000;
        let (mut hour_events, mut hour_hooks, mut batches, mut bumps) = (0usize, 0usize, 0usize, 0usize);
        let (mut batch_at, mut digest): (Option<i64>, Option<u64>) = (None, None);
        let n = s.replay(0, |seq, at, e| {
            if let Some(sid) = e.session_id() {
                sessions.insert(sid.to_string());
            }
            if at >= hour_from {
                match batch_at {
                    None => digest = Some(crate::http::overview_digest(&full, at)),
                    Some(b) if b != at => {
                        batches += 1;
                        let now = crate::http::overview_digest(&full, b);
                        bumps += usize::from(digest != Some(now));
                        digest = Some(now);
                    }
                    Some(_) => {}
                }
                batch_at = Some(at);
                hour_events += 1;
                hour_hooks += usize::from(matches!(e, Event::Hook(_)));
            }
            full.apply(seq, at, &e);
            let touched: Vec<String> = e.session_id().map_or_else(|| full.nodes.keys().cloned().collect(), |s| vec![s.to_string()]);
            for n in touched.iter().filter_map(|sid| full.nodes.get(sid)) {
                if shown.insert(n.session_id.clone(), n.state) != Some(n.state) {
                    *entered.entry(n.state.label()).or_default() += 1;
                }
                if contradicts(&full, n, at) {
                    *contradicted.entry(n.state.label()).or_default() += 1;
                }
                if states::idle_move(&full, n, at).is_some() {
                    idle.insert((n.session_id.clone(), n.episode_since));
                }
                if upkeep::resume_due(&full, n, at).is_some() {
                    resumes.insert((n.session_id.clone(), n.exited_at));
                }
                if upkeep::free_to_close(&full, n, at, two_hours) {
                    closable.insert(n.session_id.clone());
                }
                threads = threads.max(n.ledger.threads.len());
            }
        }).unwrap();
        if let Some(b) = batch_at {
            batches += 1;
            bumps += usize::from(digest != Some(crate::http::overview_digest(&full, b)));
        }
        println!(
            "last hour: {hour_events} events ({hour_hooks} hooks) in {batches} appends; every append moved the clients' tick before, {bumps} move it now (machine samples: 360 before, only 5 % steps now)"
        );
        println!("whole log: {n} events from seq {first}, {} sessions, {} nodes, {} removed", sessions.len(), full.nodes.len(), full.removed.len());
        println!("{}", summary(&full));
        let mut last = std::collections::BTreeMap::<&str, usize>::new();
        for n in full.nodes.values() {
            *last.entry(n.state.label()).or_default() += 1;
        }
        let statuses: Vec<String> = full.nodes.values().map(|n| format!("{:?}", n.status)).collect();
        println!("states entered: {entered:?}\nstates at the end: {last:?}\nstatus at the end: {statuses:?}");
        println!("events after which a node showed a state its tasks contradict: {contradicted:?}");
        println!(
            "episodes idle with a task to move: {}\nprocesses Alondra would restart: {} {resumes:?}\nnodes free for 2 h it could close: {}\nmost threads in one node: {threads}",
            idle.len(),
            resumes.len(),
            closable.len()
        );
        if let Some((after, mut resumed)) = s.latest_snapshot().unwrap() {
            resumed.backfill_created_at();
            resumed.backfill_tasks();
            let n = s.replay(after, |seq, at, e| resumed.apply(seq, at, &e)).unwrap();
            println!("from the snapshot at seq {after} and {n} events after it:\n{}", summary(&resumed));
        }
    }

    #[test]
    fn compaction_keeps_what_replay_needs_and_drops_old_events() {
        let dir = std::env::temp_dir().join(format!("alondra-compact-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("t.db");
        let _ = std::fs::remove_file(&path);
        let mut s = Store::open(&path).unwrap();
        let mut view = View::default();
        for (at, name) in [(1_000, "a"), (2_000, "b"), (9_000, "c")] {
            let ev = Event::Hook(HookEvent { session_id: name.into(), event: "Stop".into(), ..Default::default() });
            let seq = s.append(at, &ev).unwrap();
            view.apply(seq, at, &ev);
        }
        let deleted = s.compact(&view, 10_000, 5_000).unwrap();
        assert_eq!(deleted, 2);
        let late = Event::Hook(HookEvent { session_id: "d".into(), event: "Stop".into(), ..Default::default() });
        let seq = s.append(11_000, &late).unwrap();
        view.apply(seq, 11_000, &late);
        drop(s);

        let s = Store::open(&path).unwrap();
        let (after, mut rebuilt) = s.latest_snapshot().unwrap().unwrap();
        s.replay(after, |q, at, e| rebuilt.apply(q, at, &e)).unwrap();
        assert_eq!(rebuilt, view);
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
