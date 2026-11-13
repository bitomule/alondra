use super::*;
pub use alondra_daemon_engine_store::metrics_fold::*;
pub(super) use alondra_daemon_engine_store::testing::{DAY_MS, samples_of_folded_tasks, idle_sessions_without_task, tasks_into_weeks};
use alondra_core::Millis;
use alondra_core::domain::Feedback;
use alondra_core::metrics::fold::{Decision, DecisionKind, TaskFacts, fold_task};
use alondra_core::metrics::steps::StepCounts;
use alondra_core::metrics::{Role, Usage};
use anyhow::Result;
use rusqlite::{OptionalExtension, Transaction, params};
use std::collections::BTreeMap;

#[path = "metrics_fold_tests.rs"]
pub(crate) mod tests;
