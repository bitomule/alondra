use alondra_core::domain_v2::*;
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
#[cfg(test)]
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

/// Save schedule definitions before their runs so the run FK and claim-revision
/// guard see the current definition. Callers own the transaction containing any
/// Request/Task rows linked by a newly sent run.
pub(super) fn save_schedule_state(c: &Connection, schedule: &ScheduleState) -> Result<()> {
    schedule.validate()?;
    for definition in schedule.schedules.values() {
        upsert(c, "schedules", &["id"], object(definition)?)?;
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

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct LegacyScheduleReceipt {
    pub sender: String,
    pub request_id: String,
    pub digest: Vec<u8>,
    /// Original JSON text is retained byte for byte for legacy idempotency replies.
    pub result: String,
}

pub(super) fn save_legacy_schedule_receipt(
    c: &Connection,
    receipt: &LegacyScheduleReceipt,
) -> Result<()> {
    serde_json::from_str::<Value>(&receipt.result).context("invalid legacy receipt JSON")?;
    let stored = load_legacy_schedule_receipt(c, &receipt.sender, &receipt.request_id)?;
    if let Some(stored) = stored {
        ensure!(stored == *receipt, "legacy schedule receipt is immutable");
    } else {
        c.execute(
            "INSERT INTO legacy_schedule_receipts(sender,request_id,digest,result) VALUES(?1,?2,?3,?4)",
            params![receipt.sender, receipt.request_id, receipt.digest, receipt.result],
        )?;
    }
    Ok(())
}

pub(super) fn load_legacy_schedule_receipt(
    c: &Connection,
    sender: &str,
    request_id: &str,
) -> Result<Option<LegacyScheduleReceipt>> {
    let stored: Option<(Vec<u8>, String)> = c
        .query_row(
            "SELECT digest,result FROM legacy_schedule_receipts WHERE sender=?1 AND request_id=?2",
            params![sender, request_id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    stored
        .map(|(digest, result)| {
            Ok(LegacyScheduleReceipt {
                sender: sender.into(),
                request_id: request_id.into(),
                digest,
                result,
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
    strip(&mut row, &["dependencies", "stack", "documents"]);
    Ok(row)
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
        upsert(c, "schedules", &["id"], object(definition)?)?;
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
    kept("request", &before.requests, &after.requests)?;
    kept("agent", &before.agents, &after.agents)?;
    kept("task", &before.tasks, &after.tasks)?;
    kept("document", &before.documents, &after.documents)?;
    kept("document revision", &before.document_revisions, &after.document_revisions)?;
    kept("pull request", &before.pull_requests, &after.pull_requests)?;
    kept("interaction", &before.interactions, &after.interactions)?;
    kept("merge authorization", &before.merge_authorizations, &after.merge_authorizations)?;
    kept("stack readiness", &before.stack_readiness, &after.stack_readiness)?;
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
pub(super) fn save_changes(c: &Connection, before: &DomainState, after: &DomainState) -> Result<()> {
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
    for (x, _) in changed(&before.requests, &after.requests) {
        upsert(c, "requests", &["id"], object(x)?)?;
    }
    for (x, _) in changed(&before.agents, &after.agents) {
        upsert(c, "agents", &["id"], object(x)?)?;
    }
    let tasks: Vec<_> = changed(&before.tasks, &after.tasks).collect();
    for (x, _) in &tasks {
        upsert(c, "tasks", &["id"], task_row(x)?)?;
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
    for (i, prior) in changed(&before.interactions, &after.interactions) {
        upsert(c, "interactions", &["id"], interaction_row(i)?)?;
        save_interaction_relations(c, i, prior)?;
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
    save_schedule_changes(c, &before.schedule, &after.schedule)?;
    Ok(())
}
/// Write a whole validated state into a store whose rows may be stale or absent.
pub(super) fn save(c: &Connection, state: &DomainState) -> Result<()> {
    state.validate()?;
    exec(
        c,
        "UPDATE domain_meta SET revision=?1,orchestrator_generation=?2 WHERE singleton=1",
        params![state.revision as i64, state.orchestrator_generation as i64],
    )?;
    for x in state.requests.values() {
        upsert(c, "requests", &["id"], object(x)?)?;
    }
    for x in state.agents.values() {
        upsert(c, "agents", &["id"], object(x)?)?;
    }
    for x in state.tasks.values() {
        upsert(c, "tasks", &["id"], task_row(x)?)?;
    }
    for x in state.documents.values() {
        upsert(c, "documents", &["id"], object(x)?)?;
    }
    for x in state.document_revisions.values() {
        upsert(c, "document_revisions", &["id"], object(x)?)?;
    }
    for t in state.tasks.values() {
        save_task_links(c, t, None)?;
    }
    for p in state.pull_requests.values() {
        save_pull_request(c, p, None)?;
    }
    for i in state.interactions.values() {
        upsert(c, "interactions", &["id"], interaction_row(i)?)?;
    }
    exec(c, "DELETE FROM interaction_attachments", [])?;
    for i in state.interactions.values() {
        save_interaction_relations(c, i, None)?;
    }
    for r in &state.document_reviews {
        save_document_review(c, r, None)?;
    }
    for p in state.pull_requests.values() {
        save_pr_decisions(c, p, None)?;
    }
    for a in state.merge_authorizations.values() {
        upsert(c, "merge_authorizations", &["id"], object(a)?)?;
    }
    for t in state.tasks.values() {
        save_task_stack(c, t, None)?;
    }
    exec(c, "DELETE FROM stack_readiness", [])?;
    for r in state.stack_readiness.values() {
        upsert(c, "stack_readiness", &["task_id"], object(r)?)?;
    }
    save_schedule_state(c, &state.schedule)?;
    Ok(())
}

pub(super) fn load(c: &Connection) -> Result<DomainState> {
    let (epoch, revision, generation): (String, i64, i64) = c.query_row(
        "SELECT data_epoch,revision,orchestrator_generation FROM domain_meta WHERE singleton=1",
        [],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
    )?;
    let mut state = DomainState::new(epoch);
    state.revision = revision.try_into().context("negative revision")?;
    state.orchestrator_generation = generation.try_into().context("negative generation")?;
    for row in rows(c, "requests", &["closed", "withdrawn"], &[])? {
        let x: Request = decode(row)?;
        state.requests.insert(x.id.clone(), x);
    }
    for row in rows(c, "agents", &["connected"], &[])? {
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
        &["blocking", "is_final", "acceptance_valid", "withdrawn"],
        &["options", "answer"],
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
    for mut row in rows(c, "delivery_feedback", &[], &[])? {
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
    state.schedule = load_schedule_state(c)?;
    state.validate()?;
    Ok(state)
}
