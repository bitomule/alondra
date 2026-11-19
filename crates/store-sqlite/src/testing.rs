//! Narrow access for the engine's runtime-dependent storage tests.
use super::*;
pub use super::owner::ObservationRefusal;
pub const DAY_MS: i64 = super::metrics_fold::DAY_MS;
pub const WINDOW_MS: i64 = super::retention::WINDOW_MS;
pub const AUDIT_BOUND_TO_RECEIPTS: &str = super::retention::AUDIT_BOUND_TO_RECEIPTS;
pub fn summary(json: &str) -> String { super::retention::summary(json) }
pub fn load(connection: &Connection) -> Result<DomainState> { super::repository::load(connection) }
pub fn save_journal<'a>(journal: &mut alondra_store::LifecycleTransaction<'_>, task: &TaskId,
    records: impl IntoIterator<Item = &'a LifecycleRecord>, known: usize) -> Result<()> {
    super::repository::save_journal(journal, task, records, known)
}
pub fn samples_of_folded_tasks(tx: &rusqlite::Transaction<'_>, now: i64, limit: i64) -> rusqlite::Result<usize> {
    super::metrics_fold::samples_of_folded_tasks(tx, now, limit)
}
pub fn idle_sessions_without_task(tx: &rusqlite::Transaction<'_>, now: i64, limit: i64) -> rusqlite::Result<usize> {
    super::metrics_fold::idle_sessions_without_task(tx, now, limit)
}
pub fn tasks_into_weeks(tx: &rusqlite::Transaction<'_>, now: i64, limit: i64) -> rusqlite::Result<usize> {
    super::metrics_fold::tasks_into_weeks(tx, now, limit)
}
impl DomainStore {
    pub fn cached_state(&self) -> Option<(&Arc<DomainState>, u64)> {
        self.cached.as_ref().map(|cached| (&cached.state, cached.total_changes))
    }
}
