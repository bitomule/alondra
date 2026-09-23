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
            Ok((seq, body)) => Ok(Some((seq as u64, serde_json::from_str(&body).context("instantánea ilegible")?))),
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
        self.conn.execute_batch("PRAGMA wal_checkpoint(TRUNCATE); VACUUM;")?;
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
                Err(e) => eprintln!("alondrad: evento {seq} ilegible, se ignora: {e}"),
            }
            count += 1;
        }
        Ok(count)
    }

    pub fn session_timeline(&self, session: &str, limit: usize) -> Result<Vec<(u64, Millis, String)>> {
        let mut stmt = self.conn.prepare(
            "SELECT seq, at, body FROM events WHERE session = ?1 ORDER BY seq DESC LIMIT ?2",
        )?;
        let rows = stmt.query_map(params![session, limit as i64], |r| {
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
