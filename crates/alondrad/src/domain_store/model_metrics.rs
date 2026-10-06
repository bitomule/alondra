//! The model decisions and node costs `model_metrics.sql` keeps, and the report built from them
//! joined with each Task's history.
use super::DomainStore;
use alondra_core::Millis;
use alondra_core::domain_v2::ModelChooser;
use alondra_core::model_report::{Attempt, Group, ModelReport, Outcome};
use alondra_core::options::Approach;
use anyhow::Result;
use rusqlite::{Connection, params};
use std::collections::{BTreeMap, HashMap};

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

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct NodeCost {
    pub session_id: String,
    /// The Task its state still names; the Task of its last model decision wins.
    pub task_id: Option<String>,
    pub model: Option<String>,
    pub cost_usd: Option<f64>,
    pub context_peak_percent: Option<f64>,
    pub started_at: Option<Millis>,
    pub ended_at: Millis,
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

    /// A session that ends again after a respawn keeps its earliest start, its highest context
    /// and cost, and its latest end.
    pub(crate) fn record_node_cost(&mut self, c: &NodeCost) -> Result<()> {
        self.conn.execute(
            "INSERT INTO node_costs(session_id,task_id,model,cost_usd,context_peak_percent,started_at,ended_at)
             VALUES(?1,COALESCE((SELECT task_id FROM model_decisions WHERE agent_id=?1 ORDER BY id DESC LIMIT 1),?2),?3,?4,?5,?6,?7)
             ON CONFLICT(session_id) DO UPDATE SET
              task_id=COALESCE(excluded.task_id,node_costs.task_id),
              model=COALESCE(excluded.model,node_costs.model),
              cost_usd=CASE WHEN node_costs.cost_usd IS NULL THEN excluded.cost_usd WHEN excluded.cost_usd IS NULL THEN node_costs.cost_usd ELSE max(node_costs.cost_usd,excluded.cost_usd) END,
              context_peak_percent=CASE WHEN node_costs.context_peak_percent IS NULL THEN excluded.context_peak_percent WHEN excluded.context_peak_percent IS NULL THEN node_costs.context_peak_percent ELSE max(node_costs.context_peak_percent,excluded.context_peak_percent) END,
              started_at=CASE WHEN node_costs.started_at IS NULL THEN excluded.started_at WHEN excluded.started_at IS NULL THEN node_costs.started_at ELSE min(node_costs.started_at,excluded.started_at) END,
              ended_at=excluded.ended_at",
            params![c.session_id, c.task_id, c.model, c.cost_usd, c.context_peak_percent, c.started_at, c.ended_at],
        )?;
        self.note_side_write();
        Ok(())
    }

    #[cfg(test)]
    pub(crate) fn node_cost(&self, session_id: &str) -> Result<Option<NodeCost>> {
        use rusqlite::OptionalExtension;
        Ok(self.conn.query_row(
            "SELECT session_id,task_id,model,cost_usd,context_peak_percent,started_at,ended_at FROM node_costs WHERE session_id=?1",
            [session_id],
            |r| Ok(NodeCost { session_id: r.get(0)?, task_id: r.get(1)?, model: r.get(2)?, cost_usd: r.get(3)?, context_peak_percent: r.get(4)?, started_at: r.get(5)?, ended_at: r.get(6)? }),
        ).optional()?)
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

    pub(crate) fn model_report(&mut self, since: Option<Millis>, until: Millis) -> Result<ModelReport> {
        let tx = self.conn.transaction_with_behavior(rusqlite::TransactionBehavior::Deferred)?;
        let facts = Facts::load(&tx)?;
        tx.commit()?;
        Ok(report(&facts, since, until))
    }
}

#[derive(Clone, Debug)]
struct DecisionRow {
    task_id: String,
    agent_id: Option<String>,
    at: Millis,
    kind: String,
    model: Option<String>,
    effort: Option<String>,
    approach: Option<String>,
    reason: Option<String>,
    title: String,
}

#[derive(Default)]
struct Facts {
    decisions: Vec<DecisionRow>,
    /// Each Task's states in the order they were entered, with when.
    history: HashMap<String, Vec<(Millis, String)>>,
    /// When the user asked for changes on a delivery that carried a PR, per Task.
    changes: HashMap<String, Vec<Millis>>,
    /// Session → (Task, cost).
    costs: HashMap<String, (Option<String>, Option<f64>)>,
}

impl Facts {
    fn load(c: &Connection) -> Result<Self> {
        let mut facts = Facts::default();
        let mut statement = c.prepare("SELECT task_id,agent_id,at,kind,model,effort,approach,reason,title FROM model_decisions ORDER BY task_id,at,id")?;
        facts.decisions = statement.query_map([], |r| Ok(DecisionRow {
            task_id: r.get(0)?, agent_id: r.get(1)?, at: r.get(2)?, kind: r.get(3)?, model: r.get(4)?,
            effort: r.get(5)?, approach: r.get(6)?, reason: r.get(7)?, title: r.get(8)?,
        }))?.collect::<std::result::Result<_, _>>()?;
        let mut statement = c.prepare("SELECT h.task_id,a.at,h.state FROM task_history h JOIN command_audit a USING(revision) WHERE h.task_id IN(SELECT task_id FROM model_decisions) ORDER BY h.task_id,h.revision")?;
        for row in statement.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, Millis>(1)?, r.get::<_, String>(2)?)))? {
            let (task, at, state) = row?;
            facts.history.entry(task).or_default().push((at, state));
        }
        let mut statement = c.prepare(
            "SELECT i.task_id,a.at FROM delivery_feedback f JOIN interactions i ON i.id=f.interaction_id JOIN command_audit a ON a.revision=f.at_revision
             WHERE f.instruction='changes' AND i.task_id IS NOT NULL AND EXISTS(SELECT 1 FROM delivery_targets t WHERE t.interaction_id=f.interaction_id AND t.kind='pull_request')",
        )?;
        for row in statement.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, Millis>(1)?)))? {
            let (task, at) = row?;
            facts.changes.entry(task).or_default().push(at);
        }
        let mut statement = c.prepare("SELECT session_id,task_id,cost_usd FROM node_costs")?;
        for row in statement.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, Option<String>>(1)?, r.get::<_, Option<f64>>(2)?)))? {
            let (session, task, cost) = row?;
            facts.costs.insert(session, (task, cost));
        }
        Ok(facts)
    }
}

