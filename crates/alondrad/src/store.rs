use std::path::Path;

use alondra_core::{Event, Millis};
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
            CREATE INDEX IF NOT EXISTS events_session ON events(session);",
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

    pub fn replay(&self, mut f: impl FnMut(u64, Millis, Event)) -> Result<u64> {
        let mut stmt = self.conn.prepare("SELECT seq, at, body FROM events ORDER BY seq")?;
        let mut rows = stmt.query([])?;
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
        s.replay(|q, at, e| seen.push((q, at, e))).unwrap();
        assert_eq!(seen, vec![(seq, 5, ev)]);
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
