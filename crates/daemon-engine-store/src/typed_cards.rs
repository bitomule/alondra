//! What Alondra's own cards are and which hand-backs say a merge failed, as stored columns. A
//! database written before they existed gets them once, read from the ids and bodies Alondra wrote
//! then; nothing reads those again afterwards.
use alondra_core::domain::{MergeFailure, SystemCard};
use rusqlite::{Transaction, params};

const REFUSED_CARDS_CAUSE: &str = "refused-cards:";

/// Adds the columns a database lacks and fills them from what it already holds.
pub(super) fn upgrade(tx: &Transaction<'_>) -> anyhow::Result<()> {
    if add_column(tx, "interactions", "system_card")? {
        mark_system_cards(tx)?;
    }
    if add_column(tx, "delivery_feedback", "merge_failure")? {
        mark_merge_failures(tx)?;
    }
    add_column(tx, "delivery_feedback", "target")?;
    Ok(())
}

/// True when the column was missing and is added now.
fn add_column(tx: &Transaction<'_>, table: &str, column: &str) -> rusqlite::Result<bool> {
    let present = tx.query_row(&format!("SELECT count(*) FROM pragma_table_info('{table}') WHERE name=?1"), [column], |row| row.get::<_, i64>(0))? == 1;
    if !present {
        tx.execute_batch(&format!("ALTER TABLE {table} ADD COLUMN {column} TEXT CHECK({column} IS NULL OR json_valid({column}))"))?;
    }
    Ok(!present)
}

fn mark_system_cards(tx: &Transaction<'_>) -> anyhow::Result<()> {
    let mut query = tx.prepare(
        "SELECT id,json_extract(card,'$.cause') FROM interactions WHERE author_kind='system' OR (author_kind='agent' AND id LIKE 'unheard-%')",
    )?;
    let cards: Vec<(String, Option<String>)> = query.query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?.collect::<rusqlite::Result<_>>()?;
    for (id, cause) in cards {
        let owner = cause.as_deref().and_then(|cause| cause.strip_prefix(REFUSED_CARDS_CAUSE));
        let card = match owner {
            Some(owner) => Some(SystemCard::RefusedCards { owner: owner.to_owned() }),
            None => SystemCard::of_stored_id(&id),
        };
        if let Some(card) = card {
            tx.execute("UPDATE interactions SET system_card=?1 WHERE id=?2", params![serde_json::to_string(&card)?, id])?;
        }
    }
    Ok(())
}

fn mark_merge_failures(tx: &Transaction<'_>) -> anyhow::Result<()> {
    let mut query = tx.prepare(
        "SELECT interaction_id,ordinal,body FROM delivery_feedback WHERE user_id='alondra' AND instruction='conflicts'",
    )?;
    let rows: Vec<(String, i64, String)> = query.query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))?.collect::<rusqlite::Result<_>>()?;
    for (interaction, ordinal, body) in rows {
        if let Some(failure) = MergeFailure::of_stored_body(&body) {
            tx.execute(
                "UPDATE delivery_feedback SET merge_failure=?1 WHERE interaction_id=?2 AND ordinal=?3",
                params![serde_json::to_string(&failure)?, interaction, ordinal],
            )?;
        }
    }
    Ok(())
}
