//! What sessions spend, recorded as their turns end, and each ended task folded into one row of
//! `task_metrics` that stays when the samples it came from are pruned (`model_metrics.sql`).
use super::DomainStore;
use alondra_core::Millis;
use alondra_core::domain::Feedback;
use alondra_core::metrics::fold::{Decision, DecisionKind, TaskFacts, fold_task};
use alondra_core::metrics::steps::StepCounts;
use alondra_core::metrics::{Role, Usage};
use anyhow::Result;
use rusqlite::{OptionalExtension, Transaction, params};

const DAY_MS: i64 = 24 * 60 * 60 * 1_000;
/// How long the samples of a folded task stay, so a task reopened meanwhile is folded again with
/// them, and how long a merged PR's size is asked for.
pub(crate) const SAMPLES_MS: i64 = 7 * DAY_MS;
/// How long each ended task keeps its own row before it is added into its week.
pub(crate) const TASKS_MS: i64 = 90 * DAY_MS;

/// What a session reported as one of its turns ended.
#[derive(Clone, Debug)]
pub(crate) struct Spent<'a> {
    pub session_id: &'a str,
    /// Fixed by the session's first report: a session without a task (the orchestrator, or a node
    /// no task was known for) is added up by day as it spends instead of folded.
    pub task_id: Option<&'a str>,
    pub role: Role,
    pub provider: &'a str,
    pub usage: &'a [Usage],
    pub steps: StepCounts,
    pub at: Millis,
}

impl DomainStore {
    pub(crate) fn record_spend(&mut self, spent: &Spent<'_>) -> Result<()> {
        let tx = self.conn.transaction()?;
        for usage in spent.usage.iter().filter(|usage| !usage.is_empty()) {
            record_usage(&tx, spent, usage)?;
        }
        if !spent.steps.is_empty() {
            let s = spent.steps;
            tx.execute(
                "INSERT INTO session_steps(session_id,task_id,steps,subagent_steps,looks,polls,tool_calls,last_at) VALUES(?1,?2,?3,?4,?5,?6,?7,?8)
                 ON CONFLICT(session_id) DO UPDATE SET steps=steps+excluded.steps,subagent_steps=subagent_steps+excluded.subagent_steps,
                  looks=looks+excluded.looks,polls=polls+excluded.polls,tool_calls=tool_calls+excluded.tool_calls,folded=0,last_at=excluded.last_at",
                params![spent.session_id, spent.task_id, s.steps, s.subagent_steps, s.looks, s.polls, s.tool_calls, spent.at],
            )?;
        }
        tx.commit()?;
        self.note_side_write();
        Ok(())
    }

    pub(crate) fn record_release(&mut self, version: &str, at: Millis) -> Result<()> {
        self.conn.execute("INSERT OR IGNORE INTO daemon_releases(version,first_seen_at) VALUES(?1,?2)", params![version, at])?;
        self.note_side_write();
        Ok(())
    }

    /// Folds at most `limit` ended tasks that have no row yet, or that changed or spent since
    /// they were folded, and says how many it folded.
    pub(crate) fn fold_ended_tasks(&mut self, limit: usize) -> Result<usize> {
        let tx = self.conn.transaction()?;
        let tasks: Vec<String> = {
            let mut statement = tx.prepare(
                "WITH last AS(SELECT task_id,max(revision) revision FROM task_history GROUP BY task_id)
                 SELECT t.id FROM tasks t JOIN last l ON l.task_id=t.id JOIN task_history h ON h.revision=l.revision AND h.task_id=t.id
                  LEFT JOIN task_metrics m ON m.task_id=t.id
                 WHERE t.state IN('done','cancelled') AND h.state IN('done','cancelled')
                  AND (m.task_id IS NULL OR m.folded_revision<l.revision
                  OR EXISTS(SELECT 1 FROM usage_sessions u WHERE u.task_id=t.id AND u.folded=0)
                  OR EXISTS(SELECT 1 FROM session_steps s WHERE s.task_id=t.id AND s.folded=0))
                 ORDER BY t.id LIMIT ?1",
            )?;
            statement.query_map([limit as i64], |row| row.get(0))?.collect::<rusqlite::Result<_>>()?
        };
        let mut folded = 0;
        for task in &tasks {
            folded += usize::from(fold(&tx, task)?);
        }
        tx.commit()?;
        if folded > 0 {
            self.note_side_write();
        }
        Ok(folded)
    }

