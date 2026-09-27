//! Aggregate authorization failures for the opt-in laboratory binary only.
//! No SQL, error Display/debug strings, parameters, URLs or identity labels.
//! `statement_timeout` means SQLSTATE 57014 (query_canceled): a server-side
//! statement timeout is one cause; explicit cancellation can yield the same code.
use serde_json::{Value, json};
use sqlx_core::error::Error;
use std::sync::atomic::{AtomicU64, Ordering};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Category {
    PoolTimeout,
    StatementTimeout,
    Connection,
    Other,
}

fn database_category(code: Option<&str>) -> Category {
    match code {
        Some("57014") => Category::StatementTimeout,
        Some(code) if code.starts_with("08") => Category::Connection,
        // PostgreSQL shutdown/crash/cannot-connect-now responses.
        Some("57P01" | "57P02" | "57P03") => Category::Connection,
        _ => Category::Other,
    }
}
fn category(error: &anyhow::Error) -> Category {
    match error.downcast_ref::<Error>() {
        Some(Error::PoolTimedOut) => Category::PoolTimeout,
        Some(Error::Database(db)) => database_category(db.code().as_deref()),
        Some(Error::Io(_) | Error::Tls(_) | Error::PoolClosed | Error::WorkerCrashed) => {
            Category::Connection
        }
        _ => Category::Other,
    }
}

struct Counts([AtomicU64; 4]);
impl Counts {
    const fn new() -> Self {
        Self([const { AtomicU64::new(0) }; 4])
    }
    fn record(&self, category: Category) {
        self.0[category as usize].fetch_add(1, Ordering::Relaxed);
    }
    fn snapshot(&self) -> Value {
        json!({
            "pool_timeout": self.0[0].load(Ordering::Relaxed),
            "statement_timeout": self.0[1].load(Ordering::Relaxed),
            "connection": self.0[2].load(Ordering::Relaxed),
            "other": self.0[3].load(Ordering::Relaxed),
        })
    }
}
static COUNTS: Counts = Counts::new();

// Called only on the existing Unavailable branches, never on access denials.
// Counters are cumulative per process and reset when its epoch restarts.
pub(crate) fn record(error: &anyhow::Error) {
    COUNTS.record(category(error));
}
pub(crate) fn snapshot() -> Value {
    COUNTS.snapshot()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn typed_categories_never_emit_error_details() {
        let counts = Counts::new();
        let pool = anyhow::Error::new(Error::PoolTimedOut).context("private query/account");
        assert_eq!(category(&pool), Category::PoolTimeout);
        counts.record(category(&pool));
        let io = anyhow::Error::new(Error::Io(std::io::Error::other(
            "private-host:5432/password",
        )));
        assert_eq!(category(&io), Category::Connection);
        counts.record(category(&io));
        let unknown = anyhow::anyhow!("private SQL parameters");
        assert_eq!(category(&unknown), Category::Other);
        counts.record(category(&unknown));
        assert_eq!(database_category(Some("57014")), Category::StatementTimeout);
        counts.record(database_category(Some("57014")));
        assert_eq!(
            counts.snapshot(),
            json!({"pool_timeout":1,"statement_timeout":1,"connection":1,"other":1})
        );
        assert_eq!(database_category(Some("08006")), Category::Connection);
        assert_eq!(database_category(Some("57P01")), Category::Connection);
        assert_eq!(database_category(Some("23505")), Category::Other);
        assert_eq!(database_category(None), Category::Other);
        assert_eq!(
            category(&anyhow::Error::new(Error::RowNotFound)),
            Category::Other
        );
    }
}
