use alondra_core::domain::*;
use anyhow::{Context, Result, ensure};
use rusqlite::{Connection, OptionalExtension, params, types::Value as SqlValue};
use serde::{Serialize, de::DeserializeOwned};
use serde_json::{Map, Value, json};
use std::cell::Cell;
use std::collections::BTreeMap;

thread_local! {
    static WRITES: Cell<u64> = const { Cell::new(0) };
    static SCANS: Cell<u64> = const { Cell::new(0) };
}
#[cfg(any(test, feature = "test-hooks"))]
/// Writing statements and full-table scans issued on this thread so far. Tests
/// pin a command's cost with these instead of the wall clock.
pub(crate) fn statement_counts() -> (u64, u64) {
    (WRITES.with(Cell::get), SCANS.with(Cell::get))
}
pub(super) fn exec(c: &Connection, sql: &str, params: impl rusqlite::Params) -> Result<usize> {
    WRITES.with(|writes| writes.set(writes.get() + 1));
    Ok(c.execute(sql, params)?)
}

fn value(v: &impl Serialize) -> Result<Value> {
    Ok(serde_json::to_value(v)?)
}
fn decode<T: DeserializeOwned>(v: Value) -> Result<T> {
    Ok(serde_json::from_value(v)?)
}
fn object(v: &impl Serialize) -> Result<Map<String, Value>> {
    Ok(value(v)?.as_object().context("expected object")?.clone())
}
fn strip(row: &mut Map<String, Value>, names: &[&str]) {
    for n in names {
        row.remove(*n);
    }
}
fn sql(v: &Value) -> Result<SqlValue> {
    Ok(match v {
        Value::Null => SqlValue::Null,
        Value::Bool(b) => SqlValue::Integer(i64::from(*b)),
        Value::Number(n) => SqlValue::Integer(n.as_i64().context("integer outside SQLite range")?),
        Value::String(s) => SqlValue::Text(s.clone()),
        _ => SqlValue::Text(serde_json::to_string(v)?),
    })
}
/// All table and column identifiers originate exclusively in this module.
fn upsert(c: &Connection, table: &str, keys: &[&str], row: Map<String, Value>) -> Result<()> {
    let columns: Vec<_> = row.keys().collect();
    let names = columns
        .iter()
        .map(|x| x.as_str())
        .collect::<Vec<_>>()
        .join(",");
    let marks = (1..=columns.len())
        .map(|i| format!("?{i}"))
        .collect::<Vec<_>>()
        .join(",");
    let changes = columns
        .iter()
        .filter(|s| !keys.contains(&s.as_str()))
        .map(|s| format!("{s}=excluded.{s}"))
        .collect::<Vec<_>>()
        .join(",");
    let action = if changes.is_empty() {
        "DO NOTHING".into()
    } else {
        format!("DO UPDATE SET {changes}")
    };
    let query = format!(
        "INSERT INTO {table}({names}) VALUES({marks}) ON CONFLICT({}) {action}",
        keys.join(",")
    );
    let values: Vec<_> = columns
        .iter()
        .map(|k| sql(&row[*k]))
        .collect::<Result<_>>()?;
    exec(c, &query, rusqlite::params_from_iter(values))?;
    Ok(())
}
fn put(c: &Connection, table: &str, keys: &[&str], v: Value) -> Result<()> {
    upsert(
        c,
        table,
        keys,
        v.as_object().context("expected row")?.clone(),
    )
}
fn rows(c: &Connection, table: &str, bools: &[&str], jsons: &[&str]) -> Result<Vec<Value>> {
    SCANS.with(|scans| scans.set(scans.get() + 1));
    let mut q = c.prepare(&format!("SELECT * FROM {table} ORDER BY rowid"))?;
    let names = q
        .column_names()
        .into_iter()
        .map(str::to_owned)
        .collect::<Vec<_>>();
    let mut query = q.query([])?;
    let mut result = vec![];
    while let Some(row) = query.next()? {
        let mut obj = Map::new();
        for (i, n) in names.iter().enumerate() {
            let v: SqlValue = row.get(i)?;
            let v = match v {
                SqlValue::Null => Value::Null,
                SqlValue::Integer(i) if bools.contains(&n.as_str()) => json!(i != 0),
                SqlValue::Integer(i) => json!(i),
                SqlValue::Text(s) if jsons.contains(&n.as_str()) => serde_json::from_str(&s)?,
                SqlValue::Text(s) => json!(s),
                _ => anyhow::bail!("unsupported domain column {table}.{n}"),
            };
            obj.insert(n.clone(), v);
        }
        result.push(Value::Object(obj));
    }
    Ok(result)
}
fn take(row: &mut Value, name: &str) -> Value {
    row.as_object_mut()
        .unwrap()
        .remove(name)
        .unwrap_or(Value::Null)
}
fn s(v: &Value) -> Result<&str> {
    v.as_str().context("missing relation ID")
}

fn schedule_row(schedule: &Schedule) -> Result<Map<String, Value>> {
    let mut row = object(schedule)?;
    strip(&mut row, &["paid_account_grant"]);
    Ok(row)
}

/// Save schedule definitions before their runs so the run FK and claim-revision
/// guard see the current definition. Callers own the transaction containing any
/// Request/Task rows linked by a newly sent run.
#[cfg(any(test, feature = "test-hooks"))]
pub(super) fn save_schedule_state(c: &Connection, schedule: &ScheduleState) -> Result<()> {
    schedule.validate()?;
    for definition in schedule.schedules.values() {
        upsert(c, "schedules", &["id"], schedule_row(definition)?)?;
    }
    for run in schedule.runs.values() {
        upsert(c, "schedule_runs", &["id"], object(run)?)?;
        if let (Some(request_id), Some(snapshot)) = (&run.request_id, &run.definition_snapshot) {
            remember_schedule_request_tools(c, request_id, &snapshot.disallowed_tools)?;
        }
    }
    Ok(())
}

pub(super) fn load_schedule_state(c: &Connection) -> Result<ScheduleState> {
    let mut state = ScheduleState::default();
    for row in rows(
        c,
        "schedules",
        &["enabled"],
        &["options", "disallowed_tools"],
    )? {
        let definition: Schedule = decode(row)?;
        ensure!(
            state
                .schedules
                .insert(definition.id.clone(), definition)
                .is_none(),
            "duplicate schedule identity"
        );
    }
    for row in rows(c, "schedule_runs", &[], &["definition_snapshot", "launch"])? {
        let run: ScheduleRun = decode(row)?;
        ensure!(
            state.runs.insert(run.id.clone(), run).is_none(),
            "duplicate schedule run identity"
        );
    }
    state.validate()?;
    Ok(state)
}

/// Immutable launch-time policy belongs to the Request, even if the Schedule
/// definition is subsequently edited or tombstoned. An exact replay is safe.
pub(super) fn remember_schedule_request_tools(
    c: &Connection,
    request_id: &RequestId,
    patterns: &[String],
) -> Result<()> {
    let encoded = serde_json::to_string(patterns)?;
    let stored: Option<String> = c
        .query_row(
            "SELECT patterns FROM schedule_request_tools WHERE request_id=?1",
            [&request_id.0],
            |r| r.get(0),
        )
        .optional()?;
    if let Some(stored) = stored {
        ensure!(
            serde_json::from_str::<Vec<String>>(&stored)? == patterns,
            "schedule Request tool policy is immutable"
        );
    } else {
        exec(
            c,
            "INSERT INTO schedule_request_tools(request_id,patterns) VALUES(?1,?2)",
            params![request_id.0, encoded],
        )?;
    }
    Ok(())
}

pub(super) fn load_schedule_request_tools(
    c: &Connection,
    request_id: &RequestId,
) -> Result<Option<Vec<String>>> {
    c.query_row(
        "SELECT patterns FROM schedule_request_tools WHERE request_id=?1",
        [&request_id.0],
        |r| r.get::<_, String>(0),
    )
    .optional()?
    .map(|patterns| serde_json::from_str(&patterns).map_err(Into::into))
    .transpose()
}

pub(super) fn remember_request_launch(c: &Connection, request_id: &RequestId, launch: &super::RequestLaunch) -> Result<()> {
    exec(
        c,
        "INSERT INTO request_launch(request_id,project,options,issue,issue_text,attachments) VALUES(?1,?2,?3,?4,?5,?6)",
        params![
            request_id.0,
            launch.project,
            serde_json::to_string(&launch.options)?,
            launch.issue,
            launch.issue_text,
            serde_json::to_string(&launch.attachments)?
        ],
    )?;
    Ok(())
}