    /// The merged PRs of tasks folded in the last week whose size is not known yet, with the
    /// address GitHub answers for each.
    pub(crate) fn prs_without_size(&self, now: Millis, limit: usize) -> Result<Vec<(String, String)>> {
        let mut statement = self.conn.prepare(
            "SELECT m.task_id,'https://'||p.host||'/'||p.repository||'/pull/'||p.number FROM task_metrics m JOIN pull_requests p ON p.task_id=m.task_id
             WHERE m.pr_merged=1 AND m.pr_lines IS NULL AND m.finished_at>=?1 ORDER BY m.finished_at DESC LIMIT ?2",
        )?;
        let rows = statement.query_map(params![now - SAMPLES_MS, limit as i64], |row| Ok((row.get(0)?, row.get(1)?)))?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    #[cfg(test)]
    pub(crate) fn usage_row(&self, session: &str, model: &str) -> Option<(Option<String>, Option<i64>, [i64; 4], bool)> {
        self.conn.query_row(
            "SELECT task_id,cost_micros,input,output,cache_read,cache_write,complete FROM usage_sessions WHERE session_id=?1 AND model=?2",
            params![session, model],
            |r| Ok((r.get(0)?, r.get(1)?, [r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?], r.get(6)?)),
        ).optional().unwrap()
    }

    #[cfg(test)]
    pub(crate) fn steps_row(&self, session: &str) -> Option<(Option<String>, StepCounts)> {
        self.conn.query_row(
            "SELECT task_id,steps,subagent_steps,looks,polls,tool_calls FROM session_steps WHERE session_id=?1",
            [session],
            |r| Ok((r.get(0)?, StepCounts { steps: r.get(1)?, subagent_steps: r.get(2)?, looks: r.get(3)?, polls: r.get(4)?, tool_calls: r.get(5)? })),
        ).optional().unwrap()
    }

    #[cfg(test)]
    pub(crate) fn pr_lines(&self, task_id: &str) -> Option<i64> {
        self.conn.query_row("SELECT pr_lines FROM task_metrics WHERE task_id=?1", [task_id], |r| r.get(0)).unwrap()
    }

    /// The lines a task's merged PR added and deleted, together.
    pub(crate) fn record_pr_lines(&mut self, task_id: &str, lines: i64) -> Result<()> {
        self.conn.execute("UPDATE task_metrics SET pr_lines=?2 WHERE task_id=?1", params![task_id, lines])?;
        self.note_side_write();
        Ok(())
    }
}

/// The first report of a session that works for no task is a delta too: each later one adds what
/// it grew by to its day.
fn record_usage(tx: &Transaction<'_>, spent: &Spent<'_>, usage: &Usage) -> rusqlite::Result<()> {
    let known: Option<(Option<String>, Option<i64>, i64, i64, i64, i64)> = tx.query_row(
        "SELECT task_id,cost_micros,input,output,cache_read,cache_write FROM usage_sessions WHERE session_id=?1 AND model=?2",
        params![spent.session_id, usage.model],
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?, row.get(5)?)),
    ).optional()?;
    tx.execute(
        "INSERT INTO usage_sessions(session_id,model,provider,task_id,role,cost_micros,input,output,cache_read,cache_write,complete,first_at,last_at)
         VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?12,?11,?11)
         ON CONFLICT(session_id,model) DO UPDATE SET
          complete=complete AND excluded.complete AND excluded.input+excluded.output+excluded.cache_read+excluded.cache_write>=input+output+cache_read+cache_write,
          cost_micros=CASE WHEN cost_micros IS NULL THEN excluded.cost_micros WHEN excluded.cost_micros IS NULL THEN cost_micros ELSE max(cost_micros,excluded.cost_micros) END,
          input=max(input,excluded.input),output=max(output,excluded.output),
          cache_read=max(cache_read,excluded.cache_read),cache_write=max(cache_write,excluded.cache_write),
          folded=0,last_at=excluded.last_at",
        params![spent.session_id, usage.model, spent.provider, spent.task_id, spent.role.as_str(), usage.cost_micros,
            usage.input, usage.output, usage.cache_read, usage.cache_write, spent.at, spent.provider != "codex"],
    )?;
    let task = match &known {
        Some((task, ..)) => task.clone(),
        None => spent.task_id.map(str::to_owned),
    };
    if task.is_some() {
        return Ok(());
    }
    let grew = |now: i64, before: Option<i64>| (now - before.unwrap_or(0)).max(0);
    let (cost, input, output, cache_read, cache_write) = match &known {
        Some((_, cost, input, output, cache_read, cache_write)) => (
            usage.cost_micros.map(|now| grew(now, *cost)), grew(usage.input, Some(*input)), grew(usage.output, Some(*output)),
            grew(usage.cache_read, Some(*cache_read)), grew(usage.cache_write, Some(*cache_write)),
        ),
        None => (usage.cost_micros, usage.input, usage.output, usage.cache_read, usage.cache_write),
    };
    let role = match spent.role {
        Role::Orchestrator => "orchestrator",
        Role::Node => "unattributed",
    };
    let first = known.is_none();
    tx.execute(
        "INSERT INTO usage_days(day,provider,model,role,cost_micros,costed,input,output,cache_read,cache_write,sessions)
         VALUES(date(?1/1000,'unixepoch'),?2,?3,?4,?5,?6,?7,?8,?9,?10,?11)
         ON CONFLICT(day,provider,model,role) DO UPDATE SET cost_micros=cost_micros+excluded.cost_micros,costed=costed+excluded.costed,
          input=input+excluded.input,output=output+excluded.output,cache_read=cache_read+excluded.cache_read,
          cache_write=cache_write+excluded.cache_write,sessions=sessions+excluded.sessions",
        params![spent.at, spent.provider, usage.model, role, cost.unwrap_or(0), i64::from(first && usage.cost_micros.is_some()),
            input, output, cache_read, cache_write, i64::from(first)],
    )?;
    Ok(())
}

