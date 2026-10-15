use crate::{Error, Result};
use rusqlite::{Connection, OptionalExtension};

const FORMAT: &str = "alondra-task-resources-v1";
const MARKER_SCHEMA: &str = "CREATE TABLE lifecycle_journal_format(singleton INTEGER PRIMARY KEY CHECK(singleton=1),format TEXT NOT NULL)";

pub(super) fn install(connection: &Connection) -> Result<()> {
    let existing: bool = connection.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type IN('table','view') AND name GLOB 'lifecycle_*')",
        [], |row| row.get(0),
    )?;
    if existing {
        return verify_schema(connection);
    }
    connection.execute_batch(include_str!("schema.sql"))?;
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
fn verify_schema(connection: &Connection) -> Result<()> {
    require_object(
        connection,
        "table",
        "lifecycle_journal_format",
        MARKER_SCHEMA,
    )?;
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
    // The owned schema source delimits each CREATE at a line boundary; trigger bodies
    // contain semicolons, so statement splitting on semicolons would lose their guards.
    for (index, part) in include_str!("schema.sql").split("\nCREATE ").enumerate() {
        let body = if index == 0 {
            part.strip_prefix("CREATE ")
                .ok_or(Error::UnsupportedJournalFormat)?
        } else {
            part
        };
        let (kind, remaining) = if let Some(value) = body.strip_prefix("TABLE IF NOT EXISTS ") {
            ("table", value)
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
        require_object(
            connection,
            kind,
            name,
            expected.trim().trim_end_matches(';'),
        )?;
    }
    Ok(())
}
fn require_object(connection: &Connection, kind: &str, name: &str, expected: &str) -> Result<()> {
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
