use crate::{Error, Result};
use rusqlite::{Connection, OptionalExtension};

const FORMAT: &str = "alondra-task-resources-v1";
const MARKER_SCHEMA: &str = "CREATE TABLE lifecycle_journal_format(singleton INTEGER PRIMARY KEY CHECK(singleton=1),format TEXT NOT NULL)";
const SCHEMA: &str = include_str!("schema.sql");
/// Tables a journal of this format gained after it was first installed: a journal that has none of
/// them gets them all, and one that has only some is refused like any other changed schema.
const ADDITIONS: &str = include_str!("controls.sql");
/// The definitions of the addition tables whose checks later controls widened, as journals
/// installed before each of them still hold them: before the MCP controls, before force stop, and
/// before clearing an MCP sign-in.
const EARLIER: [&str; 3] = [
    include_str!("controls_before_mcp.sql"),
    include_str!("controls_before_force_stop.sql"),
    include_str!("controls_before_clear_auth.sql"),
];
/// Earlier guards kept all observation and input bytes, and allowed only the user's required
/// cards to change. Only these exact definitions may be replaced on an existing journal.
const EARLIER_TRIGGERS: [&str; 3] = [
    include_str!("observations_before_retention.sql"),
    include_str!("inputs_and_commands_before_compaction.sql"),
    include_str!("cards_before_orchestrator.sql"),
];

pub(super) fn install(connection: &Connection) -> Result<()> {
    let existing: bool = connection.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type IN('table','view') AND name GLOB 'lifecycle_*')",
        [], |row| row.get(0),
    )?;
    if existing {
        extend(connection)?;
        return verify_schema(connection);
    }
    connection.execute_batch(SCHEMA)?;
    connection.execute_batch(ADDITIONS)?;
    receipts(connection, true)?;
    context(connection, true)?;
    connection.execute_batch(MARKER_SCHEMA)?;
    connection.execute(
        "INSERT INTO lifecycle_journal_format(singleton,format) VALUES(1,?1)",
        [FORMAT],
    )?;
    verify_schema(connection)
}

pub(super) fn verify(connection: &Connection) -> Result<()> {
    verify_schema(connection)
}
/// Whether the connection holds a journal of this format, after giving it the additions it lacks.
pub(super) fn extend(connection: &Connection) -> Result<bool> {
    let marked: bool = connection.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='lifecycle_journal_format' AND sql=?1)",
        [MARKER_SCHEMA], |row| row.get(0),
    )?;
    if !marked {
        return Ok(false);
    }
    let format: Option<String> = connection
        .query_row("SELECT format FROM lifecycle_journal_format WHERE singleton=1", [], |row| row.get(0))
        .optional()?;
    if format.as_deref() != Some(FORMAT) {
        return Ok(false);
    }
    receipts(connection, true)?;
    upgrade_triggers(connection)?;
    for (_, name, _) in objects(ADDITIONS)? {
        let present: bool = connection.query_row("SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE name=?1)", [name], |row| row.get(0))?;
        if present {
            widen(connection)?;
            verify_schema(connection)?;
            context(connection,true)?;
            return Ok(true);
        }
    }
    connection.execute_batch(ADDITIONS)?;
    verify_schema(connection)?;
    context(connection,true)?;
    Ok(true)
}
/// Rebuilds, with every row, each addition table that still has an earlier definition, and brings
/// back the triggers dropped with it. Its rows already satisfy the wider checks; the references to
/// it are checked when the caller commits.
fn widen(connection: &Connection) -> Result<()> {
    let current = objects(ADDITIONS)?;
    let mut rebuilt = false;
    for (_, name, previous) in EARLIER.iter().map(|earlier| objects(earlier)).collect::<Result<Vec<_>>>()?.into_iter().flatten() {
        let held: Option<String> = connection
            .query_row("SELECT sql FROM sqlite_master WHERE type='table' AND name=?1", [name], |row| row.get(0))
            .optional()?;
        if held.as_deref() != Some(previous.as_str()) {
            continue;
        }
        let (_, _, definition) = current.iter().find(|(_, current, _)| *current == name).ok_or(Error::UnsupportedJournalFormat)?;
        connection.execute_batch(&format!(
            "PRAGMA defer_foreign_keys=ON;
             CREATE TEMP TABLE widening AS SELECT * FROM main.{name};
             DROP TABLE main.{name};
             {definition};
             INSERT INTO main.{name} SELECT * FROM temp.widening;
             DROP TABLE temp.widening;"
        ))?;
        rebuilt = true;
    }
    if rebuilt {
        connection.execute_batch(ADDITIONS)?;
    }
    Ok(())
}
/// Replaces known earlier trigger definitions without changing the tables or their rows.
fn upgrade_triggers(connection: &Connection) -> Result<()> {
    let current = objects(SCHEMA)?;
    for (_, name, previous) in EARLIER_TRIGGERS.iter().map(|before| objects(before)).collect::<Result<Vec<_>>>()?.into_iter().flatten() {
        let held: Option<String> = connection
            .query_row("SELECT sql FROM sqlite_master WHERE type='trigger' AND name=?1", [name], |row| row.get(0))
            .optional()?;
        if held.as_deref() != Some(previous.as_str()) {
            continue;
        }
        let (_, _, definition) = current.iter().find(|(_, current, _)| *current == name).ok_or(Error::UnsupportedJournalFormat)?;
        connection.execute_batch(&format!("DROP TRIGGER main.{name}; {definition};"))?;
    }
    Ok(())
}
fn verify_schema(connection: &Connection) -> Result<()> {
    crate::measure::schema_verified();
    context(connection, false)?;
    receipts(connection, false)?;
    require_object(
        connection,
        "table",
        "lifecycle_journal_format",
        MARKER_SCHEMA,
    )?;
    verify_marker(connection)?;
    for source in [SCHEMA, ADDITIONS] {
        for (kind, name, expected) in objects(source)? {
            require_object(connection, kind, name, &expected)?;
        }
    }
    Ok(())
}

