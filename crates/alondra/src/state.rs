//! Offline state audit and conservative recovery when the daemon cannot start.

use std::path::Path;

use alondra_core::{Event, View, now_millis};
use anyhow::{Context, Result, bail};
use rusqlite::{Connection, OpenFlags, OptionalExtension, params};

fn replay(conn: &Connection, snapshot: bool) -> Result<View> {
    let (after, mut view) = if snapshot {
        let stored: Option<(i64, String)> = conn.query_row(
            "SELECT seq, body FROM snapshots ORDER BY seq DESC LIMIT 1", [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        ).optional()?;
        match stored {
            Some((seq, body)) => (seq as u64, serde_json::from_str(&body).context("snapshot unreadable")?),
            None => (0, View::default()),
        }
    } else {
        (0, View::default())
    };
    view.backfill_created_at();
    view.backfill_tasks();
    let mut stmt = conn.prepare("SELECT seq, at, body FROM events WHERE seq > ?1 ORDER BY seq")?;
    let mut rows = stmt.query(params![after as i64])?;
    while let Some(row) = rows.next()? {
        let (seq, at, body): (i64, i64, String) = (row.get(0)?, row.get(1)?, row.get(2)?);
        let event: Event = serde_json::from_str(&body).with_context(|| format!("event {seq} unreadable"))?;
        view.apply(seq as u64, at, &event);
    }
    let issues = view.integrity_issues();
    if !issues.is_empty() {
        bail!("{}", issues.join("; "));
    }
    Ok(view)
}

pub fn audit(home: &Path) -> Result<(u64, usize)> {
    let db = home.join("alondra.db");
    let conn = Connection::open_with_flags(&db, OpenFlags::SQLITE_OPEN_READ_ONLY)
        .with_context(|| format!("opening {}", db.display()))?;
    let view = replay(&conn, true)?;
    Ok((view.seq, view.board.tasks.len()))
}

pub fn repair_from_events(home: &Path) -> Result<String> {
    let socket = home.join("hooks.sock");
    if std::os::unix::net::UnixStream::connect(&socket).is_ok() {
        bail!("stop alondrad before repairing its database");
    }
    let db = home.join("alondra.db");
    let mut conn = Connection::open(&db).with_context(|| format!("opening {}", db.display()))?;
    let (count, first, last): (i64, i64, i64) = conn.query_row(
        "SELECT COUNT(*), COALESCE(MIN(seq), 0), COALESCE(MAX(seq), 0) FROM events", [],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
    )?;
    if count == 0 || first != 1 || count != last {
        bail!("the event log is incomplete after compaction; restore a known-good backup instead");
    }
    // Before changing a byte, prove the complete log builds a valid view on its own.
    let full = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| replay(&conn, false)))
        .map_err(|_| anyhow::anyhow!("full replay panicked; database left unchanged"))??;
    let backup = home.join(format!("alondra.db.before-repair-{}", now_millis()));
    conn.execute("VACUUM INTO ?1", params![backup.to_string_lossy().as_ref()])
        .with_context(|| format!("backing up to {}", backup.display()))?;
    let tx = conn.transaction()?;
    tx.execute("DELETE FROM snapshots", [])?;
    tx.commit()?;
    Ok(format!("Backed up to {}. Rebuilt state from {} events and removed stale snapshots; restart alondrad.", backup.display(), full.seq))
}

#[cfg(test)]
mod tests {
    use super::*;
    use alondra_core::HookEvent;

    #[test]
    fn corrupt_snapshot_can_be_replaced_from_complete_log_after_backup() {
        let home = std::env::temp_dir().join(format!("alondra-state-repair-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&home);
        std::fs::create_dir_all(&home).unwrap();
        let db = home.join("alondra.db");
        let conn = Connection::open(&db).unwrap();
        conn.execute_batch("CREATE TABLE events (seq INTEGER PRIMARY KEY, at INTEGER, body TEXT); CREATE TABLE snapshots (seq INTEGER PRIMARY KEY, body TEXT);").unwrap();
        let event = Event::Hook(HookEvent { session_id: "s".into(), event: "SessionStart".into(), ..Default::default() });
        conn.execute("INSERT INTO events VALUES (1, 1, ?1)", params![serde_json::to_string(&event).unwrap()]).unwrap();
        conn.execute("INSERT INTO snapshots VALUES (1, 'bad json')", []).unwrap();
        drop(conn);
        assert!(audit(&home).is_err());
        let result = repair_from_events(&home).unwrap();
        assert!(result.contains("Backed up to"));
        assert_eq!(audit(&home).unwrap().0, 1);
        assert!(std::fs::read_dir(&home).unwrap().any(|e| e.unwrap().file_name().to_string_lossy().contains("before-repair")));
        std::fs::remove_dir_all(home).unwrap();
    }

    #[test]
    fn repair_refuses_a_log_pruned_below_the_snapshot() {
        let home = std::env::temp_dir().join(format!("alondra-state-gap-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&home);
        std::fs::create_dir_all(&home).unwrap();
        let conn = Connection::open(home.join("alondra.db")).unwrap();
        conn.execute_batch("CREATE TABLE events (seq INTEGER PRIMARY KEY, at INTEGER, body TEXT); CREATE TABLE snapshots (seq INTEGER PRIMARY KEY, body TEXT); INSERT INTO events VALUES (2, 2, '{}');").unwrap();
        drop(conn);
        assert!(repair_from_events(&home).unwrap_err().to_string().contains("incomplete"));
        std::fs::remove_dir_all(home).unwrap();
    }
}
