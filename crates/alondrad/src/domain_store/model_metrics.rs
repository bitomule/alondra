//! The model decisions `model_metrics.sql` keeps, and the report built from them joined with each
//! Task's history and what its sessions spent.
use super::DomainStore;
use alondra_core::Millis;
use alondra_core::domain::{Feedback, ModelChooser};
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

#[derive(Clone, Copy)]
struct ChangeRound {
    at: Millis,
    by_reviewer: bool,
    on_pull_request: bool,
}

#[derive(Default)]
struct Facts {
    decisions: Vec<DecisionRow>,
    /// Each Task's states in the order they were entered, with when.
    history: HashMap<String, Vec<(Millis, String)>>,
    /// Every time the user or a reviewer asked for changes on a delivery, per Task.
    changes: HashMap<String, Vec<ChangeRound>>,
    /// Each reviewer verdict on a Task: when, and how many serious failures it found.
    findings: HashMap<String, Vec<(Millis, u32)>>,
    /// Session → (Task, cost), while its samples are kept.
    costs: HashMap<String, (Option<String>, Option<f64>)>,
    /// Task → what its sessions cost, once folded.
    task_costs: HashMap<String, f64>,
    /// The role each Task's launch plan names: design, implementation or review.
    roles: HashMap<String, String>,
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
            "SELECT i.task_id,a.at,f.user_id=?1,EXISTS(SELECT 1 FROM delivery_targets t WHERE t.interaction_id=f.interaction_id AND t.kind='pull_request')
             FROM delivery_feedback f JOIN interactions i ON i.id=f.interaction_id JOIN command_audit a ON a.revision=f.at_revision
             WHERE f.instruction='changes' AND i.task_id IS NOT NULL",
        )?;
        for row in statement.query_map([Feedback::ALONDRA], |r| Ok((r.get::<_, String>(0)?, ChangeRound { at: r.get(1)?, by_reviewer: r.get(2)?, on_pull_request: r.get(3)? })))? {
            let (task, round) = row?;
            facts.changes.entry(task).or_default().push(round);
        }
        let mut statement = c.prepare("SELECT f.task_id,a.at,f.serious_findings FROM review_findings f JOIN command_audit a ON a.revision=f.at_revision")?;
        for row in statement.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, Millis>(1)?, r.get::<_, u32>(2)?)))? {
            let (task, at, serious) = row?;
            facts.findings.entry(task).or_default().push((at, serious));
        }
        let mut statement = c.prepare("SELECT task_id,role FROM task_launch_plans WHERE role IS NOT NULL")?;
        for row in statement.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))? {
            let (task, role) = row?;
            facts.roles.insert(task, role);
        }
        let dollars = |micros: Option<i64>| micros.map(|micros| micros as f64 / 1_000_000.0);
        let mut statement = c.prepare("SELECT session_id,max(task_id),sum(cost_micros) FROM usage_sessions GROUP BY session_id")?;
        for row in statement.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, Option<String>>(1)?, r.get::<_, Option<i64>>(2)?)))? {
            let (session, task, cost) = row?;
            facts.costs.insert(session, (task, dollars(cost)));
        }
        let mut statement = c.prepare("SELECT task_id,sum(cost_micros) FROM task_model_usage GROUP BY task_id HAVING count(cost_micros)>0")?;
        for row in statement.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))? {
            let (task, cost) = row?;
            facts.task_costs.extend(dollars(Some(cost)).map(|cost| (task, cost)));
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
    let rounds: Vec<ChangeRound> = facts.changes.get(&d.task_id).into_iter().flatten().copied().filter(|round| within(round.at)).collect();
    let changes = rounds.iter().any(|round| round.on_pull_request);
    let serious_findings = facts.findings.get(&d.task_id).into_iter().flatten().filter(|(at, _)| within(*at)).map(|(_, serious)| serious).sum();
    if reopened { outcomes.push(Outcome::Reopened); }
    if relaunched { outcomes.push(Outcome::Relaunched); }
    if cancelled { outcomes.push(Outcome::Cancelled); }
    if changes { outcomes.push(Outcome::ChangesRequested); }
    let sampled = |agent: &String| facts.costs.get(agent).filter(|(task, _)| task.as_ref().is_none_or(|task| *task == d.task_id));
    let task_sampled = facts.costs.values().any(|(task, _)| task.as_deref() == Some(d.task_id.as_str()));
    let cost = match d.agent_id.as_ref().filter(|agent| attributed.insert((*agent).clone())) {
        Some(agent) if sampled(agent).is_some() => sampled(agent).and_then(|(_, cost)| *cost),
        _ if next.is_none() && !task_sampled => facts.task_costs.get(&d.task_id).copied(),
        _ => None,
    };
    let relaunched_to = next.filter(|_| relaunched).and_then(|n| n.model.clone());
    Run {
        attempt: Attempt {
            task_id: d.task_id.clone(), title: d.title.clone(), at: d.at, model: d.model.clone(), effort: d.effort.clone(),
            reason: d.reason.clone(), outcomes, relaunched_to,
            change_rounds: rounds.len() as u32,
            reviewer_rounds: rounds.iter().filter(|round| round.by_reviewer).count() as u32,
            serious_findings,
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
        g.change_rounds += run.attempt.change_rounds;
        g.reviewer_rounds += run.attempt.reviewer_rounds;
        g.serious_findings += run.attempt.serious_findings;
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
    let by_role = ["design", "implementation", "review"].into_iter().filter_map(|role| {
        let of_role: Vec<&Run> = runs.iter().filter(|run| facts.roles.get(&run.attempt.task_id).map(String::as_str) == Some(role)).collect();
        (!of_role.is_empty()).then(|| Group { role: Some(role.to_owned()), ..group(&of_role) })
    }).collect();
    let mut sonnet_trouble: Vec<Attempt> = runs.iter()
        .filter(|run| run.attempt.model.as_deref() == Some("sonnet") && (!run.attempt.outcomes.is_empty() || run.attempt.change_rounds > 0 || run.attempt.serious_findings > 0))
        .map(|run| run.attempt.clone())
        .collect();
    sonnet_trouble.sort_by(|a, b| b.at.cmp(&a.at));
    let mut attempts: Vec<Attempt> = runs.iter().map(|run| run.attempt.clone()).collect();
    attempts.sort_by_key(|attempt| attempt.at);
    ModelReport { since, until, by_model, by_approach, by_role, sonnet_trouble, attempts }
}

#[cfg(test)]
mod tests;