fn decision_kind(kind: &str) -> DecisionKind {
    match kind {
        "launch" => DecisionKind::Launch,
        "relaunch" => DecisionKind::Relaunch,
        _ => DecisionKind::Send,
    }
}

/// The task's facts as the store keeps them after its cards are compacted.
fn facts(tx: &Transaction<'_>, task: &str) -> rusqlite::Result<(TaskFacts, i64)> {
    let mut facts = TaskFacts::default();
    let mut revision = 0;
    let mut statement = tx.prepare("SELECT a.at,h.state,h.revision FROM task_history h JOIN command_audit a USING(revision) WHERE h.task_id=?1 ORDER BY h.revision")?;
    for row in statement.query_map([task], |row| Ok((row.get::<_, Millis>(0)?, row.get::<_, String>(1)?, row.get::<_, i64>(2)?)))? {
        let (at, state, at_revision) = row?;
        facts.history.push((at, state));
        revision = at_revision;
    }
    let mut statement = tx.prepare(
        "SELECT f.instruction,f.user_id=?2 FROM delivery_feedback f JOIN interactions i ON i.id=f.interaction_id WHERE i.task_id=?1 ORDER BY f.at_revision,f.ordinal",
    )?;
    for row in statement.query_map(params![task, Feedback::ALONDRA], |row| Ok((row.get::<_, String>(0)?, row.get::<_, bool>(1)?)))? {
        let (instruction, by_alondra) = row?;
        if instruction == "changes" {
            facts.change_rounds.push(by_alondra);
        }
        if !by_alondra && instruction != "conflicts" {
            facts.user_feedback += 1;
        }
    }
    let mut statement = tx.prepare("SELECT kind,agent_id,model,effort,approach FROM model_decisions WHERE task_id=?1 ORDER BY at,id")?;
    facts.decisions = statement.query_map([task], |row| Ok(Decision {
        kind: decision_kind(&row.get::<_, String>(0)?), agent_id: row.get(1)?, model: row.get(2)?, effort: row.get(3)?, approach: row.get(4)?,
    }))?.collect::<rusqlite::Result<_>>()?;
    Ok((facts, revision))
}

#[derive(Default)]
struct Sampled {
    sessions: i64,
    steps: [i64; 4],
}

