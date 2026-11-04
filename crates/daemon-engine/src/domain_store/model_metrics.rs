//! The model decisions `model_metrics.sql` keeps: the model, effort and approach each ended task
//! is folded with.
use super::DomainStore;
use alondra_core::Millis;
use alondra_core::domain::ModelChooser;
use alondra_core::options::Approach;
use alondra_core::task_tags::TaskTags;
use anyhow::Result;
use rusqlite::{Transaction, params};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DecisionKind {
    Launch,
    Relaunch,
    Send,
}

impl DecisionKind {
    fn as_str(self) -> &'static str {
        match self {
            Self::Launch => "launch",
            Self::Relaunch => "relaunch",
            Self::Send => "send",
        }
    }
}

#[derive(Clone, Debug)]
pub struct Decision {
    pub task_id: String,
    pub request_id: String,
    pub agent_id: Option<String>,
    pub at: Millis,
    pub kind: DecisionKind,
    pub model: Option<String>,
    pub effort: Option<String>,
    pub approach: Option<Approach>,
    pub chosen_by: ModelChooser,
    pub reason: Option<String>,
    pub title: String,
    pub provider: Option<alondra_core::accounts::Provider>,
    pub tags: Option<TaskTags>,
}

const ROUTING_COLUMNS: [(&str, &str, &str); 6] = [
    ("model_decisions", "provider", "TEXT"),
    ("model_decisions", "task_kind", "TEXT"),
    ("model_decisions", "areas", "TEXT CHECK(areas IS NULL OR json_valid(areas))"),
    ("model_decisions", "risks", "TEXT CHECK(risks IS NULL OR json_valid(risks))"),
    ("task_metrics", "provider", "TEXT"),
    ("task_metrics", "kind", "TEXT"),
];

/// Gives a store made before decisions kept their provider and tags those columns, empty for the
/// rows it already has.
pub(super) fn add_routing_columns(tx: &Transaction<'_>) -> rusqlite::Result<()> {
    for (table, column, kind) in ROUTING_COLUMNS {
        let present: bool = tx.query_row("SELECT count(*) FROM pragma_table_info(?1) WHERE name=?2", [table, column], |r| r.get::<_, i64>(0))? == 1;
        if !present {
            tx.execute_batch(&format!("ALTER TABLE {table} ADD COLUMN {column} {kind}"))?;
        }
    }
    Ok(())
}

fn approach_text(approach: Approach) -> &'static str {
    match approach {
        Approach::Solo => "solo",
        Approach::Subagents => "subagents",
        Approach::Workflow => "workflow",
    }
}

impl DomainStore {
    pub(crate) fn record_model_decision(&mut self, d: &Decision) -> Result<()> {
        let chosen_by = match d.chosen_by {
            ModelChooser::Orchestrator => "orchestrator",
            ModelChooser::User => "user",
        };
        self.conn.execute(
            "INSERT INTO model_decisions(task_id,request_id,agent_id,at,kind,model,effort,approach,chosen_by,reason,title,provider,task_kind,areas,risks)
             VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15)",
            params![d.task_id, d.request_id, d.agent_id, d.at, d.kind.as_str(), d.model, d.effort, d.approach.map(approach_text), chosen_by, d.reason, d.title,
                d.provider.map(|provider| provider.as_str()), d.tags.as_ref().map(|tags| tags.kind.as_str()),
                d.tags.as_ref().and_then(TaskTags::areas_json), d.tags.as_ref().and_then(TaskTags::risks_json)],
        )?;
        self.note_side_write();
        Ok(())
    }

    #[cfg(any(test, feature = "test-hooks"))]
    pub(crate) fn model_decision_rows(&self) -> Result<Vec<(String, String, Option<String>, Option<String>, Option<String>, Option<String>)>> {
        let mut statement = self.conn.prepare("SELECT task_id,kind,model,effort,approach,reason FROM model_decisions ORDER BY id")?;
        let rows = statement.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?)))?;
        Ok(rows.collect::<std::result::Result<_, _>>()?)
    }

    #[cfg(any(test, feature = "test-hooks"))]
    pub(crate) fn model_decision_routing(&self) -> Result<Vec<(Option<String>, Option<String>, Option<String>, Option<String>)>> {
        let mut statement = self.conn.prepare("SELECT provider,task_kind,areas,risks FROM model_decisions ORDER BY id")?;
        let rows = statement.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))?;
        Ok(rows.collect::<std::result::Result<_, _>>()?)
    }

    #[cfg(any(test, feature = "test-hooks"))]
    pub(crate) fn model_decision_meta(&self) -> Result<Vec<(String, String, String)>> {
        let mut statement = self.conn.prepare("SELECT agent_id,chosen_by,title FROM model_decisions ORDER BY id")?;
        let rows = statement.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?;
        Ok(rows.collect::<std::result::Result<_, _>>()?)
    }
}

#[cfg(test)]
mod tests;