pub(super) fn load_request_launch(c: &Connection, request_id: &RequestId) -> Result<Option<super::RequestLaunch>> {
    c.query_row(
        "SELECT project,options,issue,issue_text,attachments FROM request_launch WHERE request_id=?1",
        [&request_id.0],
        |r| Ok((r.get::<_, Option<String>>(0)?, r.get::<_, String>(1)?, r.get::<_, Option<String>>(2)?, r.get::<_, Option<String>>(3)?, r.get::<_, String>(4)?)),
    )
    .optional()?
    .map(|(project, options, issue, issue_text, attachments)| {
        Ok(super::RequestLaunch {
            project,
            options: serde_json::from_str(&options)?,
            issue,
            issue_text,
            attachments: serde_json::from_str(&attachments)?,
        })
    })
    .transpose()
}

pub(super) fn remember_schedule_failed_launch(
    c: &Connection,
    request_id: &RequestId,
) -> Result<()> {
    c.execute(
        "INSERT OR IGNORE INTO schedule_failed_launches(request_id) VALUES(?1)",
        [&request_id.0],
    )?;
    Ok(())
}

pub(super) fn was_schedule_launch_failed(c: &Connection, request_id: &RequestId) -> Result<bool> {
    Ok(c.query_row(
        "SELECT 1 FROM schedule_failed_launches WHERE request_id=?1",
        [&request_id.0],
        |_| Ok(()),
    )
    .optional()?
    .is_some())
}

#[cfg(test)]
#[path = "repository/schedule_tests.rs"]
mod schedule_tests;