/// Folds one ended task; false when its history does not end it.
fn fold(tx: &Transaction<'_>, task: &str) -> rusqlite::Result<bool> {
    let (facts, revision) = facts(tx, task)?;
    let Some(m) = fold_task(&facts) else { return Ok(false) };
    let (project, role, pr_merged): (Option<String>, Option<String>, bool) = tx.query_row(
        "SELECT COALESCE(lower(trim(l.project)),lower(replace(p.repository,rtrim(p.repository,replace(p.repository,'/','')),''))),
          (SELECT role FROM task_launch_plans WHERE task_id=t.id),COALESCE(p.state='merged',0)
         FROM tasks t LEFT JOIN request_launch l ON l.request_id=t.request_id LEFT JOIN pull_requests p ON p.task_id=t.id WHERE t.id=?1",
        [task],
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
    )?;
    let sampled: Option<Sampled> = tx.query_row(
        "SELECT (SELECT count(*) FROM (SELECT session_id FROM usage_sessions WHERE task_id=?1 UNION SELECT session_id FROM session_steps WHERE task_id=?1)),
          COALESCE(sum(steps),0),COALESCE(sum(subagent_steps),0),COALESCE(sum(looks),0),COALESCE(sum(polls),0) FROM session_steps WHERE task_id=?1",
        [task],
        |row| Ok(Sampled { sessions: row.get(0)?, steps: [row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?] }),
    ).map(|sampled| (sampled.sessions > 0).then_some(sampled))?;
    if sampled.is_some() {
        tx.execute("DELETE FROM task_model_usage WHERE task_id=?1", [task])?;
        tx.execute(
            "INSERT INTO task_model_usage(task_id,model,provider,cost_micros,input,output,cache_read,cache_write)
             SELECT task_id,model,min(provider),sum(cost_micros),sum(input),sum(output),sum(cache_read),sum(cache_write) FROM usage_sessions WHERE task_id=?1 GROUP BY model",
            [task],
        )?;
    }
    let measured: bool = tx.query_row("SELECT EXISTS(SELECT 1 FROM task_model_usage WHERE task_id=?1)", [task], |row| row.get(0))?;
    let (provider, kind): (Option<String>, Option<String>) = tx.query_row(
        "SELECT (SELECT provider FROM model_decisions WHERE task_id=?1 AND kind IN('launch','relaunch') ORDER BY at DESC,id DESC LIMIT 1),
          (SELECT task_kind FROM model_decisions WHERE task_id=?1 AND task_kind IS NOT NULL ORDER BY at DESC,id DESC LIMIT 1)",
        [task],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    let flag = i64::from;
    let sampled_values = sampled.as_ref().map(|s| (s.sessions, s.steps));
    tx.execute(
        "INSERT INTO task_metrics(task_id,project,model,effort,approach,role,outcome,started_at,finished_at,clean,change_rounds,reviewer_rounds,
          interventions,reopened,relaunched,work_ms,waiting_user_ms,queued_ms,blocked_ms,measured,sessions,steps,subagent_steps,looks,polls,
          pr_merged,pr_lines,folded_revision,provider,kind)
         VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,?18,?19,?20,COALESCE(?21,0),COALESCE(?22,0),COALESCE(?23,0),COALESCE(?24,0),COALESCE(?25,0),?26,NULL,?27,?28,?29)
         ON CONFLICT(task_id) DO UPDATE SET project=excluded.project,model=excluded.model,effort=excluded.effort,approach=excluded.approach,
          role=excluded.role,outcome=excluded.outcome,started_at=excluded.started_at,finished_at=excluded.finished_at,clean=excluded.clean,
          change_rounds=excluded.change_rounds,reviewer_rounds=excluded.reviewer_rounds,interventions=excluded.interventions,
          reopened=excluded.reopened,relaunched=excluded.relaunched,work_ms=excluded.work_ms,waiting_user_ms=excluded.waiting_user_ms,
          queued_ms=excluded.queued_ms,blocked_ms=excluded.blocked_ms,measured=excluded.measured,
          sessions=COALESCE(?21,sessions),steps=COALESCE(?22,steps),subagent_steps=COALESCE(?23,subagent_steps),
          looks=COALESCE(?24,looks),polls=COALESCE(?25,polls),pr_merged=excluded.pr_merged,
          folded_revision=excluded.folded_revision,provider=excluded.provider,kind=excluded.kind",
        params![
            task, project, m.model, m.effort, m.approach, role, m.outcome.as_str(), m.started_at, m.finished_at, flag(m.clean),
            m.change_rounds, m.reviewer_rounds, m.interventions, flag(m.reopened), flag(m.relaunched), m.work_ms, m.waiting_user_ms,
            m.queued_ms, m.blocked_ms, flag(measured), sampled_values.map(|(sessions, _)| sessions), sampled_values.map(|(_, s)| s[0]),
            sampled_values.map(|(_, s)| s[1]), sampled_values.map(|(_, s)| s[2]), sampled_values.map(|(_, s)| s[3]),
            flag(pr_merged), revision, provider, kind,
        ],
    )?;
    tx.execute("UPDATE usage_sessions SET folded=1 WHERE task_id=?1", [task])?;
    tx.execute("UPDATE session_steps SET folded=1 WHERE task_id=?1", [task])?;
    Ok(true)
}

