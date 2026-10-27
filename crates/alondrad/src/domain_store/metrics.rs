//! What the Metrics screen and `/metrics` sum: every ended task's own row, the weeks older ones
//! were added into, what sessions without a task spent by day, and the daemon's releases.
use super::DomainStore;
use alondra_core::Millis;
use alondra_core::metrics::fold::Outcome;
use alondra_core::metrics::report::{DayRow, MetricsInputs, Release, Spend, Sums, TaskRow, Tokens, WeekRow, day_millis};
use anyhow::Result;
use rusqlite::Row;

const TASKS: &str = "SELECT m.project,m.model,m.effort,m.approach,m.outcome,m.started_at,m.finished_at,m.clean,m.change_rounds,
  m.reviewer_rounds,m.interventions,m.reopened,m.relaunched,m.work_ms,m.waiting_user_ms,m.queued_ms,m.blocked_ms,m.sessions,m.steps,
  m.subagent_steps,m.looks,m.polls,m.pr_merged,m.pr_lines,m.measured,u.cost,u.costed,u.input,u.output,u.cache_read,u.cache_write,m.provider,m.kind
 FROM task_metrics m LEFT JOIN (SELECT task_id,sum(cost_micros) cost,count(cost_micros) costed,sum(input) input,sum(output) output,
  sum(cache_read) cache_read,sum(cache_write) cache_write FROM task_model_usage GROUP BY task_id) u ON u.task_id=m.task_id
 ORDER BY m.finished_at";

const WEEKS: &str = "SELECT week,project,model,effort,approach,tasks,done,clean,change_rounds,reviewer_rounds,interventions,reopened,
  relaunched,work_ms,waiting_user_ms,queued_ms,blocked_ms,measured,sessions,steps,subagent_steps,looks,polls,merged_prs,pr_lines,
  lined_prs,cost_micros,costed,merged_cost_micros,lined_cost_micros,input,output,cache_read,cache_write FROM metric_weeks ORDER BY week";

/// The days of sessions without a task: the tasks' own spend in `usage_days` is already in their
/// weeks.
const DAYS: &str = "SELECT day,model,sum(cost_micros),sum(costed),sum(sessions),sum(input),sum(output),sum(cache_read),sum(cache_write)
 FROM usage_days WHERE role IN('orchestrator','unattributed') GROUP BY day,model ORDER BY day,model";

fn task(r: &Row<'_>) -> rusqlite::Result<TaskRow> {
    let measured: bool = r.get(24)?;
    let costed: Option<i64> = r.get(26)?;
    let spend = measured.then(|| -> rusqlite::Result<Spend> {
        Ok(Spend {
            cost_micros: if costed.unwrap_or(0) > 0 { r.get(25)? } else { None },
            tokens: Tokens {
                input: r.get::<_, Option<i64>>(27)?.unwrap_or(0),
                output: r.get::<_, Option<i64>>(28)?.unwrap_or(0),
                cache_read: r.get::<_, Option<i64>>(29)?.unwrap_or(0),
                cache_write: r.get::<_, Option<i64>>(30)?.unwrap_or(0),
            },
        })
    }).transpose()?;
    Ok(TaskRow {
        project: r.get(0)?,
        model: r.get(1)?,
        effort: r.get(2)?,
        approach: r.get(3)?,
        provider: r.get(31)?,
        kind: r.get(32)?,
        outcome: if r.get::<_, String>(4)? == "done" { Outcome::Done } else { Outcome::Cancelled },
        started_at: r.get(5)?,
        finished_at: r.get(6)?,
        clean: r.get(7)?,
        change_rounds: r.get(8)?,
        reviewer_rounds: r.get(9)?,
        interventions: r.get(10)?,
        reopened: r.get(11)?,
        relaunched: r.get(12)?,
        work_ms: r.get(13)?,
        waiting_user_ms: r.get(14)?,
        queued_ms: r.get(15)?,
        blocked_ms: r.get(16)?,
        sessions: r.get(17)?,
        steps: r.get(18)?,
        subagent_steps: r.get(19)?,
        looks: r.get(20)?,
        polls: r.get(21)?,
        pr_merged: r.get(22)?,
        pr_lines: r.get(23)?,
        spend,
    })
}

fn week(r: &Row<'_>) -> rusqlite::Result<Option<WeekRow>> {
    let sums = Sums {
        tasks: r.get(5)?,
        done: r.get(6)?,
        clean: r.get(7)?,
        change_rounds: r.get(8)?,
        reviewer_rounds: r.get(9)?,
        interventions: r.get(10)?,
        reopened: r.get(11)?,
        relaunched: r.get(12)?,
        work_ms: r.get(13)?,
        waiting_user_ms: r.get(14)?,
        queued_ms: r.get(15)?,
        blocked_ms: r.get(16)?,
        measured: r.get(17)?,
        sessions: r.get(18)?,
        steps: r.get(19)?,
        subagent_steps: r.get(20)?,
        looks: r.get(21)?,
        polls: r.get(22)?,
        merged_prs: r.get(23)?,
        pr_lines: r.get(24)?,
        lined_prs: r.get(25)?,
        cost_micros: r.get(26)?,
        costed: r.get(27)?,
        merged_cost_micros: r.get(28)?,
        lined_cost_micros: r.get(29)?,
        tokens: Tokens { input: r.get(30)?, output: r.get(31)?, cache_read: r.get(32)?, cache_write: r.get(33)? },
    };
    let (project, model, effort, approach) = (r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?);
    Ok(day_millis(&r.get::<_, String>(0)?).map(|week| WeekRow { week, project, model, effort, approach, sums }))
}

fn day(r: &Row<'_>) -> rusqlite::Result<Option<DayRow>> {
    Ok(day_millis(&r.get::<_, String>(0)?).map(|day| -> rusqlite::Result<DayRow> {
        Ok(DayRow {
            day,
            model: r.get(1)?,
            cost_micros: r.get(2)?,
            costed: r.get(3)?,
            sessions: r.get(4)?,
            tokens: Tokens { input: r.get(5)?, output: r.get(6)?, cache_read: r.get(7)?, cache_write: r.get(8)? },
        })
    }).transpose()?)
}

impl DomainStore {
    /// Everything a metrics report sums, as the store holds it now. Tasks that ended since the last
    /// fold are not in it yet: fold them first.
    pub(crate) fn metrics_inputs(&self, now: Millis) -> Result<MetricsInputs> {
        let tasks = self.conn.prepare(TASKS)?.query_map([], task)?.collect::<rusqlite::Result<_>>()?;
        let weeks = self.conn.prepare(WEEKS)?.query_map([], week)?.filter_map(Result::transpose).collect::<rusqlite::Result<_>>()?;
        let days = self.conn.prepare(DAYS)?.query_map([], day)?.filter_map(Result::transpose).collect::<rusqlite::Result<_>>()?;
        let releases = self.conn.prepare("SELECT version,first_seen_at FROM daemon_releases ORDER BY first_seen_at,version")?
            .query_map([], |r| Ok(Release { version: r.get(0)?, first_seen_at: r.get(1)? }))?
            .collect::<rusqlite::Result<_>>()?;
        Ok(MetricsInputs { now, tasks, weeks, days, releases })
    }
}

#[cfg(test)]
#[path = "metrics_tests.rs"]
mod tests;
