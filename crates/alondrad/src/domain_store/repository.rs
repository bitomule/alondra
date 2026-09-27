use alondra_core::domain_v2::*;
use anyhow::{Context, Result};
use rusqlite::{Connection, params, types::Value as SqlValue};
use serde::{Serialize, de::DeserializeOwned};
use serde_json::{Map, Value, json};

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
    c.execute(&query, rusqlite::params_from_iter(values))?;
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

fn save_attachments(
    c: &Connection,
    interaction: &InteractionId,
    kind: &str,
    feedback_ordinal: Option<i64>,
    attachments: &[AttachmentRef],
) -> Result<()> {
    for (ordinal, a) in attachments.iter().enumerate() {
        c.execute("INSERT INTO interaction_attachments(interaction_id,kind,feedback_ordinal,ordinal,id,name,content_digest,content_reference,bytes) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9)",
            params![interaction.0, kind, feedback_ordinal, ordinal as i64, a.id, a.name, a.content_digest, a.content_reference, a.bytes as i64])?;
    }
    Ok(())
}
pub(super) fn save(c: &Connection, state: &DomainState) -> Result<()> {
    state.validate()?;
    c.execute(
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
        let mut row = object(x)?;
        strip(&mut row, &["dependencies", "stack", "documents"]);
        upsert(c, "tasks", &["id"], row)?;
    }
    for x in state.documents.values() {
        upsert(c, "documents", &["id"], object(x)?)?;
    }
    for x in state.document_revisions.values() {
        upsert(c, "document_revisions", &["id"], object(x)?)?;
    }
    for t in state.tasks.values() {
        c.execute("DELETE FROM task_dependencies WHERE task_id=?1", [&t.id.0])?;
        for dep in &t.dependencies {
            put(
                c,
                "task_dependencies",
                &["task_id", "depends_on_id"],
                json!({"task_id":t.id,"depends_on_id":dep}),
            )?;
        }
        for (doc, role) in &t.documents {
            put(
                c,
                "task_documents",
                &["task_id", "document_id"],
                json!({"task_id":t.id,"document_id":doc,"role":role}),
            )?;
        }
    }
    for p in state.pull_requests.values() {
        let mut row = object(p)?;
        row.remove("review");
        upsert(c, "pull_requests", &["id"], row)?;
        if p.review.is_some() {
            put(c, "pull_request_reviews", &["pr_id"], json!({"pr_id":p.id}))?;
        }
    }
    for i in state.interactions.values() {
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
        upsert(c, "interactions", &["id"], row)?;
    }
    c.execute("DELETE FROM interaction_attachments", [])?;
    for i in state.interactions.values() {
        save_attachments(c, &i.id, "interaction", None, &i.attachments)?;
        let task = match &i.scope {
            Scope::Task { task_id } => Some(task_id),
            _ => None,
        };
        if let Some(subject) = &i.subject {
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
                save_attachments(c, &i.id, "answer", None, &answer.attachments)?;
            }
            InteractionPayload::Information { read_by } => {
                for user in read_by {
                    put(
                        c,
                        "interaction_reads",
                        &["interaction_id", "user_id"],
                        json!({"interaction_id":i.id,"user_id":user}),
                    )?;
                }
            }
            InteractionPayload::Delivery {
                targets, feedback, ..
            } => {
                for (index, target) in targets.iter().enumerate() {
                    let mut row = object(
                        &json!({"interaction_id":i.id,"ordinal":index,"task_id":task,"text":null,"document_id":null,"revision_id":null,"pr_id":null,"head":null}),
                    )?;
                    row.extend(object(target)?);
                    upsert(c, "delivery_targets", &["interaction_id", "ordinal"], row)?;
                }
                for (index, f) in feedback.iter().enumerate() {
                    let mut row = object(f)?;
                    row.remove("attachments");
                    row.extend(object(&json!({"interaction_id":i.id,"ordinal":index}))?);
                    upsert(c, "delivery_feedback", &["interaction_id", "ordinal"], row)?;
                    save_attachments(c, &i.id, "feedback", Some(index as i64), &f.attachments)?;
                }
            }
            _ => (),
        }
    }
    for r in &state.document_reviews {
        put(
            c,
            "document_reviews",
            &["task_id", "document_id"],
            json!({"task_id":r.task_id,"document_id":r.document_id}),
        )?;
        for (index, d) in r.decisions.iter().enumerate() {
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
    }
    for p in state.pull_requests.values() {
        if let Some(r) = &p.review {
            for (index, d) in r.decisions.iter().enumerate() {
                let mut row = object(d)?;
                row.extend(object(
                    &json!({"pr_id":p.id,"task_id":p.task_id,"ordinal":index}),
                )?);
                upsert(c, "pr_decisions", &["pr_id", "ordinal"], row)?;
            }
        }
    }
    for a in state.merge_authorizations.values() {
        upsert(c, "merge_authorizations", &["id"], object(a)?)?;
    }
    for t in state.tasks.values() {
        c.execute("DELETE FROM task_stacks WHERE task_id=?1", [&t.id.0])?;
        if let Some(stack) = &t.stack {
            let mut row = object(stack)?;
            row.insert("task_id".into(), json!(t.id));
            upsert(c, "task_stacks", &["task_id"], row)?;
        }
    }
    c.execute("DELETE FROM stack_readiness", [])?;
    for r in state.stack_readiness.values() {
        upsert(c, "stack_readiness", &["task_id"], object(r)?)?;
    }
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
    for row in rows(c, "requests", &["closed"], &[])? {
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
        &["blocking", "is_final", "acceptance_valid"],
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
    state.validate()?;
    Ok(state)
}
