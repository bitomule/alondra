//! Observations of an SDK host that no owner can ever settle. Each is discarded with why, so its
//! task goes on instead of failing the same way at every takeover.
use super::StoreError;
use super::owner::ObservationRefusal;
use alondra_foundation::ExecutionStamp;
use alondra_provider_contract::AdapterError;
use rusqlite::{Connection, params};

pub(super) const SCHEMA: &str = "CREATE TABLE IF NOT EXISTS provider_discarded_observations(task_id TEXT NOT NULL,generation INTEGER NOT NULL,cursor INTEGER NOT NULL CHECK(cursor>0),reason TEXT NOT NULL,at INTEGER NOT NULL,PRIMARY KEY(task_id,generation,cursor))";

/// Why an observation was discarded. Its code is what the table keeps.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DiscardReason {
    /// The domain recorded its facts, but not as far as the observation says they move the task.
    Unsettled,
    /// It holds a fact the domain has no command for, or one that names what the task lacks.
    Unrecordable,
    /// The provider's adapter refused it, with the kind of its refusal.
    AdapterRefused(AdapterRefusal),
    /// Its evidence is larger than an observation may be.
    Capacity,
    /// The domain refused it even with what it cannot record left out.
    DomainRefused,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AdapterRefusal {
    InvalidObservation,
    Unsupported,
    UnknownInput,
    Storage,
    Protocol,
}

impl DiscardReason {
    /// The reason an observation that failed with `error` is discarded for, or `None` when the
    /// failure is not the observation's own: another owner, a conflict of evidence, the disk.
    pub(crate) fn of(error: &StoreError) -> Option<Self> {
        let StoreError::Storage(error) = error else { return Some(Self::DomainRefused) };
        if matches!(error.downcast_ref::<alondra_store::Error>(), Some(alondra_store::Error::UnsettledObservation)) {
            return Some(Self::Unsettled);
        }
        use alondra_agent_application::Error;
        Some(match error.downcast_ref::<ObservationRefusal>()? {
            ObservationRefusal::Unrecordable(_) | ObservationRefusal::Provider(Error::RequiresDomainCommand) => Self::Unrecordable,
            ObservationRefusal::Provider(Error::ObservationCapacity) => Self::Capacity,
            ObservationRefusal::Provider(Error::Adapter(adapter)) => Self::AdapterRefused(match adapter {
                AdapterError::Disconnected => return None,
                AdapterError::InvalidObservation => AdapterRefusal::InvalidObservation,
                AdapterError::Unsupported => AdapterRefusal::Unsupported,
                AdapterError::UnknownInput => AdapterRefusal::UnknownInput,
                AdapterError::Storage(_) => AdapterRefusal::Storage,
                AdapterError::Protocol(_) => AdapterRefusal::Protocol,
            }),
            ObservationRefusal::Provider(Error::Store(_) | Error::BindingMismatch) => return None,
        })
    }

    pub fn code(self) -> &'static str {
        match self {
            Self::Unsettled => "unsettled",
            Self::Unrecordable => "unrecordable",
            Self::AdapterRefused(AdapterRefusal::InvalidObservation) => "adapter_invalid_observation",
            Self::AdapterRefused(AdapterRefusal::Unsupported) => "adapter_unsupported",
            Self::AdapterRefused(AdapterRefusal::UnknownInput) => "adapter_unknown_input",
            Self::AdapterRefused(AdapterRefusal::Storage) => "adapter_storage",
            Self::AdapterRefused(AdapterRefusal::Protocol) => "adapter_protocol",
            Self::Capacity => "capacity",
            Self::DomainRefused => "domain_refused",
        }
    }
}

pub(super) fn record(conn: &Connection, execution: &ExecutionStamp, cursor: u64, reason: DiscardReason) -> anyhow::Result<()> {
    conn.execute(
        "INSERT INTO provider_discarded_observations(task_id,generation,cursor,reason,at) VALUES(?1,?2,?3,?4,?5) ON CONFLICT(task_id,generation,cursor) DO NOTHING",
        params![execution.task().0, i64::try_from(execution.generation())?, i64::try_from(cursor)?, reason.code(), alondra_core::now_millis()],
    )?;
    Ok(())
}