/// The marker's value can change without changing SQLite's schema cookie.
pub(super) fn verify_marker(connection: &Connection) -> Result<()> {
    let format: Option<String> = connection
        .query_row(
            "SELECT format FROM lifecycle_journal_format WHERE singleton=1",
            [],
            |row| row.get(0),
        )
        .optional()?;
    if format.as_deref() != Some(FORMAT) {
        return Err(Error::UnsupportedJournalFormat);
    }
    Ok(())
}
fn receipts(connection: &Connection, install: bool) -> Result<()> {
    let expected = objects(crate::receipt_proofs::SCHEMA)?;
    let mut present = false;
    for (_, name, _) in &expected {
        crate::measure::schema_queried();
        present |= connection.query_row("SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE name=?1)", [name], |row| row.get::<_, bool>(0))?;
    }
    if !present && install { connection.execute_batch(crate::receipt_proofs::SCHEMA)?; }
    for (kind, name, sql) in expected { require_object(connection, kind, name, &sql)?; }
    Ok(())
}
fn context(connection: &Connection, install: bool) -> Result<()> {
    let objects = objects(crate::task_context::SCHEMA)?;
    let mut present = false;
    for (_, name, _) in &objects {
        crate::measure::schema_queried();
        present |= connection.query_row("SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE name=?1)", [name], |row| row.get::<_, bool>(0))?;
    }
    if !present && !install { return Ok(()); }
    if install {
        connection.execute_batch(crate::task_context::SCHEMA)?;
        if !present { connection.execute_batch(crate::task_context::LEGACY_COVERAGE)?; }
    }
    for (kind, name, expected) in objects { require_object(connection, kind, name, &expected)?; }
    if install { connection.execute_batch(crate::task_context::USAGE_BACKFILL)?; }
    Ok(())
}
/// The owned schema source delimits each CREATE at a line boundary; trigger bodies contain
/// semicolons, so statement splitting on semicolons would lose their guards.
fn objects(source: &str) -> Result<Vec<(&'static str, &str, String)>> {
    let mut objects = vec![];
    for (index, part) in source.split("\nCREATE ").enumerate() {
        let body = if index == 0 {
            part.strip_prefix("CREATE ")
                .ok_or(Error::UnsupportedJournalFormat)?
        } else {
            part
        };
        let (kind, remaining) = if let Some(value) = body.strip_prefix("TABLE IF NOT EXISTS ") {
            ("table", value)
        } else if let Some(value) = body.strip_prefix("VIEW IF NOT EXISTS ") {
            ("view", value)
        } else if let Some(value) = body.strip_prefix("UNIQUE INDEX IF NOT EXISTS ") {
            ("index", value)
        } else if let Some(value) = body.strip_prefix("TRIGGER IF NOT EXISTS ") {
            ("trigger", value)
        } else {
            return Err(Error::UnsupportedJournalFormat);
        };
        let name = remaining
            .split(|c: char| c.is_whitespace() || c == '(')
            .next()
            .ok_or(Error::UnsupportedJournalFormat)?;
        let expected = format!("CREATE {}", body.replace("IF NOT EXISTS ", ""));
        objects.push((kind, name, expected.trim().trim_end_matches(';').to_owned()));
    }
    Ok(objects)
}
fn require_object(connection: &Connection, kind: &str, name: &str, expected: &str) -> Result<()> {
    crate::measure::schema_queried();
    let object: Option<(String, Option<String>)> = connection
        .query_row(
            "SELECT type,sql FROM sqlite_master WHERE name=?1",
            [name],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;
    if object
        .as_ref()
        .is_none_or(|(actual, sql)| actual != kind || sql.as_deref() != Some(expected))
    {
        return Err(Error::UnsupportedJournalFormat);
    }
    Ok(())
}
