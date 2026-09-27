use super::{DomainStore, check_schema, repository};
use anyhow::{Result, ensure};
use rusqlite::Connection;
use std::io::{self, Write};

const MAX_ENTITIES: i64 = 16_384;
const MAX_DOCUMENT_BYTES: usize = 8 * 1024 * 1024;
const DOMAIN_TABLES: &[&str] = &[
    "requests",
    "agents",
    "tasks",
    "task_dependencies",
    "documents",
    "document_revisions",
    "task_documents",
    "pull_requests",
    "pull_request_reviews",
    "interactions",
    "interaction_reads",
    "interaction_subjects",
    "delivery_targets",
    "delivery_feedback",
    "document_reviews",
    "document_decisions",
    "pr_decisions",
    "merge_authorizations",
    "task_stacks",
    "stack_readiness",
    "interaction_attachments",
];
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ProjectionLimit {
    Entities,
    Bytes,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct ProjectionTooLarge {
    pub reason: ProjectionLimit,
}
impl std::fmt::Display for ProjectionTooLarge {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "domain projection too large: {:?}", self.reason)
    }
}
impl std::error::Error for ProjectionTooLarge {}
fn count_entities(c: &Connection) -> Result<i64> {
    // Fixed identifiers only. Check before DomainState/projection allocation.
    let query = format!(
        "SELECT {}",
        DOMAIN_TABLES
            .iter()
            .map(|table| format!("(SELECT count(*) FROM {table})"))
            .collect::<Vec<_>>()
            .join(" + ")
    );
    Ok(c.query_row(&query, [], |r| r.get(0))?)
}
/// Fixed table/column allowlist; SQLite `length(TEXT)` counts characters, so cast to
/// BLOB to bound actual UTF-8 bytes allocated by repository::load.
const VARIABLE_TEXT: &[(&str, &[&str])] = &[
    ("requests", &["id", "body"]),
    ("agents", &["id", "name"]),
    (
        "tasks",
        &[
            "id",
            "request_id",
            "agent_id",
            "title",
            "final_presentation",
            "pr_id",
        ],
    ),
    ("task_dependencies", &["task_id", "depends_on_id"]),
    ("documents", &["id", "title", "current_revision"]),
    (
        "document_revisions",
        &["id", "document_id", "content_digest", "content_reference"],
    ),
    ("task_documents", &["task_id", "document_id", "role"]),
    (
        "pull_requests",
        &[
            "id",
            "task_id",
            "host",
            "repository",
            "head",
            "base",
            "merged_head",
            "allowed_methods",
        ],
    ),
    ("pull_request_reviews", &["pr_id"]),
    (
        "interactions",
        &[
            "id",
            "task_id",
            "request_id",
            "author_agent_id",
            "recipient_agent_id",
            "body",
            "options",
            "answer",
            "acceptance_user",
        ],
    ),
    ("interaction_reads", &["interaction_id", "user_id"]),
    (
        "interaction_subjects",
        &[
            "interaction_id",
            "task_id",
            "pr_id",
            "document_id",
            "revision_id",
        ],
    ),
    (
        "delivery_targets",
        &[
            "interaction_id",
            "task_id",
            "text",
            "document_id",
            "revision_id",
            "pr_id",
            "head",
        ],
    ),
    ("delivery_feedback", &["interaction_id", "user_id", "body"]),
    ("document_reviews", &["task_id", "document_id"]),
    (
        "document_decisions",
        &[
            "task_id",
            "document_id",
            "revision_id",
            "presentation_id",
            "user_id",
        ],
    ),
    (
        "pr_decisions",
        &["pr_id", "task_id", "presentation_id", "head", "user_id"],
    ),
    (
        "merge_authorizations",
        &[
            "id",
            "task_id",
            "pr_id",
            "presentation_id",
            "head",
            "user_id",
            "claimed_by",
        ],
    ),
    (
        "task_stacks",
        &[
            "task_id",
            "upstream_task",
            "presentation_id",
            "pr_id",
            "head",
        ],
    ),
    (
        "stack_readiness",
        &["task_id", "presentation_id", "pr_id", "head"],
    ),
    (
        "interaction_attachments",
        &[
            "interaction_id",
            "kind",
            "id",
            "name",
            "content_digest",
            "content_reference",
        ],
    ),
];
fn stored_text_bytes(c: &Connection) -> Result<i64> {
    let sums = VARIABLE_TEXT
        .iter()
        .map(|(table, columns)| {
            let cells = columns
                .iter()
                .map(|column| format!("coalesce(length(cast({column} as blob)),0)"))
                .collect::<Vec<_>>()
                .join(" + ");
            format!("(SELECT coalesce(sum({cells}),0) FROM {table})")
        })
        .collect::<Vec<_>>()
        .join(" + ");
    let total: i64 = c.query_row(&format!("SELECT {sums}"), [], |r| r.get(0))?;
    ensure!(total >= 0, "invalid negative projection text size");
    Ok(total)
}
struct BoundedWriter {
    bytes: Vec<u8>,
    limit: usize,
    exceeded: bool,
}
impl Write for BoundedWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        if self
            .bytes
            .len()
            .checked_add(buf.len())
            .is_none_or(|n| n > self.limit)
        {
            self.exceeded = true;
            return Err(io::Error::other("domain projection byte budget exceeded"));
        }
        self.bytes.extend_from_slice(buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
impl DomainStore {
    pub(crate) fn project_bounded(&mut self, max_bytes: usize) -> Result<Vec<u8>> {
        ensure!(max_bytes > 0, "projection byte budget must be positive");
        let tx = self.conn.transaction()?;
        check_schema(&tx)?;
        if count_entities(&tx)? > MAX_ENTITIES {
            return Err(ProjectionTooLarge {
                reason: ProjectionLimit::Entities,
            }
            .into());
        }
        // Reject accumulated content before building the whole in-memory DomainState.
        // Final JSON still has a separate exact byte guard below.
        if stored_text_bytes(&tx)? as u64 > max_bytes.min(MAX_DOCUMENT_BYTES) as u64 {
            return Err(ProjectionTooLarge {
                reason: ProjectionLimit::Bytes,
            }
            .into());
        }
        let state = repository::load(&tx)?;
        let projection = state.projection();
        let mut writer = BoundedWriter {
            bytes: Vec::new(),
            limit: max_bytes.min(MAX_DOCUMENT_BYTES),
            exceeded: false,
        };
        let serialized = serde_json::to_writer(&mut writer, &projection);
        if writer.exceeded {
            return Err(ProjectionTooLarge {
                reason: ProjectionLimit::Bytes,
            }
            .into());
        }
        serialized?;
        tx.commit()?;
        Ok(writer.bytes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain_store::tests::{Fixture, seeded};
    #[test]
    fn bounded_projection_matches_normal_and_never_truncates() {
        let fixture = Fixture::new();
        let mut store = DomainStore::open(&fixture.path()).unwrap();
        seeded(&mut store);
        let ordinary = serde_json::to_value(store.project().unwrap()).unwrap();
        let bytes = store.project_bounded(8 * 1024 * 1024).unwrap();
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&bytes).unwrap(),
            ordinary
        );
        let err = store.project_bounded(bytes.len() - 1).unwrap_err();
        assert_eq!(
            err.downcast_ref::<ProjectionTooLarge>().unwrap().reason,
            ProjectionLimit::Bytes
        );
    }
    #[test]
    fn utf8_body_bytes_are_rejected_before_projection_allocation() {
        let fixture = Fixture::new();
        let mut store = DomainStore::open(&fixture.path()).unwrap();
        let body = "é".repeat(524_288); // one MiB UTF-8, half as many chars
        let tx = store.conn.transaction().unwrap();
        for i in 0..9 {
            tx.execute(
                "INSERT INTO requests(id,body,closed) VALUES(?1,?2,0)",
                rusqlite::params![format!("r-{i}"), body],
            )
            .unwrap();
        }
        tx.commit().unwrap();
        assert!(count_entities(&store.conn).unwrap() < MAX_ENTITIES);
        assert!(stored_text_bytes(&store.conn).unwrap() > 8 * 1024 * 1024);
        let err = store.project_bounded(8 * 1024 * 1024).unwrap_err();
        assert_eq!(
            err.downcast_ref::<ProjectionTooLarge>().unwrap().reason,
            ProjectionLimit::Bytes
        );
    }
    #[test]
    fn entity_pre_admission_fails_before_loading_large_state() {
        let fixture = Fixture::new();
        let mut store = DomainStore::open(&fixture.path()).unwrap();
        let tx = store.conn.transaction().unwrap();
        let mut stmt = tx
            .prepare("INSERT INTO requests(id,body,closed) VALUES(?1,'synthetic',0)")
            .unwrap();
        for i in 0..=MAX_ENTITIES {
            stmt.execute([format!("request-{i}")]).unwrap();
        }
        drop(stmt);
        tx.commit().unwrap();
        let err = store.project_bounded(8 * 1024 * 1024).unwrap_err();
        assert_eq!(
            err.downcast_ref::<ProjectionTooLarge>().unwrap().reason,
            ProjectionLimit::Entities
        );
    }
}
