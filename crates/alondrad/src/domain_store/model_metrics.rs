//! The model decisions `model_metrics.sql` keeps: the model, effort and approach each ended task
//! is folded with.
use super::DomainStore;
use alondra_core::Millis;
use alondra_core::domain::ModelChooser;
use alondra_core::options::Approach;
use anyhow::Result;
use rusqlite::params;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum DecisionKind {
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
pub(crate) struct Decision {
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
            "INSERT INTO model_decisions(task_id,request_id,agent_id,at,kind,model,effort,approach,chosen_by,reason,title) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11)",
            params![d.task_id, d.request_id, d.agent_id, d.at, d.kind.as_str(), d.model, d.effort, d.approach.map(approach_text), chosen_by, d.reason, d.title],
        )?;
        self.note_side_write();
        Ok(())
    }

    #[cfg(test)]
    pub(crate) fn model_decision_rows(&self) -> Result<Vec<(String, String, Option<String>, Option<String>, Option<String>, Option<String>)>> {
        let mut statement = self.conn.prepare("SELECT task_id,kind,model,effort,approach,reason FROM model_decisions ORDER BY id")?;
        let rows = statement.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?)))?;
        Ok(rows.collect::<std::result::Result<_, _>>()?)
    }

    #[cfg(test)]
    pub(crate) fn model_decision_meta(&self) -> Result<Vec<(String, String, String)>> {
        let mut statement = self.conn.prepare("SELECT agent_id,chosen_by,title FROM model_decisions ORDER BY id")?;
        let rows = statement.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?;
        Ok(rows.collect::<std::result::Result<_, _>>()?)
    }
}

#[cfg(test)]
mod tests;