struct Run {
    attempt: Attempt,
    approach: Option<String>,
    done_after: Option<Millis>,
    questions: u32,
    cost: Option<f64>,
}

fn terminal(state: &str) -> bool {
    matches!(state, "done" | "cancelled")
}

/// A launch or relaunch starts an attempt; a message starts one only when it hands the Task to
/// another node, since one to the same node continues its work.
fn attempts(decisions: &[DecisionRow]) -> Vec<(DecisionRow, Option<DecisionRow>)> {
    let mut started: Vec<DecisionRow> = vec![];
    for d in decisions {
        let continues = started.last().is_some_and(|current| current.task_id == d.task_id && d.kind == "send" && current.agent_id == d.agent_id);
        if continues {
            continue;
        }
        let latest_on_node = |field: fn(&DecisionRow) -> &Option<String>| decisions.iter()
            .filter(|earlier| earlier.agent_id.is_some() && earlier.agent_id == d.agent_id && earlier.at <= d.at && field(earlier).is_some())
            .max_by_key(|earlier| earlier.at)
            .and_then(|earlier| field(earlier).clone());
        let mut d = d.clone();
        d.model = d.model.clone().or_else(|| latest_on_node(|row| &row.model));
        d.effort = d.effort.clone().or_else(|| latest_on_node(|row| &row.effort));
        d.approach = d.approach.clone().or_else(|| latest_on_node(|row| &row.approach));
        started.push(d);
    }
    let next: Vec<Option<DecisionRow>> = started.iter().enumerate()
        .map(|(i, d)| started.get(i + 1).filter(|n| n.task_id == d.task_id).cloned())
        .collect();
    started.into_iter().zip(next).collect()
}