fn save_attachments(
    c: &Connection,
    interaction: &InteractionId,
    kind: &str,
    feedback_ordinal: Option<i64>,
    attachments: &[AttachmentRef],
) -> Result<()> {
    for (ordinal, a) in attachments.iter().enumerate() {
        exec(c, "INSERT INTO interaction_attachments(interaction_id,kind,feedback_ordinal,ordinal,id,name,content_digest,content_reference,bytes) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9)",
            params![interaction.0, kind, feedback_ordinal, ordinal as i64, a.id, a.name, a.content_digest, a.content_reference, a.bytes as i64])?;
    }
    Ok(())
}
/// Entries of `after` that are new or differ from `before`, with their prior value.
fn changed<'a, K: Ord, V: PartialEq>(
    before: &'a BTreeMap<K, V>,
    after: &'a BTreeMap<K, V>,
) -> impl Iterator<Item = (&'a V, Option<&'a V>)> {
    after.iter().filter_map(move |(key, value)| {
        let prior = before.get(key);
        (prior != Some(value)).then_some((value, prior))
    })
}
fn task_row(t: &Task) -> Result<Map<String, Value>> {
    let mut row = object(t)?;
    strip(&mut row, &["dependencies", "stack", "documents", "model", "wrap_up", "delivered_dependencies", "blocks", "verdict", "review_rounds", "priority", "ending", "stalls", "stall_closing", "open_pr", "worktrees", "account", "provider", "selection", "pending_selection", "allowed", "project", "continues"]);
    // The column's check predates the two states that split `in_review`; the state is derived
    // from the facts when the store is read, so either of them is kept as the old value.
    if matches!(t.state, TaskState::AwaitingBlocker | TaskState::ForYourReview) {
        row.insert("state".into(), json!("in_review"));
    }
    Ok(row)
}
fn save_relations(c: &Connection, t: &Task) -> Result<()> {
    exec(
        c,
        "INSERT INTO task_relations(task_id,delivered_dependencies,blocks,verdict,review_rounds) VALUES(?1,?2,?3,?4,?5) \
         ON CONFLICT(task_id) DO UPDATE SET delivered_dependencies=excluded.delivered_dependencies,blocks=excluded.blocks,\
         verdict=excluded.verdict,review_rounds=excluded.review_rounds",
        params![
            t.id.0,
            serde_json::to_string(&t.delivered_dependencies)?,
            serde_json::to_string(&t.blocks)?,
            t.verdict.as_ref().map(serde_json::to_string).transpose()?,
            t.review_rounds as i64
        ],
    )?;
    Ok(())
}
fn save_review_findings(c: &Connection, reviewer: &Task, verdict: &Verdict, revision: Revision) -> Result<()> {
    let kind = match verdict {
        Verdict::Approved { .. } => "approved",
        Verdict::ChangesRequested { .. } => "changes_requested",
    };
    for reviewed in &reviewer.blocks {
        exec(
            c,
            "INSERT OR IGNORE INTO review_findings(task_id,reviewer_task_id,at_revision,kind,serious_findings) VALUES(?1,?2,?3,?4,?5)",
            params![reviewed.0, reviewer.id.0, revision as i64, kind, verdict.serious_findings()],
        )?;
    }
    Ok(())
}
fn save_wrap_up(c: &Connection, t: &Task) -> Result<()> {
    let w = &t.wrap_up;
    let node_end = match w.node {
        Some(end) => Some(serde_json::to_value(end)?.as_str().context("node end is a string")?.to_owned()),
        None => None,
    };
    exec(
        c,
        "INSERT INTO task_wrap_ups(task_id,asked_at,cleaned_at,close_cancelled,node_end) VALUES(?1,?2,?3,?4,?5) \
         ON CONFLICT(task_id) DO UPDATE SET asked_at=excluded.asked_at,cleaned_at=excluded.cleaned_at,\
         close_cancelled=excluded.close_cancelled,node_end=excluded.node_end",
        params![t.id.0, w.asked_at.map(|r| r as i64), w.cleaned_at.map(|r| r as i64), w.close_cancelled as i64, node_end],
    )?;
    exec(c, "DELETE FROM task_post_merge_turns WHERE task_id=?1", [&t.id.0])?;
    match &w.post_merge_work {
        Some(work) => {
            exec(
                c,
                "INSERT INTO task_post_merge_work(task_id,reason,declared_at) VALUES(?1,?2,?3) \
                 ON CONFLICT(task_id) DO UPDATE SET reason=excluded.reason,declared_at=excluded.declared_at",
                params![t.id.0, work.reason, work.declared_at as i64],
            )?;
            if work.turn_since {
                exec(c, "INSERT INTO task_post_merge_turns(task_id) VALUES(?1)", [&t.id.0])?;
            }
        }
        None => {
            exec(c, "DELETE FROM task_post_merge_work WHERE task_id=?1", [&t.id.0])?;
        }
    }
    Ok(())
}
fn save_grants(c: &Connection, t: &Task) -> Result<()> {
    exec(c, "DELETE FROM task_permission_grants WHERE task_id=?1", [&t.id.0])?;
    for (card, grant) in &t.allowed {
        exec(
            c,
            "INSERT INTO task_permission_grants(task_id,card,allows,uses) VALUES(?1,?2,?3,?4)",
            params![t.id.0, card.0, serde_json::to_string(&grant.allows)?, grant.uses as i64],
        )?;
    }
    Ok(())
}
fn save_worktrees(c: &Connection, t: &Task) -> Result<()> {
    for (path, w) in &t.worktrees {
        let text = |value: Value| value.as_str().map(str::to_owned).context("a worktree field is a string");
        exec(
            c,
            "INSERT INTO task_worktree_records(task_id,path,origin,branch,discard,end) VALUES(?1,?2,?3,?4,?5,?6) \
             ON CONFLICT(task_id,path) DO UPDATE SET origin=excluded.origin,branch=excluded.branch,discard=excluded.discard,end=excluded.end",
            params![
                t.id.0,
                path,
                text(serde_json::to_value(w.origin)?)?,
                w.branch,
                w.discard as i64,
                w.end.map(|end| serde_json::to_value(end).map_err(anyhow::Error::from).and_then(text)).transpose()?
            ],
        )?;
    }
    Ok(())
}
/// Dependencies and document links; stacks follow the interactions they reference.
fn save_task_links(c: &Connection, t: &Task, before: Option<&Task>) -> Result<()> {
    if before.is_none_or(|b| b.dependencies != t.dependencies) {
        exec(c, "DELETE FROM task_dependencies WHERE task_id=?1", [&t.id.0])?;
        for dep in &t.dependencies {
            put(
                c,
                "task_dependencies",
                &["task_id", "depends_on_id"],
                json!({"task_id":t.id,"depends_on_id":dep}),
            )?;
        }
    }
    for (doc, role) in &t.documents {
        if before.is_none_or(|b| b.documents.get(doc) != Some(role)) {
            put(
                c,
                "task_documents",
                &["task_id", "document_id"],
                json!({"task_id":t.id,"document_id":doc,"role":role}),
            )?;
        }
    }
    Ok(())
}
fn save_task_stack(c: &Connection, t: &Task, before: Option<&Task>) -> Result<()> {
    if before.is_some_and(|b| b.stack == t.stack) {
        return Ok(());
    }
    exec(c, "DELETE FROM task_stacks WHERE task_id=?1", [&t.id.0])?;
    if let Some(stack) = &t.stack {
        let mut row = object(stack)?;
        row.insert("task_id".into(), json!(t.id));
        upsert(c, "task_stacks", &["task_id"], row)?;
    }
    Ok(())
}
fn save_pull_request(c: &Connection, p: &PullRequest, before: Option<&PullRequest>) -> Result<()> {
    let mut row = object(p)?;
    row.remove("review");
    upsert(c, "pull_requests", &["id"], row)?;
    if p.review.is_some() && before.is_none_or(|b| b.review.is_none()) {
        put(c, "pull_request_reviews", &["pr_id"], json!({"pr_id":p.id}))?;
    }
    Ok(())
}
fn save_pr_decisions(c: &Connection, p: &PullRequest, before: Option<&PullRequest>) -> Result<()> {
    let Some(r) = &p.review else { return Ok(()) };
    let earlier = before.and_then(|b| b.review.as_ref());
    for (index, d) in r.decisions.iter().enumerate() {
        if earlier.is_some_and(|e| e.decisions.get(index) == Some(d)) {
            continue;
        }
        let mut row = object(d)?;
        row.extend(object(&json!({"pr_id":p.id,"task_id":p.task_id,"ordinal":index}))?);
        upsert(c, "pr_decisions", &["pr_id", "ordinal"], row)?;
    }
    Ok(())
}
fn interaction_row(i: &Interaction) -> Result<Map<String, Value>> {
    let mut row = object(i)?;
    row.remove("attachments");
    strip(
        &mut row,
        &[
            "scope",
            "author",
            "recipient",
            "subject",
            "payload",
            "order",
        ],
    );
    row.insert("ordering".into(), json!(i.order));
    row.insert("clarification".into(), json!(i.clarification));
    row.insert("snoozed_until".into(), json!(i.snoozed_until));
    row.insert("resurfaced_at_revision".into(), json!(i.resurfaced_at_revision));
    row.insert("system_card".into(), json!(i.system_card));
    let (scope, task, request) = match &i.scope {
        Scope::Task { task_id } => ("task", json!(task_id), Value::Null),
        Scope::Orchestrator { request_id } => ("orchestrator", Value::Null, json!(request_id)),
    };
    row.extend(object(
        &json!({"scope_kind":scope,"task_id":task,"request_id":request}),
    )?);
    let (kind, agent, generation) = match &i.author {
        Author::Agent { agent_id } => ("agent", json!(agent_id), Value::Null),
        Author::Orchestrator { generation } => ("orchestrator", Value::Null, json!(generation)),
        Author::System => ("system", Value::Null, Value::Null),
    };
    row.extend(object(
        &json!({"author_kind":kind,"author_agent_id":agent,"author_generation":generation}),
    )?);
    let (kind, agent) = match &i.recipient {
        Recipient::User => ("user", Value::Null),
        Recipient::Agent { agent_id } => ("agent", json!(agent_id)),
        Recipient::Orchestrator => ("orchestrator", Value::Null),
    };
    row.extend(object(&json!({"recipient_kind":kind,"recipient_agent_id":agent,"options":null,"answer":null,"is_final":null,"acceptance_user":null,"acceptance_revision":null,"acceptance_valid":null}))?);
    match &i.payload {
        InteractionPayload::Information { .. } => {
            row.insert("kind".into(), json!("information"));
        }
        InteractionPayload::Question { options, answer }
        | InteractionPayload::ActionRequest { options, answer } => {
            row.insert(
                "kind".into(),
                json!(
                    if matches!(&i.payload, InteractionPayload::Question { .. }) {
                        "question"
                    } else {
                        "action_request"
                    }
                ),
            );
            row.insert("options".into(), json!(options));
            let mut answer_row = json!(answer);
            if let Some(value) = answer_row.as_object_mut() {
                value.remove("attachments");
            }
            row.insert("answer".into(), answer_row);
        }
        InteractionPayload::Delivery {
            is_final,
            acceptance,
            ..
        } => {
            row.insert("kind".into(), json!("delivery"));
            row.insert("is_final".into(), json!(is_final));
            if let Some(a) = acceptance {
                row.extend(object(&json!({"acceptance_user":a.user_id,"acceptance_revision":a.at_revision,"acceptance_valid":a.valid}))?);
            }
        }
    }
    Ok(row)
}
/// Attachments, targets and history rows are append-only, so only what `before`
/// lacks is written. With no `before`, everything of the interaction is written.
fn save_interaction_relations(
    c: &Connection,
    i: &Interaction,
    before: Option<&Interaction>,
) -> Result<()> {
    if before.is_none() {
        save_attachments(c, &i.id, "interaction", None, &i.attachments)?;
    }
    let task = match &i.scope {
        Scope::Task { task_id } => Some(task_id),
        _ => None,
    };
    if let Some(subject) = &i.subject
        && before.is_none_or(|b| b.subject.as_ref() != Some(subject))
    {
        let mut row = object(
            &json!({"interaction_id":i.id,"task_id":task,"pr_id":null,"document_id":null,"revision_id":null}),
        )?;
        match subject {
            InteractionSubject::PullRequest { pr_id } => {
                row.insert("pr_id".into(), json!(pr_id));
            }
            InteractionSubject::DocumentReview {
                document_id,
                revision_id,
            } => {
                row.insert("document_id".into(), json!(document_id));
                row.insert("revision_id".into(), json!(revision_id));
            }
        }
        upsert(c, "interaction_subjects", &["interaction_id"], row)?;
    }
    match &i.payload {
        InteractionPayload::Question {
            answer: Some(answer),
            ..
        }
        | InteractionPayload::ActionRequest {
            answer: Some(answer),
            ..
        } => {
            let answered_before = before.is_some_and(|b| {
                matches!(
                    &b.payload,
                    InteractionPayload::Question { answer: Some(_), .. }
                        | InteractionPayload::ActionRequest { answer: Some(_), .. }
                )
            });
            if !answered_before {
                save_attachments(c, &i.id, "answer", None, &answer.attachments)?;
            }
        }
        InteractionPayload::Information { read_by } => {
            for user in read_by {
                let read_before = before.is_some_and(|b| {
                    matches!(&b.payload, InteractionPayload::Information { read_by } if read_by.contains(user))
                });
                if !read_before {
                    put(
                        c,
                        "interaction_reads",
                        &["interaction_id", "user_id"],
                        json!({"interaction_id":i.id,"user_id":user}),
                    )?;
                }
            }
        }
        InteractionPayload::Delivery {
            targets, feedback, ..
        } => {
            if before.is_none() {
                for (index, target) in targets.iter().enumerate() {
                    let mut row = object(
                        &json!({"interaction_id":i.id,"ordinal":index,"task_id":task,"text":null,"document_id":null,"revision_id":null,"pr_id":null,"head":null}),
                    )?;
                    row.extend(object(target)?);
                    upsert(c, "delivery_targets", &["interaction_id", "ordinal"], row)?;
                }
            }
            let earlier = before.and_then(|b| match &b.payload {
                InteractionPayload::Delivery { feedback, .. } => Some(feedback.as_slice()),
                _ => None,
            });
            for (index, f) in feedback.iter().enumerate() {
                if earlier.is_some_and(|e| e.get(index) == Some(f)) {
                    continue;
                }
                let mut row = object(f)?;
                row.remove("attachments");
                row.insert("merge_failure".into(), json!(f.merge_failure));
                row.extend(object(&json!({"interaction_id":i.id,"ordinal":index}))?);
                upsert(c, "delivery_feedback", &["interaction_id", "ordinal"], row)?;
                if earlier.is_none_or(|e| e.len() <= index) {
                    save_attachments(c, &i.id, "feedback", Some(index as i64), &f.attachments)?;
                }
            }
        }
        _ => (),
    }
    Ok(())
}
fn save_document_review(c: &Connection, r: &DocumentReview, before: Option<&DocumentReview>) -> Result<()> {
    if before.is_none() {
        put(
            c,
            "document_reviews",
            &["task_id", "document_id"],
            json!({"task_id":r.task_id,"document_id":r.document_id}),
        )?;
    }
    for (index, d) in r.decisions.iter().enumerate() {
        if before.is_some_and(|b| b.decisions.get(index) == Some(d)) {
            continue;
        }
        let mut row = object(d)?;
        row.extend(object(
            &json!({"task_id":r.task_id,"document_id":r.document_id,"ordinal":index}),
        )?);
        upsert(
            c,
            "document_decisions",
            &["task_id", "document_id", "ordinal"],
            row,
        )?;
    }
    Ok(())
}
fn save_schedule_changes(c: &Connection, before: &ScheduleState, after: &ScheduleState) -> Result<()> {
    for (definition, _) in changed(&before.schedules, &after.schedules) {
        upsert(c, "schedules", &["id"], schedule_row(definition)?)?;
    }
    for (run, prior) in changed(&before.runs, &after.runs) {
        upsert(c, "schedule_runs", &["id"], object(run)?)?;
        if let (Some(request_id), Some(snapshot)) = (&run.request_id, &run.definition_snapshot)
            && prior.is_none_or(|p| p.request_id.is_none() || p.definition_snapshot.is_none())
        {
            remember_schedule_request_tools(c, request_id, &snapshot.disallowed_tools)?;
        }
    }
    Ok(())
}
fn kept<K: Ord + std::fmt::Debug, V>(
    what: &str,
    before: &BTreeMap<K, V>,
    after: &BTreeMap<K, V>,
) -> Result<()> {
    if let Some(lost) = before.keys().find(|k| !after.contains_key(k)) {
        anyhow::bail!("domain persistence cannot remove {what} {lost:?}");
    }
    Ok(())
}
/// `save_changes` only inserts and updates. A rule that removed an entry would
/// leave its row behind, so such a change is refused instead of half-persisted.
fn ensure_nothing_removed(before: &DomainState, after: &DomainState) -> Result<()> {
    ensure!(before.orchestrator_session.is_none() || after.orchestrator_session.is_some(),
        "domain persistence cannot forget the orchestrator's session");
    kept("request", &before.requests, &after.requests)?;
    kept("agent", &before.agents, &after.agents)?;
    kept("task", &before.tasks, &after.tasks)?;
    kept("document", &before.documents, &after.documents)?;
    kept("document revision", &before.document_revisions, &after.document_revisions)?;
    kept("pull request", &before.pull_requests, &after.pull_requests)?;
    kept("interaction", &before.interactions, &after.interactions)?;
    kept("merge authorization", &before.merge_authorizations, &after.merge_authorizations)?;
    kept("stack readiness", &before.stack_readiness, &after.stack_readiness)?;
    kept("hook hold", &before.holds, &after.holds)?;
    kept("task checklist", &before.checklists, &after.checklists)?;
    kept("delivery checklist", &before.checklist_marks, &after.checklist_marks)?;
    kept("node's last words", &before.last_words, &after.last_words)?;
    kept("approved plan request", &before.request_plans, &after.request_plans)?;
    kept("plan review", &before.plan_reviews, &after.plan_reviews)?;
    if let Some(lost) = before.lifecycle_journal.keys().find(|task| !after.lifecycle_journal.contains_key(task)) {
        anyhow::bail!("domain persistence cannot remove lifecycle journal {lost:?}");
    }
    for (task, journal) in &before.lifecycle_journal {
        ensure!(after.lifecycle_journal[task].len() >= journal.len(), "the lifecycle journal of task {task} is append-only");
    }
    kept("schedule", &before.schedule.schedules, &after.schedule.schedules)?;
    kept("schedule run", &before.schedule.runs, &after.schedule.runs)?;
    for (id, task) in &before.tasks {
        kept("task document link", &task.documents, &after.tasks[id].documents)?;
    }
    for (id, pr) in &before.pull_requests {
        let (Some(review), Some(next)) = (&pr.review, &after.pull_requests[id].review) else {
            ensure!(
                pr.review.is_none(),
                "domain persistence cannot remove a pull request review"
            );
            continue;
        };
        ensure!(
            next.decisions.len() >= review.decisions.len(),
            "domain persistence cannot remove pull request decisions"
        );
    }
    for review in &before.document_reviews {
        let Some(next) = after
            .document_reviews
            .iter()
            .find(|r| r.task_id == review.task_id && r.document_id == review.document_id)
        else {
            anyhow::bail!("domain persistence cannot remove a document review");
        };
        ensure!(
            next.decisions.len() >= review.decisions.len(),
            "domain persistence cannot remove document review decisions"
        );
    }
    Ok(())
}
/// Write only what changed between two validated states. Rows are never
/// removed except the per-Task dependency and stack sets, which are replaced.
pub(super) fn save_changes(
    c: &Connection,
    journal: &mut alondra_store::LifecycleTransaction<'_>,
    before: &DomainState,
    after: &DomainState,
) -> Result<()> {
    ensure_nothing_removed(before, after)?;
    if before.revision != after.revision
        || before.orchestrator_generation != after.orchestrator_generation
    {
        exec(
            c,
            "UPDATE domain_meta SET revision=?1,orchestrator_generation=?2 WHERE singleton=1",
            params![after.revision as i64, after.orchestrator_generation as i64],
        )?;
    }
    if before.orchestrator_session != after.orchestrator_session {
        if let Some(session) = &after.orchestrator_session {
            exec(c, "INSERT INTO orchestrator_session(singleton,session_id) VALUES(1,?1) ON CONFLICT(singleton) DO UPDATE SET session_id=excluded.session_id", params![session.0])?;
        }
    }
    for (x, _) in changed(&before.requests, &after.requests) {
        let mut row = object(x)?;
        strip(&mut row, &["maintenance", "message", "cleared", "origin"]);
        upsert(c, "requests", &["id"], row)?;
        if x.maintenance {
            exec(c, "INSERT OR IGNORE INTO maintenance_requests(request_id) VALUES(?1)", params![x.id.0])?;
        }
        if x.message {
            exec(c, "INSERT OR IGNORE INTO message_requests(request_id) VALUES(?1)", params![x.id.0])?;
        }
        if x.cleared {
            exec(c, "INSERT OR IGNORE INTO cleared_requests(request_id) VALUES(?1)", params![x.id.0])?;
        }
        if x.origin != RequestOrigin::User {
            exec(c, "INSERT INTO request_origins(request_id,origin) VALUES(?1,?2) ON CONFLICT(request_id) DO UPDATE SET origin=excluded.origin",
                params![x.id.0, serde_json::to_string(&x.origin)?])?;
        }
    }
    for (x, _) in changed(&before.agents, &after.agents) {
        upsert(c, "agents", &["id"], object(x)?)?;
    }
    let tasks: Vec<_> = changed(&before.tasks, &after.tasks).collect();
    for (x, prior) in &tasks {
        upsert(c, "tasks", &["id"], task_row(x)?)?;
        if let Some(model) = x.model.as_ref().filter(|_| prior.is_none_or(|p| p.model != x.model)) {
            let mut row = object(model)?;
            row.insert("task_id".into(), json!(x.id));
            upsert(c, "task_models", &["task_id"], row)?;
        }
        if let Some(project) = x.project.as_ref().filter(|_| prior.is_none_or(|p| p.project != x.project)) {
            exec(c, "INSERT INTO task_projects(task_id,project) VALUES(?1,?2) ON CONFLICT(task_id) DO UPDATE SET project=excluded.project", params![x.id.0, project])?;
        }
        if let Some(continues) = x.continues.as_ref().filter(|_| prior.is_none_or(|p| p.continues != x.continues)) {
            exec(c, "INSERT INTO task_continuations(task_id,continues) VALUES(?1,?2) ON CONFLICT(task_id) DO UPDATE SET continues=excluded.continues", params![x.id.0, continues.0])?;
        }
        if prior.map_or(x.account.is_some(), |p| p.account != x.account) {
            match &x.account {
                Some(account) => exec(c, "INSERT INTO task_accounts(task_id,account) VALUES(?1,?2) ON CONFLICT(task_id) DO UPDATE SET account=excluded.account", params![x.id.0, account.as_str()])?,
                None => exec(c, "DELETE FROM task_accounts WHERE task_id=?1", params![x.id.0])?,
            };
        }
        if prior.is_none_or(|p| p.wrap_up != x.wrap_up) {
            save_wrap_up(c, x)?;
        }
        if prior.map_or(!x.worktrees.is_empty(), |p| p.worktrees != x.worktrees) {
            save_worktrees(c, x)?;
        }
        if prior.map_or(!x.allowed.is_empty(), |p| p.allowed != x.allowed) {
            save_grants(c, x)?;
        }
        if prior.map_or(
            !x.delivered_dependencies.is_empty() || !x.blocks.is_empty() || x.verdict.is_some() || x.review_rounds != 0,
            |p| (&p.delivered_dependencies, &p.blocks, &p.verdict, p.review_rounds) != (&x.delivered_dependencies, &x.blocks, &x.verdict, x.review_rounds),
        ) {
            save_relations(c, x)?;
        }
        if let Some(verdict) = x.verdict.as_ref().filter(|_| prior.is_none_or(|p| p.verdict != x.verdict)) {
            save_review_findings(c, x, verdict, after.revision)?;
        }
        if prior.map_or(!x.priority.is_normal(), |p| p.priority != x.priority) {
            exec(
                c,
                "INSERT INTO task_priorities(task_id,priority) VALUES(?1,?2) ON CONFLICT(task_id) DO UPDATE SET priority=excluded.priority",
                params![x.id.0, serde_json::to_value(x.priority)?.as_str().context("priority is a string")?],
            )?;
        }
        if prior.map_or(x.stalls != 0 || x.stall_closing.is_some(), |p| p.stalls != x.stalls || p.stall_closing != x.stall_closing) {
            exec(
                c,
                "INSERT INTO task_stalls(task_id,stalls,closing) VALUES(?1,?2,?3) ON CONFLICT(task_id) DO UPDATE SET stalls=excluded.stalls,closing=excluded.closing",
                params![x.id.0, x.stalls as i64, x.stall_closing.as_ref().map(serde_json::to_string).transpose()?],
            )?;
        }
        if prior.map_or(x.open_pr.is_some(), |p| p.open_pr != x.open_pr) {
            match &x.open_pr {
                Some(pending) => exec(
                    c,
                    "INSERT INTO task_open_prs(task_id,pending) VALUES(?1,?2) ON CONFLICT(task_id) DO UPDATE SET pending=excluded.pending",
                    params![x.id.0, serde_json::to_string(pending)?],
                )?,
                None => exec(c, "DELETE FROM task_open_prs WHERE task_id=?1", params![x.id.0])?,
            };
        }
    }
    for (x, _) in changed(&before.documents, &after.documents) {
        upsert(c, "documents", &["id"], object(x)?)?;
    }
    for (x, _) in changed(&before.document_revisions, &after.document_revisions) {
        upsert(c, "document_revisions", &["id"], object(x)?)?;
    }
    for (t, prior) in &tasks {
        save_task_links(c, t, *prior)?;
    }
    let pull_requests: Vec<_> = changed(&before.pull_requests, &after.pull_requests).collect();
    for (p, prior) in &pull_requests {
        save_pull_request(c, p, *prior)?;
    }
    // A card the lifecycle requires exists before the event that requires it, and is answered or
    // withdrawn only after the event that no longer requires it: new cards go before the journal,
    // changes to existing ones after it.
    let interactions: Vec<_> = changed(&before.interactions, &after.interactions).collect();
    for (i, prior) in interactions.iter().filter(|(_, prior)| prior.is_none()) {
        upsert(c, "interactions", &["id"], interaction_row(i)?)?;
        save_interaction_relations(c, i, *prior)?;
    }
    for r in &after.document_reviews {
        let prior = before
            .document_reviews
            .iter()
            .find(|b| b.task_id == r.task_id && b.document_id == r.document_id);
        if prior != Some(r) {
            save_document_review(c, r, prior)?;
        }
    }
    for (p, prior) in &pull_requests {
        save_pr_decisions(c, p, *prior)?;
    }
    for (a, _) in changed(&before.merge_authorizations, &after.merge_authorizations) {
        upsert(c, "merge_authorizations", &["id"], object(a)?)?;
    }
    for (t, prior) in &tasks {
        save_task_stack(c, t, *prior)?;
    }
    for (r, _) in changed(&before.stack_readiness, &after.stack_readiness) {
        upsert(c, "stack_readiness", &["task_id"], object(r)?)?;
    }
    for (id, hold) in &after.holds {
        if before.holds.get(id) != Some(hold) {
            let row = object(&json!({"interaction_id": id, "hold": hold}))?;
            upsert(c, "interaction_holds", &["interaction_id"], row)?;
        }
    }
    for (task_id, checklist) in after.checklists.iter().filter(|(id, c)| before.checklists.get(*id) != Some(*c)) {
        let row = object(&json!({"task_id": task_id, "plan_interaction_id": checklist.plan, "items": checklist.items}))?;
        upsert(c, "task_checklists", &["task_id"], row)?;
    }
    for (id, marks) in after.checklist_marks.iter().filter(|(id, m)| before.checklist_marks.get(*id) != Some(*m)) {
        let row = object(&json!({"interaction_id": id, "marks": marks}))?;
        upsert(c, "delivery_checklists", &["interaction_id"], row)?;
    }
    for (request_id, approved) in after.request_plans.iter().filter(|(id, a)| before.request_plans.get(*id) != Some(*a)) {
        let row = object(&json!({"request_id": request_id, "task_id": approved.task_id, "presentation_id": approved.presentation_id}))?;
        upsert(c, "request_plans", &["request_id"], row)?;
    }
    for (id, review) in after.plan_reviews.iter().filter(|(id, r)| before.plan_reviews.get(*id) != Some(*r)) {
        upsert(c, "plan_reviews", &["interaction_id"], object(&json!({"interaction_id": id, "review": review}))?)?;
    }
    for (task, records) in before.lifecycle_journal.grown(&after.lifecycle_journal) {
        let known = before.lifecycle_journal.records(task);
        let lifecycle = if known == 0 { None } else { Some(before.lifecycle_of(task)?) };
        save_new_records(journal, task, lifecycle.as_deref(), records.since(known))?;
    }
    for (i, prior) in interactions.iter().filter(|(_, prior)| prior.is_some()) {
        upsert(c, "interactions", &["id"], interaction_row(i)?)?;
        save_interaction_relations(c, i, *prior)?;
    }
    for (agent_id, text) in after.last_words.iter().filter(|(id, text)| before.last_words.get(*id) != Some(*text)) {
        upsert(c, "node_last_words", &["agent_id"], object(&json!({"agent_id": agent_id, "text": text}))?)?;
    }
    if before.accounts != after.accounts || before.account_reserve != after.account_reserve {
        exec(c, "INSERT INTO accounts(singleton,accounts,reserve) VALUES(1,?1,?2) ON CONFLICT(singleton) DO UPDATE SET accounts=excluded.accounts,reserve=excluded.reserve",
            params![serde_json::to_string(&after.accounts)?, after.account_reserve.parts()])?;
    }
    if before.account_policy != after.account_policy || before.account_moves != after.account_moves {
        exec(c, "INSERT INTO account_distribution(singleton,policy,move_when_exhausted) VALUES(1,?1,?2) ON CONFLICT(singleton) DO UPDATE SET policy=excluded.policy,move_when_exhausted=excluded.move_when_exhausted",
            params![policy_name(after.account_policy), after.account_moves])?;
    }
    if before.merge_method != after.merge_method {
        exec(c, "INSERT INTO merge_method(singleton,method) VALUES(1,?1) ON CONFLICT(singleton) DO UPDATE SET method=excluded.method",
            params![after.merge_method.id()])?;
    }
    if before.branch_update != after.branch_update {
        exec(c, "INSERT INTO branch_update(singleton,method) VALUES(1,?1) ON CONFLICT(singleton) DO UPDATE SET method=excluded.method",
            params![after.branch_update.id()])?;
    }
    if before.account_usage != after.account_usage {
        let key = |usage: &alondra_core::usage::AccountUsage| (usage.provider.as_str(), usage.account.as_str().to_owned());
        for gone in before.account_usage.iter().filter(|old| !after.account_usage.iter().any(|new| key(new) == key(old))) {
            exec(c, "DELETE FROM account_usage WHERE provider=?1 AND account=?2", params![gone.provider.as_str(), gone.account.as_str()])?;
        }
        for usage in after.account_usage.iter().filter(|new| !before.account_usage.contains(new)) {
            exec(c, "INSERT INTO account_usage(provider,account,usage) VALUES(?1,?2,?3) ON CONFLICT(provider,account) DO UPDATE SET usage=excluded.usage",
                params![usage.provider.as_str(), usage.account.as_str(), serde_json::to_string(usage)?])?;
        }
    }
    super::provider_accounts::save(c, before, after)?;
    save_schedule_changes(c, &before.schedule, &after.schedule)?;
    Ok(())
}
/// The task's new records, as the events the store keeps: only the store writes the journal, and
/// it replays the task and checks each event before keeping it. What a user tells an SDK
/// execution, and each decision Claude waits on, leaves with its dispatch or response prepared in
/// the same transaction, for its owner to claim.
pub(super) fn save_journal<'a>(
    journal: &mut alondra_store::LifecycleTransaction<'_>,
    task: &TaskId,
    records: impl IntoIterator<Item = &'a LifecycleRecord>,
    known: usize,
) -> Result<()> {
    save_writes(journal, task, journal_writes(task, records, known)?)
}

