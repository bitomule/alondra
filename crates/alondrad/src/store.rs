use std::path::Path;

use alondra_core::{Event, Millis, View};
use anyhow::{Context, Result};
use rusqlite::{Connection, params};

pub struct Store {
    conn: Connection,
}

impl Store {
    pub fn open(path: &Path) -> Result<Self> {
        let conn = Connection::open(path).with_context(|| format!("abriendo {}", path.display()))?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "synchronous", "NORMAL")?;
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS events (
                seq INTEGER PRIMARY KEY,
                at INTEGER NOT NULL,
                kind TEXT NOT NULL,
                session TEXT,
                body TEXT NOT NULL
            );
            CREATE INDEX IF NOT EXISTS events_session ON events(session);
            CREATE TABLE IF NOT EXISTS snapshots (
                seq INTEGER PRIMARY KEY,
                at INTEGER NOT NULL,
                body TEXT NOT NULL
            );",
        )?;
        Ok(Store { conn })
    }

    pub fn append(&mut self, at: Millis, event: &Event) -> Result<u64> {
        let body = serde_json::to_string(event)?;
        self.conn.execute(
            "INSERT INTO events (at, kind, session, body) VALUES (?1, ?2, ?3, ?4)",
            params![at, event.kind(), event.session_id(), body],
        )?;
        Ok(self.conn.last_insert_rowid() as u64)
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
            match serde_json::from_str::<Event>(&body) {
                Ok(ev) => f(seq as u64, at, ev),
                Err(e) => log!("event {seq} unreadable, ignored: {e}"),
            }
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
    use alondra_core::HookEvent;

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
        let n = s.replay(0, |seq, at, e| {
            if let Some(sid) = e.session_id() {
                sessions.insert(sid.to_string());
            }
            full.apply(seq, at, &e);
        }).unwrap();
        println!("whole log: {n} events from seq {first}, {} sessions, {} nodes, {} removed", sessions.len(), full.nodes.len(), full.removed.len());        println!("{}", summary(&full));
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