/// The samples of tasks that ended over a week ago, once every one of a task's samples was folded:
/// folding it again reads all of them or none.
pub(super) fn samples_of_folded_tasks(tx: &Transaction<'_>, now: i64, limit: i64) -> rusqlite::Result<usize> {
    let folded = "SELECT m.task_id FROM task_metrics m JOIN tasks t ON t.id=m.task_id WHERE t.state IN('done','cancelled') AND m.finished_at<?1
        AND NOT EXISTS(SELECT 1 FROM usage_sessions u WHERE u.task_id=m.task_id AND u.folded=0)
        AND NOT EXISTS(SELECT 1 FROM session_steps s WHERE s.task_id=m.task_id AND s.folded=0)";
    let usage = tx.execute(
        &format!("DELETE FROM usage_sessions WHERE (session_id,model) IN(SELECT session_id,model FROM usage_sessions WHERE task_id IN({folded}) ORDER BY task_id LIMIT ?2)"),
        params![now - SAMPLES_MS, limit],
    )? as i64;
    let steps = tx.execute(
        &format!("DELETE FROM session_steps WHERE session_id IN(SELECT session_id FROM session_steps WHERE task_id IN({folded}) ORDER BY task_id LIMIT ?2)"),
        params![now - SAMPLES_MS, limit - usage],
    )? as i64;
    Ok((usage + steps) as usize)
}

/// The sessions without a task that spent nothing for a week: their days already hold what they
/// spent. Steps of a node no task was known for are not kept.
pub(super) fn idle_sessions_without_task(tx: &Transaction<'_>, now: i64, limit: i64) -> rusqlite::Result<usize> {
    let usage = tx.execute(
        "DELETE FROM usage_sessions WHERE (session_id,model) IN(SELECT session_id,model FROM usage_sessions WHERE task_id IS NULL AND last_at<?1 LIMIT ?2)",
        params![now - SAMPLES_MS, limit],
    )? as i64;
    let steps = tx.execute(
        "DELETE FROM session_steps WHERE session_id IN(SELECT session_id FROM session_steps WHERE task_id IS NULL AND last_at<?1 LIMIT ?2)",
        params![now - SAMPLES_MS, limit - usage],
    )? as i64;
    Ok((usage + steps) as usize)
}

/// Sums of a batch of `task_metrics` rows, by week and by what the week is split by.
const WEEK_SUMS: [(&str, &str); 29] = [
    ("tasks", "count(*)"),
    ("done", "sum(m.outcome='done')"),
    ("clean", "sum(m.clean)"),
    ("change_rounds", "sum(m.change_rounds)"),
    ("reviewer_rounds", "sum(m.reviewer_rounds)"),
    ("interventions", "sum(m.interventions)"),
    ("reopened", "sum(m.reopened)"),
    ("relaunched", "sum(m.relaunched)"),
    ("work_ms", "sum(m.work_ms)"),
    ("waiting_user_ms", "sum(m.waiting_user_ms)"),
    ("queued_ms", "sum(m.queued_ms)"),
    ("blocked_ms", "sum(m.blocked_ms)"),
    ("measured", "sum(m.measured)"),
    ("sessions", "sum(m.sessions)"),
    ("steps", "sum(m.steps)"),
    ("subagent_steps", "sum(m.subagent_steps)"),
    ("looks", "sum(m.looks)"),
    ("polls", "sum(m.polls)"),
    ("merged_prs", "sum(m.pr_merged)"),
    ("pr_lines", "COALESCE(sum(m.pr_lines),0)"),
    ("lined_prs", "count(m.pr_lines)"),
    ("cost_micros", "COALESCE(sum(u.cost),0)"),
    ("costed", "count(u.cost)"),
    ("merged_cost_micros", "COALESCE(sum(CASE WHEN m.pr_merged=1 THEN u.cost END),0)"),
    ("lined_cost_micros", "COALESCE(sum(CASE WHEN m.pr_lines IS NOT NULL THEN u.cost END),0)"),
    ("input", "COALESCE(sum(u.input),0)"),
    ("output", "COALESCE(sum(u.output),0)"),
    ("cache_read", "COALESCE(sum(u.cache_read),0)"),
    ("cache_write", "COALESCE(sum(u.cache_write),0)"),
];