/// `save_journal` for the records a task's journal adds after the lifecycle `known` its earlier
/// records lead to: only the new records are replayed.
pub(super) fn save_new_records<'a>(
    journal: &mut alondra_store::LifecycleTransaction<'_>,
    task: &TaskId,
    known: Option<&alondra_domain::TaskLifecycle>,
    records: impl IntoIterator<Item = &'a LifecycleRecord>,
) -> Result<()> {
    save_writes(journal, task, journal_writes_after(task, known, records)?)
}

fn save_writes(journal: &mut alondra_store::LifecycleTransaction<'_>, task: &TaskId, writes: Vec<JournalWrite>) -> Result<()> {
    for write in writes {
        match write.event {
            None => journal.create_without_text(task, write.input.context("a new journal names its initial input")?)?,
            Some(event) => {
                let told = match (&write.input, write.text) {
                    (Some(input), Some(text)) => {
                        journal.put_input(task, alondra_store::InputPayload::new(input.clone(), text)?)?;
                        Some(input.clone())
                    }
                    (Some(input), None) => {
                        journal.put_input_without_text(task, input)?;
                        None
                    }
                    (None, _) => None,
                };
                let decided = match &event {
                    alondra_domain::Event::Decide { card, decision } => {
                        let aggregate = journal.load(task)?;
                        let callback = aggregate.card_callback(card).context("a decided card decides a callback")?;
                        Some((alondra_domain::decision_epoch(aggregate.generation(), callback).context("a decided card decides a request")?, decision.clone()))
                    }
                    alondra_domain::Event::Runtime { stamp, fact: alondra_domain::RuntimeFact::AnsweredByPolicy { callback, decision }, .. } => {
                        Some((alondra_domain::decision_epoch(stamp.generation(), callback).context("a policy decides a request")?, decision.clone()))
                    }
                    _ => None,
                };
                journal.apply(task, write.revision, &format!("lifecycle-{}", write.revision + 1), event.clone())?;
                if let Some((epoch, decision)) = decided {
                    journal.stage_response(task, epoch, &decision)?;
                }
                match (event, told) {
                    (alondra_domain::Event::RequestControl { control, kind }, _) => journal.stage_control(task, &control, &kind)?,
                    (alondra_domain::Event::Compact { input }, _) => journal.stage_compaction(task, &input)?,
                    (_, Some(input))
                        if journal.load(task)?.execution().is_some_and(|stamp| stamp.host().as_str() != alondra_domain::CLAUDE_BG_HOST) =>
                    {
                        journal.stage_input(task, &input)?
                    }
                    _ => {}
                }
            }
        }
    }
    Ok(())
}
/// The domain from its tables and its journal. Every task's lifecycle fields are checked against
/// its journal before anything else reads them: a column that disagrees fails the load, and
/// nothing repairs it.
pub(super) fn load(c: &Connection) -> Result<DomainState> {
    let mut state = load_rows(c)?;
    let scope = alondra_store::AuthorityScope::new(state.data_epoch.clone())?;
    let journal = alondra_store::read_journal(c, &scope, |lifecycle, event, texts| anyhow::Ok(record_for(lifecycle, event, texts)?))?;
    let mut journals = Vec::with_capacity(journal.len());
    for (task, stored) in journal {
        ensure!(state.tasks.contains_key(&task), "the lifecycle journal of task {task} has no task");
        let mut records = Vec::with_capacity(stored.entries.len() + 1);
        records.push(LifecycleRecord::Planned { input: stored.initial.as_str().into() });
        records.extend(stored.entries);
        journals.push((task, records, stored.lifecycle));
    }
    let lifecycles = state.load_journal(journals)?;
    state.derive_loaded_states();
    state.validate()?;
    state.validate_loaded(&lifecycles)?;
    Ok(state)
}
/// The domain's rows as its tables keep them, without the journal: what a migration reads to
/// write the journal of tasks that have none.
pub(super) fn load_rows(c: &Connection) -> Result<DomainState> {
    let (epoch, revision, generation): (String, i64, i64) = c.query_row(
        "SELECT data_epoch,revision,orchestrator_generation FROM domain_meta WHERE singleton=1",
        [],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
    )?;
    let mut state = DomainState::new(epoch);
    state.revision = revision.try_into().context("negative revision")?;
    state.orchestrator_generation = generation.try_into().context("negative generation")?;
    state.orchestrator_session = c.query_row("SELECT session_id FROM orchestrator_session WHERE singleton=1", [], |r| r.get::<_, String>(0))
        .optional()?.map(AgentId::from);
    for row in rows(c, "requests", &["closed", "withdrawn", "auto_merge"], &[])? {
        let x: Request = decode(row)?;
        state.requests.insert(x.id.clone(), x);
    }
    let mut maintenance = c.prepare("SELECT request_id FROM maintenance_requests")?;
    let mut stored = maintenance.query([])?;
    while let Some(row) = stored.next()? {
        let id: String = row.get(0)?;
        state.requests.get_mut(&RequestId::from(id)).context("maintenance request missing")?.maintenance = true;
    }
    drop(stored);
    drop(maintenance);
    let mut messages = c.prepare("SELECT request_id FROM message_requests")?;
    let mut stored = messages.query([])?;
    while let Some(row) = stored.next()? {
        let id: String = row.get(0)?;
        state.requests.get_mut(&RequestId::from(id)).context("message request missing")?.message = true;
    }
    drop(stored);
    drop(messages);
    let mut cleared = c.prepare("SELECT request_id FROM cleared_requests")?;
    let mut stored = cleared.query([])?;
    while let Some(row) = stored.next()? {
        let id: String = row.get(0)?;
        state.requests.get_mut(&RequestId::from(id)).context("cleared request missing")?.cleared = true;
    }
    drop(stored);
    drop(cleared);
    let mut origins = c.prepare("SELECT request_id,origin FROM request_origins")?;
    let mut stored = origins.query([])?;
    while let Some(row) = stored.next()? {
        let (id, origin): (String, String) = (row.get(0)?, row.get(1)?);
        state.requests.get_mut(&RequestId::from(id)).context("request origin missing")?.origin = serde_json::from_str(&origin)?;
    }
    drop(stored);
    drop(origins);
    for row in rows(c, "agents", &[], &["phase"])? {
        let x: Agent = decode(row)?;
        state.agents.insert(x.id.clone(), x);
    }
    for mut row in rows(c, "tasks", &["started", "review_prs", "archived"], &[])? {
        row["dependencies"] = json!([]);
        row["stack"] = Value::Null;
        row["documents"] = json!({});
        let x: Task = decode(row)?;
        state.tasks.insert(x.id.clone(), x);
    }
    for row in rows(c, "documents", &[], &[])? {
        let x: Document = decode(row)?;
        state.documents.insert(x.id.clone(), x);
    }
    for row in rows(c, "document_revisions", &[], &[])? {
        let x: DocumentRevision = decode(row)?;
        state.document_revisions.insert(x.id.clone(), x);
    }
    for row in rows(c, "task_dependencies", &[], &[])? {
        state
            .tasks
            .get_mut(&TaskId::from(s(&row["task_id"])?))
            .context("dependency task missing")?
            .dependencies
            .insert(decode(row["depends_on_id"].clone())?);
    }
    for row in rows(c, "task_documents", &[], &[])? {
        state
            .tasks
            .get_mut(&TaskId::from(s(&row["task_id"])?))
            .context("document task missing")?
            .documents
            .insert(decode(row["document_id"].clone())?, s(&row["role"])?.into());
    }
    for mut row in rows(c, "pull_requests", &[], &["allowed_methods"])? {
        row["review"] = Value::Null;
        let x: PullRequest = decode(row)?;
        state.pull_requests.insert(x.id.clone(), x);
    }
    for row in rows(c, "pull_request_reviews", &[], &[])? {
        state
            .pull_requests
            .get_mut(&PrId::from(s(&row["pr_id"])?))
            .context("review PR missing")?
            .review = Some(PullRequestReview { decisions: vec![] });
    }
    for mut row in rows(
        c,
        "interactions",
        &["blocking", "is_final", "acceptance_valid", "withdrawn", "shortened"],
        &["options", "answer", "card", "clarification", "system_card"],
    )? {
        let kind = take(&mut row, "scope_kind");
        let task = take(&mut row, "task_id");
        let request = take(&mut row, "request_id");
        row["scope"] = if kind == "task" {
            json!({"kind":"task","task_id":task})
        } else {
            json!({"kind":"orchestrator","request_id":request})
        };
        let kind = take(&mut row, "author_kind");
        let agent = take(&mut row, "author_agent_id");
        let generation = take(&mut row, "author_generation");
        row["author"] = match kind.as_str() {
            Some("agent") => json!({"kind":"agent","agent_id":agent}),
            Some("orchestrator") => json!({"kind":"orchestrator","generation":generation}),
            _ => json!({"kind":"system"}),
        };
        let kind = take(&mut row, "recipient_kind");
        let agent = take(&mut row, "recipient_agent_id");
        row["recipient"] = if kind == "agent" {
            json!({"kind":"agent","agent_id":agent})
        } else {
            json!({"kind":kind})
        };
        let kind = take(&mut row, "kind");
        let options = take(&mut row, "options");
        let answer = take(&mut row, "answer");
        let is_final = take(&mut row, "is_final");
        let user = take(&mut row, "acceptance_user");
        let at = take(&mut row, "acceptance_revision");
        let valid = take(&mut row, "acceptance_valid");
        row["payload"] = match kind.as_str() {
            Some("information") => json!({"kind":"information","read_by":[]}),
            Some("question") | Some("action_request") => {
                json!({"kind":kind,"options":options,"answer":answer})
            }
            Some("delivery") => {
                json!({"kind":"delivery","targets":[],"is_final":is_final,"acceptance":if user.is_null(){Value::Null}else{json!({"user_id":user,"at_revision":at,"valid":valid})},"feedback":[]})
            }
            _ => anyhow::bail!("unknown interaction kind"),
        };
        let ordering = take(&mut row, "ordering");
        row["order"] = ordering;
        row["subject"] = Value::Null;
        let x: Interaction = decode(row)?;
        state.interactions.insert(x.id.clone(), x);
    }
    for row in rows(c, "interaction_reads", &[], &[])? {
        let i = state
            .interactions
            .get_mut(&InteractionId::from(s(&row["interaction_id"])?))
            .context("read interaction missing")?;
        if let InteractionPayload::Information { read_by } = &mut i.payload {
            read_by.insert(s(&row["user_id"])?.into());
        } else {
            anyhow::bail!("read on non-information");
        }
    }
    for row in rows(c, "interaction_subjects", &[], &[])? {
        let subject = if !row["pr_id"].is_null() {
            json!({"kind":"pull_request","pr_id":row["pr_id"]})
        } else {
            json!({"kind":"document_review","document_id":row["document_id"],"revision_id":row["revision_id"]})
        };
        state
            .interactions
            .get_mut(&InteractionId::from(s(&row["interaction_id"])?))
            .context("subject interaction missing")?
            .subject = Some(decode(subject)?);
    }
    for mut row in rows(c, "delivery_targets", &[], &[])? {
        let id = take(&mut row, "interaction_id");
        let target: DeliveryTarget = decode(row)?;
        let i = state
            .interactions
            .get_mut(&InteractionId::from(s(&id)?))
            .context("target interaction missing")?;
        if let InteractionPayload::Delivery { targets, .. } = &mut i.payload {
            targets.push(target);
        } else {
            anyhow::bail!("target on non-delivery");
        }
    }
    for mut row in rows(c, "delivery_feedback", &[], &["merge_failure"])? {
        let id = take(&mut row, "interaction_id");
        let f: Feedback = decode(row)?;
        let i = state
            .interactions
            .get_mut(&InteractionId::from(s(&id)?))
            .context("feedback interaction missing")?;
        if let InteractionPayload::Delivery { feedback, .. } = &mut i.payload {
            feedback.push(f);
        } else {
            anyhow::bail!("feedback on non-delivery");
        }
    }
    for row in rows(c, "interaction_attachments", &[], &[])? {
        let id = InteractionId::from(s(&row["interaction_id"])?);
        let attachment: AttachmentRef = decode(serde_json::json!({
            "id": row["id"], "name": row["name"], "content_digest": row["content_digest"],
            "content_reference": row["content_reference"], "bytes": row["bytes"]
        }))?;
        let i = state
            .interactions
            .get_mut(&id)
            .context("attachment interaction missing")?;
        match s(&row["kind"])? {
            "interaction" => i.attachments.push(attachment),
            "answer" => match &mut i.payload {
                InteractionPayload::Question {
                    answer: Some(a), ..
                }
                | InteractionPayload::ActionRequest {
                    answer: Some(a), ..
                } => a.attachments.push(attachment),
                _ => anyhow::bail!("answer attachment lacks answer"),
            },
            "feedback" => match &mut i.payload {
                InteractionPayload::Delivery { feedback, .. } => {
                    let n: usize = row["feedback_ordinal"]
                        .as_u64()
                        .context("feedback ordinal missing")?
                        .try_into()?;
                    feedback
                        .get_mut(n)
                        .context("feedback attachment lacks feedback")?
                        .attachments
                        .push(attachment);
                }
                _ => anyhow::bail!("feedback attachment lacks delivery"),
            },
            _ => anyhow::bail!("unknown attachment kind"),
        }
    }
    for row in rows(c, "document_reviews", &[], &[])? {
        state.document_reviews.push(DocumentReview {
            task_id: decode(row["task_id"].clone())?,
            document_id: decode(row["document_id"].clone())?,
            decisions: vec![],
        });
    }
    for row in rows(c, "document_decisions", &["accepted"], &[])? {
        let task: TaskId = decode(row["task_id"].clone())?;
        let doc: DocumentId = decode(row["document_id"].clone())?;
        state
            .document_reviews
            .iter_mut()
            .find(|r| r.task_id == task && r.document_id == doc)
            .context("document review missing")?
            .decisions
            .push(decode(row)?);
    }
    for row in rows(c, "pr_decisions", &["accepted"], &[])? {
        state
            .pull_requests
            .get_mut(&PrId::from(s(&row["pr_id"])?))
            .context("decision PR missing")?
            .review
            .as_mut()
            .context("PR review missing")?
            .decisions
            .push(decode(row)?);
    }
    for row in rows(c, "merge_authorizations", &["valid"], &[])? {
        let x: MergeAuthorization = decode(row)?;
        state.merge_authorizations.insert(x.id.clone(), x);
    }
    let mut models = c.prepare("SELECT task_id, model, chosen_by FROM task_models")?;
    let mut stored = models.query([])?;
    while let Some(row) = stored.next()? {
        let id: String = row.get(0)?;
        let model = json!({"model": row.get::<_, String>(1)?, "chosen_by": row.get::<_, String>(2)?});
        state.tasks.get_mut(&TaskId::from(id)).context("model task missing")?.model = Some(decode(model)?);
    }
    drop(stored);
    let mut projects = c.prepare("SELECT task_id, project FROM task_projects")?;
    let mut stored = projects.query([])?;
    while let Some(row) = stored.next()? {
        let id: String = row.get(0)?;
        state.tasks.get_mut(&TaskId::from(id)).context("project task missing")?.project = Some(row.get(1)?);
    }
    drop(stored);
    let mut continuations = c.prepare("SELECT task_id, continues FROM task_continuations")?;
    let mut stored = continuations.query([])?;
    while let Some(row) = stored.next()? {
        let id: String = row.get(0)?;
        state.tasks.get_mut(&TaskId::from(id)).context("continuing task missing")?.continues = Some(TaskId::from(row.get::<_, String>(1)?));
    }
    drop(stored);
    let mut accounts = c.prepare("SELECT task_id, account FROM task_accounts")?;
    let mut stored = accounts.query([])?;
    while let Some(row) = stored.next()? {
        let id: String = row.get(0)?;
        let account = alondra_core::accounts::AccountId::parse(&row.get::<_, String>(1)?).map_err(anyhow::Error::msg)?;
        state.tasks.get_mut(&TaskId::from(id)).context("account task missing")?.account = Some(account);
    }
    drop(stored);
    let mut wrap_ups = c.prepare("SELECT task_id, asked_at, cleaned_at, close_cancelled, node_end FROM task_wrap_ups")?;
    let mut stored = wrap_ups.query([])?;
    while let Some(row) = stored.next()? {
        let id: String = row.get(0)?;
        let node: Option<String> = row.get(4)?;
        let wrap_up = WrapUp {
            asked_at: row.get::<_, Option<i64>>(1)?.map(|r| r as u64),
            cleaned_at: row.get::<_, Option<i64>>(2)?.map(|r| r as u64),
            close_cancelled: row.get::<_, i64>(3)? != 0,
            node: node.map(|n| decode(Value::String(n))).transpose()?,
            post_merge_work: None,
        };
        state.tasks.get_mut(&TaskId::from(id)).context("wrap-up task missing")?.wrap_up = wrap_up;
    }
    drop(stored);
    let mut worktrees = c.prepare("SELECT task_id, path, origin, branch, discard, end FROM task_worktree_records")?;
    let mut stored = worktrees.query([])?;
    while let Some(row) = stored.next()? {
        let id: String = row.get(0)?;
        let end: Option<String> = row.get(5)?;
        let worktree = TaskWorktree {
            origin: decode(Value::String(row.get(2)?))?,
            branch: row.get(3)?,
            discard: row.get::<_, i64>(4)? != 0,
            end: end.map(|end| decode(Value::String(end))).transpose()?,
        };
        state.tasks.get_mut(&TaskId::from(id)).context("worktree task missing")?.worktrees.insert(row.get(1)?, worktree);
    }
    drop(stored);
    let mut grants = c.prepare("SELECT task_id, card, allows, uses FROM task_permission_grants")?;
    let mut stored = grants.query([])?;
    while let Some(row) = stored.next()? {
        let id: String = row.get(0)?;
        let allows: String = row.get(2)?;
        let grant = TaskGrant { allows: serde_json::from_str(&allows)?, uses: u32::try_from(row.get::<_, i64>(3)?)? };
        state.tasks.get_mut(&TaskId::from(id)).context("grant task missing")?.allowed.insert(InteractionId(row.get(1)?), grant);
    }
    drop(stored);
    let mut post_merge = c.prepare(
        "SELECT w.task_id, w.reason, w.declared_at, t.task_id IS NOT NULL FROM task_post_merge_work w \
         LEFT JOIN task_post_merge_turns t ON t.task_id = w.task_id",
    )?;
    let mut stored = post_merge.query([])?;
    while let Some(row) = stored.next()? {
        let id: String = row.get(0)?;
        let work = PostMergeWork { reason: row.get(1)?, declared_at: row.get::<_, i64>(2)? as u64, turn_since: row.get(3)? };
        state.tasks.get_mut(&TaskId::from(id)).context("post-merge work task missing")?.wrap_up.post_merge_work = Some(work);
    }
    drop(stored);
    let mut priorities = c.prepare("SELECT task_id, priority FROM task_priorities")?;
    let mut stored = priorities.query([])?;
    while let Some(row) = stored.next()? {
        let id: String = row.get(0)?;
        state.tasks.get_mut(&TaskId::from(id)).context("priority task missing")?.priority = decode(Value::String(row.get(1)?))?;
    }
    drop(stored);
    let mut stalls = c.prepare("SELECT task_id, stalls, closing FROM task_stalls")?;
    let mut stored = stalls.query([])?;
    while let Some(row) = stored.next()? {
        let id: String = row.get(0)?;
        let task = state.tasks.get_mut(&TaskId::from(id)).context("stalled task missing")?;
        task.stalls = row.get::<_, i64>(1)? as u32;
        task.stall_closing = row.get::<_, Option<String>>(2)?.as_deref().map(serde_json::from_str).transpose()?;
    }
    drop(stored);
    let mut open_prs = c.prepare("SELECT task_id, pending FROM task_open_prs")?;
    let mut stored = open_prs.query([])?;
    while let Some(row) = stored.next()? {
        let id: String = row.get(0)?;
        let task = state.tasks.get_mut(&TaskId::from(id)).context("task of an open PR missing")?;
        task.open_pr = Some(serde_json::from_str(&row.get::<_, String>(1)?)?);
    }
    drop(stored);
    let mut relations = c.prepare("SELECT task_id, delivered_dependencies, blocks, verdict, review_rounds FROM task_relations")?;
    let mut stored = relations.query([])?;
    while let Some(row) = stored.next()? {
        let id: String = row.get(0)?;
        let task = state.tasks.get_mut(&TaskId::from(id)).context("relations task missing")?;
        task.delivered_dependencies = serde_json::from_str(&row.get::<_, String>(1)?)?;
        task.blocks = serde_json::from_str(&row.get::<_, String>(2)?)?;
        task.verdict = row.get::<_, Option<String>>(3)?.map(|v| serde_json::from_str(&v)).transpose()?;
        task.review_rounds = row.get::<_, i64>(4)? as u8;
    }
    drop(stored);
    for mut row in rows(c, "task_stacks", &[], &[])? {
        let id = take(&mut row, "task_id");
        state
            .tasks
            .get_mut(&TaskId::from(s(&id)?))
            .context("stack task missing")?
            .stack = Some(decode(row)?);
    }
    for row in rows(c, "stack_readiness", &["ready"], &[])? {
        let x: StackReadiness = decode(row)?;
        state.stack_readiness.insert(x.task_id.clone(), x);
    }
    for row in rows(c, "interaction_holds", &[], &[])? {
        state.holds.insert(InteractionId::from(s(&row["interaction_id"])?), decode(row["hold"].clone())?);
    }
    for row in rows(c, "task_checklists", &[], &["items"])? {
        let checklist = Checklist { plan: InteractionId::from(s(&row["plan_interaction_id"])?), items: decode(row["items"].clone())? };
        state.checklists.insert(TaskId::from(s(&row["task_id"])?), checklist);
    }
    for row in rows(c, "delivery_checklists", &[], &["marks"])? {
        state.checklist_marks.insert(InteractionId::from(s(&row["interaction_id"])?), decode(row["marks"].clone())?);
    }
    for row in rows(c, "request_plans", &[], &[])? {
        let approved = ApprovedPlan { task_id: TaskId::from(s(&row["task_id"])?), presentation_id: InteractionId::from(s(&row["presentation_id"])?) };
        state.request_plans.insert(RequestId::from(s(&row["request_id"])?), approved);
    }
    for row in rows(c, "plan_reviews", &[], &["review"])? {
        state.plan_reviews.insert(InteractionId::from(s(&row["interaction_id"])?), decode(row["review"].clone())?);
    }
    for row in rows(c, "node_last_words", &[], &[])? {
        state.last_words.insert(AgentId::from(s(&row["agent_id"])?), s(&row["text"])?.to_owned());
    }
    let accounts = c.query_row("SELECT accounts,reserve FROM accounts WHERE singleton=1", [], |r| Ok((r.get::<_, String>(0)?, r.get::<_, u16>(1)?))).optional()?;
    if let Some((accounts, reserve)) = accounts {
        state.accounts = serde_json::from_str(&accounts)?;
        state.account_reserve = alondra_core::usage::Share::from_parts(reserve).context("an account reserve above 10000")?;
    }
    if let Some((policy, moves)) = c.query_row("SELECT policy,move_when_exhausted FROM account_distribution WHERE singleton=1", [], |r| Ok((r.get::<_, String>(0)?, r.get::<_, bool>(1)?))).optional()? {
        state.account_policy = serde_json::from_value(serde_json::Value::String(policy))?;
        state.account_moves = moves;
    }
    let stored: bool = c.query_row("SELECT count(*) FROM sqlite_schema WHERE type='table' AND name='merge_method'", [], |r| r.get::<_, i64>(0))? == 1;
    if stored && let Some(method) = c.query_row("SELECT method FROM merge_method WHERE singleton=1", [], |r| r.get::<_, String>(0)).optional()? {
        state.merge_method = MergeMethod::parse(&method).with_context(|| format!("an unknown merge method {method:?}"))?;
    }
    let stored: bool = c.query_row("SELECT count(*) FROM sqlite_schema WHERE type='table' AND name='branch_update'", [], |r| r.get::<_, i64>(0))? == 1;
    if stored && let Some(method) = c.query_row("SELECT method FROM branch_update WHERE singleton=1", [], |r| r.get::<_, String>(0)).optional()? {
        state.branch_update = alondra_core::domain::BranchUpdate::parse(&method).with_context(|| format!("an unknown branch update {method:?}"))?;
    }
    let mut usage = c.prepare("SELECT usage FROM account_usage ORDER BY rowid")?;
    for row in usage.query_map([], |r| r.get::<_, String>(0))? {
        state.account_usage.push(serde_json::from_str(&row?)?);
    }
    drop(usage);
    super::provider_accounts::load(c, &mut state)?;
    state.schedule = load_schedule_state(c)?;
    Ok(state)
}

