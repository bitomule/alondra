//! Observations of an SDK host that no owner can ever settle. Each is discarded with why, so its
//! task goes on instead of failing the same way at every takeover.
pub use alondra_engine_ports::journal::owner::{AdapterRefusal, DiscardReason};
use super::StoreError;
use super::owner::ObservationRefusal;
use alondra_foundation::ExecutionStamp;
use alondra_provider_contract::AdapterError;
use rusqlite::{Connection, params};

pub(super) const SCHEMA: &str = "CREATE TABLE IF NOT EXISTS provider_discarded_observations(task_id TEXT NOT NULL,generation INTEGER NOT NULL,cursor INTEGER NOT NULL CHECK(cursor>0),reason TEXT NOT NULL,at INTEGER NOT NULL,PRIMARY KEY(task_id,generation,cursor))";







pub(super) fn record(conn: &Connection, execution: &ExecutionStamp, cursor: u64, reason: DiscardReason) -> anyhow::Result<()> {
    conn.execute(
        "INSERT INTO provider_discarded_observations(task_id,generation,cursor,reason,at) VALUES(?1,?2,?3,?4,?5) ON CONFLICT(task_id,generation,cursor) DO NOTHING",
        params![execution.task().0, i64::try_from(execution.generation())?, i64::try_from(cursor)?, reason.code(), alondra_core::now_millis()],
    )?;
    Ok(())
}