fn run_of(facts: &Facts, d: &DecisionRow, next: Option<&DecisionRow>, attributed: &mut std::collections::HashSet<String>) -> Run {
    let end = next.map_or(Millis::MAX, |n| n.at);
    let within = |at: Millis| at >= d.at && at < end;
    let history = facts.history.get(&d.task_id).map(Vec::as_slice).unwrap_or_default();
    let mut outcomes = vec![];
    let mut done_after = None;
    let mut questions = 0;
    let (mut reopened, mut cancelled) = (false, false);
    for (index, (at, state)) in history.iter().enumerate() {
        if !within(*at) {
            continue;
        }
        match state.as_str() {
            "done" if done_after.is_none() => done_after = Some(at - d.at),
            "cancelled" => cancelled = true,
            "needs_you" => questions += 1,
            _ => {}
        }
        if !terminal(state) && index > 0 && terminal(&history[index - 1].1) {
            reopened = true;
        }
    }
    let relaunched = next.is_some_and(|n| n.kind != "send");
    let changes = facts.changes.get(&d.task_id).is_some_and(|times| times.iter().any(|at| within(*at)));
    if reopened { outcomes.push(Outcome::Reopened); }
    if relaunched { outcomes.push(Outcome::Relaunched); }
    if cancelled { outcomes.push(Outcome::Cancelled); }
    if changes { outcomes.push(Outcome::ChangesRequested); }
    let cost = d.agent_id.as_ref()
        .filter(|agent| attributed.insert((*agent).clone()))
        .and_then(|agent| facts.costs.get(agent))
        .filter(|(task, _)| task.as_ref().is_none_or(|task| *task == d.task_id))
        .and_then(|(_, cost)| *cost);
    let relaunched_to = next.filter(|_| relaunched).and_then(|n| n.model.clone());
    Run {
        attempt: Attempt {
            task_id: d.task_id.clone(), title: d.title.clone(), at: d.at, model: d.model.clone(), effort: d.effort.clone(),
            reason: d.reason.clone(), outcomes, relaunched_to,
        },
        approach: d.approach.clone(),
        done_after,
        questions,
        cost,
    }
}

fn group(runs: &[&Run]) -> Group {
    let has = |run: &Run, outcome: Outcome| run.attempt.outcomes.contains(&outcome);
    let mut g = Group { tasks: runs.len() as u32, ..Group::default() };
    for &run in runs {
        g.done_clean += u32::from(run.done_after.is_some() && run.attempt.outcomes.iter().all(|o| *o == Outcome::ChangesRequested));
        g.reopened += u32::from(has(run, Outcome::Reopened));
        g.relaunched += u32::from(has(run, Outcome::Relaunched));
        g.cancelled += u32::from(has(run, Outcome::Cancelled));
        g.changes_requested += u32::from(has(run, Outcome::ChangesRequested));
        g.questions += run.questions;
        if let Some(to) = run.attempt.relaunched_to.clone().filter(|_| has(run, Outcome::Relaunched)) {
            *g.relaunched_to.entry(to).or_default() += 1;
        }
    }
    let mut times: Vec<Millis> = runs.iter().filter_map(|run| run.done_after).collect();
    times.sort_unstable();
    g.median_ms_to_done = match times.len() {
        0 => None,
        n if n % 2 == 1 => Some(times[n / 2]),
        n => Some((times[n / 2 - 1] + times[n / 2]) / 2),
    };
    let costs: Vec<f64> = runs.iter().filter_map(|run| run.cost).collect();
    g.costed = costs.len() as u32;
    g.mean_cost_usd = (!costs.is_empty()).then(|| costs.iter().sum::<f64>() / costs.len() as f64);
    g
}

fn report(facts: &Facts, since: Option<Millis>, until: Millis) -> ModelReport {
    let mut attributed = std::collections::HashSet::new();
    let runs: Vec<Run> = attempts(&facts.decisions).iter()
        .map(|(d, next)| run_of(facts, d, next.as_ref(), &mut attributed))
        .filter(|run| since.is_none_or(|since| run.attempt.at >= since) && run.attempt.at <= until)
        .collect();
    let grouped = |key: &dyn Fn(&Run) -> (Option<String>, Option<String>, Option<String>)| {
        let mut by: BTreeMap<(Option<String>, Option<String>, Option<String>), Vec<&Run>> = BTreeMap::new();
        for run in &runs {
            by.entry(key(run)).or_default().push(run);
        }
        let mut groups: Vec<Group> = by.into_iter().map(|((model, effort, approach), runs)| Group { model, effort, approach, ..group(&runs) }).collect();
        groups.sort_by(|a, b| b.tasks.cmp(&a.tasks).then_with(|| a.model.cmp(&b.model)).then_with(|| a.effort.cmp(&b.effort)));
        groups
    };
    let by_model = grouped(&|run| (run.attempt.model.clone(), run.attempt.effort.clone(), None));
    let by_approach = grouped(&|run| (None, None, Some(run.approach.clone().unwrap_or_else(|| "unknown".into()))));
    let mut sonnet_trouble: Vec<Attempt> = runs.iter()
        .filter(|run| run.attempt.model.as_deref() == Some("sonnet") && !run.attempt.outcomes.is_empty())
        .map(|run| run.attempt.clone())
        .collect();
    sonnet_trouble.sort_by(|a, b| b.at.cmp(&a.at));
    ModelReport { since, until, by_model, by_approach, sonnet_trouble }
}

#[cfg(test)]
mod tests;