fn policy_name(policy: alondra_core::accounts::Policy) -> &'static str {
    match policy {
        alondra_core::accounts::Policy::Margin => "margin",
        alondra_core::accounts::Policy::Chain => "chain",
    }
}

/// Adds `interactions.shortened` and, that once, marks the final results an earlier compaction
/// cut: their body is the excerpt it wrote, `RESULT_LIMIT` characters and its closing marker.
/// Worktree records made before a worktree could end handed over to another task take that end.
pub(super) fn allow_handed_over_worktrees(tx: &rusqlite::Transaction<'_>, schema: &str) -> rusqlite::Result<()> {
    let sql: String = tx.query_row("SELECT sql FROM sqlite_master WHERE type='table' AND name='task_worktree_records'", [], |row| row.get(0))?;
    if sql.contains("'handed_over'") {
        return Ok(());
    }
    tx.execute_batch("ALTER TABLE task_worktree_records RENAME TO task_worktree_records_old;")?;
    tx.execute_batch(schema)?;
    tx.execute_batch("INSERT INTO task_worktree_records(task_id,path,origin,branch,discard,end) \
        SELECT task_id,path,origin,branch,discard,end FROM task_worktree_records_old; DROP TABLE task_worktree_records_old;")
}
pub(super) fn add_shortened_column(tx: &rusqlite::Transaction<'_>) -> rusqlite::Result<()> {
    let present = tx.query_row("SELECT count(*) FROM pragma_table_info('interactions') WHERE name='shortened'", [], |row| row.get::<_, i64>(0))? == 1;
    if present {
        return Ok(());
    }
    tx.execute_batch("ALTER TABLE interactions ADD COLUMN shortened INTEGER NOT NULL DEFAULT 0 CHECK(shortened IN(0,1))")?;
    const MARKER: &str = "\n[shortened]";
    tx.execute(
        "UPDATE interactions SET shortened=1 WHERE id IN(SELECT final_presentation FROM tasks WHERE final_presentation IS NOT NULL)
         AND length(body)=?1 AND substr(body,-?2)=?3",
        rusqlite::params![(alondra_core::domain::RESULT_LIMIT + MARKER.chars().count()) as i64, MARKER.chars().count() as i64, MARKER],
    )?;
    Ok(())
}
pub(super) fn add_snooze_columns(tx: &rusqlite::Transaction<'_>) -> rusqlite::Result<()> {
    for (column, definition) in [
        ("snoozed_until", "INTEGER"),
        ("resurfaced_at_revision", "INTEGER CHECK(resurfaced_at_revision IS NULL OR resurfaced_at_revision BETWEEN 0 AND 9007199254740991)"),
    ] {
        let present = tx.query_row("SELECT count(*) FROM pragma_table_info('interactions') WHERE name=?1", [column], |row| row.get::<_, i64>(0))? == 1;
        if !present {
            tx.execute_batch(&format!("ALTER TABLE interactions ADD COLUMN {column} {definition}"))?;
        }
    }
    Ok(())
}