/// Tasks that ended over 90 days ago, added into their week, and what each model spent on them
/// into the day they ended, before their rows go.
pub(super) fn tasks_into_weeks(tx: &Transaction<'_>, now: i64, limit: i64) -> rusqlite::Result<usize> {
    tx.execute_batch("CREATE TEMP TABLE IF NOT EXISTS metric_batch(task_id TEXT PRIMARY KEY); DELETE FROM metric_batch;")?;
    let batch = tx.execute(
        "INSERT INTO metric_batch(task_id) SELECT task_id FROM task_metrics WHERE finished_at<?1 ORDER BY finished_at LIMIT ?2",
        params![now - TASKS_MS, limit],
    )?;
    if batch == 0 {
        return Ok(0);
    }
    let columns: Vec<&str> = WEEK_SUMS.iter().map(|(column, _)| *column).collect();
    let sums: Vec<&str> = WEEK_SUMS.iter().map(|(_, sum)| *sum).collect();
    let added: Vec<String> = columns.iter().map(|column| format!("{column}={column}+excluded.{column}")).collect();
    tx.execute(
        &format!(
            "INSERT INTO metric_weeks(week,project,model,effort,approach,{})
             SELECT date(m.finished_at/1000,'unixepoch','weekday 0','-6 days'),COALESCE(m.project,''),COALESCE(m.model,''),COALESCE(m.effort,''),COALESCE(m.approach,''),{}
             FROM task_metrics m JOIN metric_batch b ON b.task_id=m.task_id
             LEFT JOIN (SELECT task_id,sum(cost_micros) cost,sum(input) input,sum(output) output,sum(cache_read) cache_read,sum(cache_write) cache_write
               FROM task_model_usage WHERE task_id IN(SELECT task_id FROM metric_batch) GROUP BY task_id) u ON u.task_id=m.task_id
             WHERE 1 GROUP BY 1,2,3,4,5
             ON CONFLICT(week,project,model,effort,approach) DO UPDATE SET {}",
            columns.join(","), sums.join(","), added.join(","),
        ),
        [],
    )?;
    tx.execute(
        "INSERT INTO usage_days(day,provider,model,role,cost_micros,costed,input,output,cache_read,cache_write,sessions)
         SELECT date(m.finished_at/1000,'unixepoch'),u.provider,u.model,'node',COALESCE(sum(u.cost_micros),0),count(u.cost_micros),
          sum(u.input),sum(u.output),sum(u.cache_read),sum(u.cache_write),0
         FROM task_model_usage u JOIN task_metrics m ON m.task_id=u.task_id JOIN metric_batch b ON b.task_id=u.task_id
         WHERE 1 GROUP BY 1,2,3
         ON CONFLICT(day,provider,model,role) DO UPDATE SET cost_micros=cost_micros+excluded.cost_micros,costed=costed+excluded.costed,
          input=input+excluded.input,output=output+excluded.output,cache_read=cache_read+excluded.cache_read,cache_write=cache_write+excluded.cache_write",
        [],
    )?;
    tx.execute("DELETE FROM task_model_usage WHERE task_id IN(SELECT task_id FROM metric_batch)", [])?;
    tx.execute("DELETE FROM task_metrics WHERE task_id IN(SELECT task_id FROM metric_batch)", [])?;
    tx.execute("DELETE FROM metric_batch", [])?;
    Ok(batch)
}

#[cfg(test)]
#[path = "metrics_fold_tests.rs"]
pub(crate) mod tests;
