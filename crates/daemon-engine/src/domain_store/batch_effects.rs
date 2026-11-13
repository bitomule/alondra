//! A batch's confirmed domain mutations reference the semantic receipts written by that same
//! transaction. Admission of a tool or loss of its reply is never substituted for this proof.
use super::*;

pub(super) const SCHEMA: &str = "CREATE TABLE IF NOT EXISTS orchestrator_batch_runs(run TEXT PRIMARY KEY,session TEXT,data_epoch TEXT NOT NULL,selection TEXT CHECK(selection IS NULL OR json_valid(selection)),ended INTEGER NOT NULL DEFAULT 0 CHECK(ended IN(0,1)));\
CREATE TABLE IF NOT EXISTS orchestrator_batch_effects(run TEXT NOT NULL REFERENCES orchestrator_batch_runs(run),attempt INTEGER NOT NULL,call TEXT NOT NULL,actor TEXT NOT NULL,command_id TEXT NOT NULL,data_epoch TEXT NOT NULL,digest BLOB NOT NULL CHECK(length(digest)=32),kind TEXT NOT NULL,revision INTEGER NOT NULL,changed INTEGER NOT NULL CHECK(changed IN(0,1)),pinned INTEGER NOT NULL DEFAULT 1 CHECK(pinned IN(0,1)),PRIMARY KEY(run,attempt,call,command_id));\
CREATE INDEX IF NOT EXISTS orchestrator_effect_receipt ON orchestrator_batch_effects(actor,command_id,data_epoch,pinned);\
CREATE TABLE IF NOT EXISTS orchestrator_batch_reports(run TEXT NOT NULL REFERENCES orchestrator_batch_runs(run),cursor INTEGER NOT NULL CHECK(cursor>0),digest BLOB NOT NULL CHECK(length(digest)=32),report TEXT NOT NULL CHECK(json_valid(report)),PRIMARY KEY(run,cursor));\
CREATE TABLE IF NOT EXISTS orchestrator_tool_operations(run TEXT NOT NULL REFERENCES orchestrator_batch_runs(run),call TEXT NOT NULL,kind TEXT NOT NULL,digest BLOB NOT NULL CHECK(length(digest)=32),request TEXT NOT NULL CHECK(json_valid(request)),state TEXT NOT NULL CHECK(state IN('started','succeeded','uncertain')),result TEXT,error TEXT,PRIMARY KEY(run,call));\
CREATE TABLE IF NOT EXISTS orchestrator_batch_decisions(run TEXT PRIMARY KEY REFERENCES orchestrator_batch_runs(run),target TEXT NOT NULL CHECK(target IN('discarded','retryable')));";

/// What only the orchestrator's tools answer, before they change anything.
const TOOL_REFUSALS: &[&str] = &[
    "node_paused", "task_closed", "model_is_binding", "task_moving", "usage_exhausted", "disk_low",
    "account_unavailable", "model_unavailable", "unsupported_capability", "workspace_untrusted", "main_checkout",
    "session_unreachable", "native_session_required", "selection_transition_required",
];

/// The domain's rules refuse before they commit, so such a call is settled and nothing is left to
/// reconcile, except `delivery_uncertain`. Any code not known here leaves its effects unknown.
pub(crate) fn refused_before_any_effect(code: Option<&str>) -> bool {
    code.is_some_and(|code| code != "delivery_uncertain" && (alondra_core::domain::INTERNAL_CODES.contains(&code) || TOOL_REFUSALS.contains(&code)))
}

fn error_code(error: Option<&str>) -> Option<String> {
    error.and_then(|error| serde_json::from_str::<DomainError>(error).ok()).map(|error| error.code)
}

/// An operation that is neither done nor refused before it changed anything.
pub(crate) fn is_unconfirmed(operation: &(String, String, String, Option<String>, Option<String>)) -> bool {
    match operation.2.as_str() {
        "succeeded" => false,
        "uncertain" => !refused_before_any_effect(error_code(operation.4.as_deref()).as_deref()),
        _ => true,
    }
}

const OPERATION_BYTES: i64 = 32 * 1024 * 1024;
const OUTCOME_BYTES: usize = 512 * 1024;
const OUTCOME_RESERVE: i64 = 512;

fn operation_capacity(conn: &Connection, run: &str) -> rusqlite::Result<(i64, i64, i64)> {
    conn.query_row("SELECT count(*),coalesce(sum(length(CAST(request AS BLOB))+coalesce(length(CAST(result AS BLOB)),0)+coalesce(length(CAST(error AS BLOB)),0)),0),coalesce(sum(state='started'),0) FROM orchestrator_tool_operations WHERE run=?1", [run], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct BatchReceiptRef {
    pub(crate) actor: String,
    pub(crate) command_id: String,
    pub(crate) data_epoch: String,
    pub(crate) digest: Vec<u8>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct BatchEffectRef {
    pub(crate) run: String,
    pub(crate) attempt: u64,
    pub(crate) call: String,
    pub(crate) command_id: String,
    pub(crate) receipt_ref: BatchReceiptRef,
    pub(crate) kind: String,
    pub(crate) revision: u64,
    pub(crate) changed: bool,
}

pub(crate) struct BatchCommand<'a> {
    pub(crate) run: &'a str,
    pub(crate) attempt: u64,
    pub(crate) call: &'a str,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct BatchReport {
    pub(crate) kind: String,
    pub(crate) input_consumed: bool,
    pub(crate) turn_completed: bool,
    pub(crate) native_turn: Option<String>,
    pub(crate) failure: Option<String>,
    pub(crate) public_result: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) stop: Option<alondra_core::orchestrator_batch::BatchStop>,
}

pub(super) fn record(conn: &Connection, batch: &BatchCommand<'_>, actor: &str, envelope: &CommandEnvelope, digest: &[u8], outcome: &CommandOutcome) -> Result<()> {
    ensure!(!conn.is_autocommit(), "a batch effect is recorded in its domain command transaction");
    ensure!(batch.attempt > 0 && !batch.run.is_empty() && !batch.call.is_empty(), "invalid batch effect identity");
    let epoch: String = conn.query_row("SELECT data_epoch FROM orchestrator_batch_runs WHERE run=?1", [batch.run], |row| row.get(0))?;
    ensure!(epoch == envelope.data_epoch.0, "a batch effect belongs to its claimed data epoch");
    let receipt: Option<(Vec<u8>, String)> = conn.query_row("SELECT digest,result FROM semantic_receipts WHERE actor=?1 AND command_id=?2 AND data_epoch=?3", params![actor,envelope.command_id.0,envelope.data_epoch.0], |row| Ok((row.get(0)?,row.get(1)?))).optional()?;
    let Some((actual, result)) = receipt else { anyhow::bail!("a batch effect has no durable domain receipt") };
    ensure!(actual == digest && serde_json::from_str::<CommandOutcome>(&result)? == *outcome, "a batch effect does not match its domain receipt");
    let kind = serde_json::to_value(&envelope.command)?.get("kind").and_then(Value::as_str).unwrap_or("unknown").to_owned();
    repository::exec(conn, "INSERT OR IGNORE INTO orchestrator_batch_effects(run,attempt,call,actor,command_id,data_epoch,digest,kind,revision,changed) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10)",
        params![batch.run,i64::try_from(batch.attempt)?,batch.call,actor,envelope.command_id.0,envelope.data_epoch.0,digest,kind,i64::try_from(outcome.revision)?,outcome.changed])?;
    Ok(())
}

impl DomainStore {
    pub(crate) fn batch_completed(&self, run: &str) -> Result<bool> {
        let completed: Option<bool> = self.conn.query_row("SELECT coalesce(json_extract(report,'$.turn_completed')=1 AND json_extract(report,'$.input_consumed')=1 AND json_extract(report,'$.failure') IS NULL,0) FROM orchestrator_batch_reports WHERE run=?1 AND json_extract(report,'$.kind')!='diagnostic' ORDER BY cursor DESC LIMIT 1", [run], |row| row.get(0)).optional()?;
        Ok(completed.unwrap_or(false))
    }

    pub(crate) fn batch_stop(&self, run: &str) -> Result<Option<(alondra_core::orchestrator_batch::BatchStop, String)>> {
        let report: Option<String> = self.conn.query_row("SELECT report FROM orchestrator_batch_reports WHERE run=?1 AND json_extract(report,'$.kind')!='diagnostic' ORDER BY cursor DESC LIMIT 1", [run], |row| row.get(0)).optional()?;
        let report = report.as_deref().map(serde_json::from_str::<BatchReport>).transpose()?;
        Ok(report.and_then(|report| report.stop.map(|stop| (stop, report.failure.unwrap_or_else(|| "The interrupted native batch requires reconciliation before retrying".into())))))
    }

    pub(crate) fn bind_batch_effects(&mut self, run: &str, epoch: &DataEpoch) -> Result<()> {
        let changed = self.conn.execute("INSERT OR IGNORE INTO orchestrator_batch_runs(run,data_epoch) VALUES(?1,?2)", params![run,epoch.0])?;
        let stored: String = self.conn.query_row("SELECT data_epoch FROM orchestrator_batch_runs WHERE run=?1", [run], |row| row.get(0))?;
        ensure!(stored == epoch.0, "an orchestrator run identity cannot cross data epochs");
        if changed > 0 { self.note_side_write(); }
        Ok(())
    }
    pub(crate) fn execute_from_batch(&mut self, actor: &Actor, envelope: &CommandEnvelope, run: &str) -> std::result::Result<Commit, StoreError> {
        if !matches!(actor, Actor::Orchestrator { .. }) { return Err(DomainError::new("forbidden", "A batch effect requires its orchestrator principal").into()); }
        self.bind_batch_effects(run, &envelope.data_epoch)?;
        let ended: bool = self.conn.query_row("SELECT ended FROM orchestrator_batch_runs WHERE run=?1", [run], |row| row.get(0))
            .map_err(|why| StoreError::Storage(why.into()))?;
        if ended { return Err(DomainError::new("forbidden", "This orchestrator batch is settled; start a fresh batch before mutating domain state").into()); }
        let batch = BatchCommand { run, attempt: 1, call: &envelope.command_id.0 };
        self.execute_envelope(actor, envelope, None, true, Attached::Batch(&batch))
    }

    pub(crate) fn batch_effects(&self, run: &str) -> Result<Vec<BatchEffectRef>> {
        let mut statement = self.conn.prepare("SELECT attempt,call,actor,command_id,data_epoch,digest,kind,revision,changed FROM orchestrator_batch_effects WHERE run=?1 ORDER BY revision,call")?;
        let rows = statement.query_map([run], |row| Ok((row.get::<_, i64>(0)?,row.get::<_, String>(1)?,row.get::<_, String>(2)?,row.get::<_, String>(3)?,row.get::<_, String>(4)?,row.get::<_, Vec<u8>>(5)?,row.get::<_, String>(6)?,row.get::<_, i64>(7)?,row.get::<_, bool>(8)?)))?;
        let mut effects = vec![];
        for row in rows {
            let (attempt,call,actor,command_id,data_epoch,digest,kind,revision,changed) = row?;
            effects.push(BatchEffectRef { run: run.into(), attempt: u64::try_from(attempt)?, call, command_id: command_id.clone(), receipt_ref: BatchReceiptRef { actor,command_id,data_epoch,digest },kind,revision:u64::try_from(revision)?,changed });
        }
        Ok(effects)
    }

    pub(crate) fn batch_run_manifest(&self, epoch: &DataEpoch, after: &str, limit: usize) -> Result<Value> {
        let limit = limit.clamp(1,32);
        let mut statement = self.conn.prepare("SELECT run,ended FROM orchestrator_batch_runs WHERE data_epoch=?1 AND run>?2 ORDER BY run LIMIT ?3")?;
        let rows = statement.query_map(params![epoch.0,after,i64::try_from(limit+1)?],|row|Ok((row.get::<_,String>(0)?,row.get::<_,bool>(1)?)))?;
        let mut entries = Vec::new(); let mut more = false;
        for row in rows {
            let (run,ended)=row?;
            if entries.len()==limit {more=true;break;}
            entries.push(serde_json::json!({"run_id":run,"ended":ended}));
        }
        let next = more.then(||entries.last().and_then(|entry|entry["run_id"].as_str()).unwrap_or_default().to_owned());
        Ok(serde_json::json!({"kind":"runs","entries":entries,"next":next,"instruction":"Use batch_outcomes with run_id to read each batch's durable results; never replay its calls"}))
    }

    pub(crate) fn batch_outcomes(&self, run: &str, epoch: &DataEpoch, after: &str, limit: usize) -> Result<Value> {
        let actual: String = self.conn.query_row("SELECT data_epoch FROM orchestrator_batch_runs WHERE run=?1",[run],|row| row.get(0))?;
        ensure!(actual==epoch.0,"The batch belongs to a different data epoch");
        let mut query = self.conn.prepare("SELECT identity,kind,status,metadata,result,error,code FROM (SELECT 'command:'||call||':'||command_id AS identity,kind,'succeeded' AS status,json_object('actor',actor,'command_id',command_id,'data_epoch',data_epoch,'digest',hex(digest),'revision',revision,'changed',changed) AS metadata,NULL AS result,NULL AS error,NULL AS code FROM orchestrator_batch_effects WHERE run=?1 UNION ALL SELECT 'tool:'||call,kind,state,json_object('call',call),substr(result,1,2048),substr(error,1,2048),json_extract(error,'$.code') FROM orchestrator_tool_operations WHERE run=?1) WHERE identity>?2 ORDER BY identity LIMIT ?3")?;
        let rows = query.query_map(params![run,after,i64::try_from(limit.clamp(1,32)+1)?],|row| Ok((row.get::<_,String>(0)?,row.get::<_,String>(1)?,row.get::<_,String>(2)?,row.get::<_,String>(3)?,row.get::<_,Option<String>>(4)?,row.get::<_,Option<String>>(5)?,row.get::<_,Option<String>>(6)?)))?;
        let mut entries = vec![];
        let mut more = false;
        for row in rows {
            let (identity,kind,status,metadata,result,error,code) = row?;
            if entries.len()==limit.clamp(1,32) { more=true;break; }
            let status = if status=="uncertain" && refused_before_any_effect(code.as_deref()) { "refused".to_owned() } else { status };
            let excerpt = |value:Option<String>| value.map(|mut value| { let mut end=value.len().min(2048);while !value.is_char_boundary(end) {end-=1;}value.truncate(end);value });
            entries.push(serde_json::json!({"id":identity,"kind":kind,"status":status,"receipt_ref":serde_json::from_str::<Value>(&metadata)?,"result_excerpt":excerpt(result),"error_excerpt":excerpt(error)}));
        }
        let next = more.then(|| entries.last().and_then(|entry|entry["id"].as_str()).unwrap_or_default().to_owned());
        Ok(serde_json::json!({"run_id":run,"entries":entries,"next":next,"excerpts_only":true,"instruction":"Read current domain state before continuing; an admitted or uncertain operation is not confirmed success"}))
    }

    pub(crate) fn batch_selection(&self, run: &str) -> Result<Option<alondra_daemon_engine_model::BatchSelection>> {
        let encoded: Option<String> = self.conn.query_row("SELECT selection FROM orchestrator_batch_runs WHERE run=?1", [run], |row| row.get(0)).optional()?.flatten();
        encoded.as_deref().map(serde_json::from_str).transpose().map_err(Into::into)
    }

    pub(crate) fn select_batch(&mut self, run: &str, selection: &impl Serialize) -> Result<()> {
        let changed = self.conn.execute("UPDATE orchestrator_batch_runs SET selection=?2 WHERE run=?1 AND selection IS NULL", params![run,serde_json::to_string(selection)?])?;
        ensure!(changed == 1, "a batch selection is fixed before offering its prompt");
        self.note_side_write();
        Ok(())
    }

    pub(crate) fn record_batch_report(&mut self, run: &str, cursor: u64, evidence: &[u8], report: &BatchReport) -> Result<()> {
        #[cfg(any(test, feature = "test-hooks"))]
        self.orchestrator_write_fault("report")?;
        use sha2::Digest;
        ensure!(cursor > 0 && !report.kind.is_empty() && report.kind.len() <= 128, "invalid batch report");
        ensure!(report.failure.as_ref().is_none_or(|failure| failure.len() <= 16384) && report.public_result.as_ref().is_none_or(|result| result.len() <= 256 * 1024), "batch report capacity");
        let digest = sha2::Sha256::digest(evidence).to_vec();
        let encoded = serde_json::to_string(report)?;
        let tx = self.conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let previous: Option<(Vec<u8>,String)> = tx.query_row("SELECT digest,report FROM orchestrator_batch_reports WHERE run=?1 AND cursor=?2", params![run,i64::try_from(cursor)?], |row| Ok((row.get(0)?,row.get(1)?))).optional()?;
        if let Some((previous, previous_report)) = previous { ensure!(previous == digest && previous_report == encoded, "a batch report cursor was reused with different evidence"); }
        else {
            let last: i64 = tx.query_row("SELECT coalesce(max(cursor),0) FROM orchestrator_batch_reports WHERE run=?1", [run], |row| row.get(0))?;
            ensure!(u64::try_from(last)?.checked_add(1) == Some(cursor), "a batch report has an observation gap");
            let bytes: i64 = tx.query_row("SELECT coalesce(sum(length(CAST(report AS BLOB))),0) FROM orchestrator_batch_reports WHERE run=?1", [run], |row| row.get(0))?;
            ensure!(cursor <= 65536 && bytes.saturating_add(i64::try_from(encoded.len())?) <= 32 * 1024 * 1024, "The batch report journal is full; retain its unacknowledged host evidence");
            tx.execute("INSERT INTO orchestrator_batch_reports(run,cursor,digest,report) VALUES(?1,?2,?3,?4)", params![run,i64::try_from(cursor)?,digest,encoded])?;
        }
        #[cfg(any(test, feature = "test-hooks"))]
        alondra_daemon_engine_model::faults::database_write(&tx, "report_commit")?;
        tx.commit()?;
        self.note_side_write();
        Ok(())
    }

    pub(crate) fn begin_tool_operation(&mut self, run: &str, call: &str, kind: &str, request: &Value) -> Result<Option<(String,Option<String>,Option<String>)>> {
        let encoded = serde_json::to_string(request)?;
        ensure!(encoded.len() <= 512 * 1024 && !call.is_empty() && call.len() <= 128, "The tool operation exceeds its capacity");
        let digest = canonical::digest(&(kind,request))?;
        let tx = self.conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let previous: Option<(String,Vec<u8>,String,Option<String>,Option<String>)> = tx.query_row("SELECT kind,digest,state,result,error FROM orchestrator_tool_operations WHERE run=?1 AND call=?2", params![run,call], |row| Ok((row.get(0)?,row.get(1)?,row.get(2)?,row.get(3)?,row.get(4)?))).optional()?;
        if let Some((old_kind,old_digest,state,result,error)) = previous {
            ensure!(old_kind == kind && old_digest.as_slice() == digest, "A native tool call identity was reused for a different operation");
            return Ok(Some((state,result,error)));
        }
        let ended: bool = tx.query_row("SELECT ended FROM orchestrator_batch_runs WHERE run=?1", [run], |row| row.get(0))?;
        ensure!(!ended, "The orchestrator batch is already settled");
        let (count, bytes, pending) = operation_capacity(&tx, run)?;
        let reserved = pending.saturating_add(1).saturating_mul(OUTCOME_RESERVE);
        ensure!(count < 4096 && bytes.saturating_add(i64::try_from(encoded.len())?).saturating_add(reserved) <= OPERATION_BYTES, "The batch operation journal is full");
        tx.execute("INSERT INTO orchestrator_tool_operations(run,call,kind,digest,request,state) VALUES(?1,?2,?3,?4,?5,'started')", params![run,call,kind,digest.as_slice(),encoded])?;
        tx.commit()?;
        self.note_side_write();
        Ok(None)
    }

    pub(crate) fn complete_tool_operation(&mut self, run: &str, call: &str, answer: &std::result::Result<Value,DomainError>) -> Result<()> {
        let (mut state, mut result, mut error) = match answer {
            Ok(result) => ("succeeded", Some(serde_json::to_string(result)?), None),
            Err(error) => ("uncertain", None, Some(serde_json::to_string(error)?)),
        };
        let tx = self.conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let unfinished: bool = tx.query_row("SELECT state='started' AND result IS NULL AND error IS NULL FROM orchestrator_tool_operations WHERE run=?1 AND call=?2", params![run,call], |row| row.get(0))?;
        ensure!(unfinished, "The tool operation has no unresolved durable intent");
        let (_, bytes, pending) = operation_capacity(&tx, run)?;
        let reserved = pending.saturating_sub(1).saturating_mul(OUTCOME_RESERVE);
        let actual = result.as_ref().map_or(0,String::len).saturating_add(error.as_ref().map_or(0,String::len));
        let bounded = actual > OUTCOME_BYTES || bytes.saturating_add(reserved).saturating_add(i64::try_from(actual)?) > OPERATION_BYTES;
        if bounded {
            state = "uncertain"; result = None;
            let fallback = serde_json::to_string(&DomainError::new("operation_uncertain", "The outcome exceeded the batch journal capacity. Inspect committed effects before continuing; do not repeat this operation."))?;
            ensure!(i64::try_from(fallback.len())? <= OUTCOME_RESERVE, "The operation uncertainty marker exceeds its reservation");
            error = (bytes.saturating_add(reserved).saturating_add(i64::try_from(fallback.len())?) <= OPERATION_BYTES).then_some(fallback);
        }
        let changed = tx.execute("UPDATE orchestrator_tool_operations SET state=?3,result=?4,error=?5 WHERE run=?1 AND call=?2 AND state='started' AND result IS NULL AND error IS NULL", params![run,call,state,result,error])?;
        ensure!(changed == 1, "The tool operation has no unresolved durable intent");
        tx.commit()?;
        self.note_side_write();
        ensure!(!bounded, "The tool outcome exceeded its durable journal capacity; reconcile committed effects before continuing");
        Ok(())
    }

    pub(crate) fn tool_operations(&self, run: &str) -> Result<Vec<(String,String,String,Option<String>,Option<String>)>> {
        let mut query = self.conn.prepare("SELECT call,kind,state,result,error FROM orchestrator_tool_operations WHERE run=?1 ORDER BY call")?;
        let rows = query.query_map([run], |row| Ok((row.get(0)?,row.get(1)?,row.get(2)?,row.get(3)?,row.get(4)?)))?;
        Ok(rows.collect::<std::result::Result<Vec<_>,_>>()?)
    }

    pub(crate) fn finish_batch(&mut self, run: &str) -> Result<()> {
        #[cfg(any(test, feature = "test-hooks"))]
        self.orchestrator_write_fault("finish_batch")?;
        let tx = self.conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        tx.execute("UPDATE orchestrator_batch_runs SET ended=1 WHERE run=?1", [run])?;
        tx.execute("UPDATE orchestrator_batch_effects SET pinned=0 WHERE run=?1", [run])?;
        #[cfg(any(test, feature = "test-hooks"))]
        alondra_daemon_engine_model::faults::database_write(&tx, "finish_batch_commit")?;
        tx.commit()?;
        self.note_side_write();
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain_store::tests::{Fixture, seeded};

    fn title(store: &mut DomainStore, id: &str, text: &str) -> CommandEnvelope {
        let state = store.load().unwrap();
        CommandEnvelope { protocol_version: PROTOCOL_VERSION, data_epoch: state.data_epoch.clone(), command_id: id.into(), expected_revision: 0,
            command: Command::TitleRequest { request_id: "request-1".into(), title: text.into() } }
    }

    fn uncertain(code: &str) -> (String, String, String, Option<String>, Option<String>) {
        ("call".into(), "launch".into(), "uncertain".into(), None, Some(serde_json::to_string(&DomainError::new(code, "why")).unwrap()))
    }

    #[test]
    fn every_refusal_of_the_domain_rules_settles_its_call() {
        let refusals = alondra_core::domain::INTERNAL_CODES.iter().filter(|code| **code != "delivery_uncertain");
        for code in refusals.chain(TOOL_REFUSALS) {
            assert!(!is_unconfirmed(&uncertain(code)), "{code} changed nothing");
        }
    }

    #[test]
    fn an_error_that_leaves_the_effect_unknown_stays_unconfirmed() {
        for code in ["operation_uncertain", "delivery_uncertain", "launch_failed", "replace_failed", "storage_unavailable", "a_code_nobody_knows"] {
            assert!(is_unconfirmed(&uncertain(code)), "{code} may have changed something");
        }
    }

    #[test]
    fn bounded_batch_manifest_recovers_every_run_after_large_handoff_histories_and_restart() {
        let fixture=Fixture::new();let mut store=DomainStore::open(&fixture.path()).unwrap();seeded(&mut store);
        let epoch=store.load().unwrap().data_epoch.clone();
        for index in 0..100 {
            let run=format!("handoff-{index:03}");let command=title(&mut store,&format!("manifest-command-{index}"),&format!("Confirmed result {index}"));
            store.execute_from_batch(&Actor::Orchestrator{generation:0},&command,&run).unwrap();store.finish_batch(&run).unwrap();
        }
        drop(store);let store=DomainStore::open(&fixture.path()).unwrap();let mut after=String::new();let mut found=std::collections::BTreeSet::new();
        loop {
            let page=store.batch_run_manifest(&epoch,&after,7).unwrap();assert!(page["entries"].as_array().unwrap().len()<=7);
            for entry in page["entries"].as_array().unwrap() {
                let run=entry["run_id"].as_str().unwrap();assert!(found.insert(run.to_owned()));assert_eq!(entry["ended"],true);
                let result=store.batch_outcomes(run,&epoch,"",32).unwrap();assert!(!result["entries"].as_array().unwrap().is_empty(),"every omitted prompt reference remains queryable");
            }
            match page["next"].as_str(){Some(next)=>{assert!(next>after.as_str());after=next.into()},None=>break}
        }
        assert_eq!(found.len(),100);
        assert!(store.batch_run_manifest(&DataEpoch("another-epoch".into()),"",32).unwrap()["entries"].as_array().unwrap().is_empty());
    }
    #[test]
    fn a_batch_effect_and_its_domain_result_commit_and_reopen_together() {
        let fixture = Fixture::new();
        let mut store = DomainStore::open(&fixture.path()).unwrap();
        seeded(&mut store);
        let command = title(&mut store, "batch-command", "Confirmed title");
        let result = store.execute_from_batch(&Actor::Orchestrator { generation: 0 }, &command, "batch").unwrap().outcome;
        let effects = store.batch_effects("batch").unwrap();
        assert_eq!(effects.len(), 1);
        assert_eq!(effects[0].revision, result.revision);
        assert_eq!(effects[0].receipt_ref.digest, canonical::digest(&command).unwrap());
        drop(store);
        let mut store = DomainStore::open(&fixture.path()).unwrap();
        assert_eq!(store.batch_effects("batch").unwrap(), effects);
        let replay = store.execute_from_batch(&Actor::Orchestrator { generation: 0 }, &command, "batch").unwrap();
        assert!(replay.replayed);
        assert_eq!(replay.outcome, result);
        assert_eq!(store.batch_effects("batch").unwrap().len(), 1);
    }

    /// The orchestrator raised a card for the user and withdrew it in the same pass: its notice
    /// waits for the pass to end, by when the card is gone and no push leaves for it.
    #[test]
    fn a_card_the_orchestrator_puts_in_a_pass_is_announced_only_after_that_pass() {
        let fixture = Fixture::new();
        let mut store = DomainStore::open(&fixture.path()).unwrap();
        seeded(&mut store);
        let epoch = store.load().unwrap().data_epoch.clone();
        let envelope = |id: &str, command: Command| CommandEnvelope {
            protocol_version: PROTOCOL_VERSION, data_epoch: epoch.clone(), command_id: id.into(), expected_revision: 0, command,
        };
        let options = vec![QuestionOption { id: "resume".into(), label: "Resume the task".into(), description: None }];
        let raise = envelope("raise", Command::RaiseProblem {
            attachments: vec![], interaction_id: "message-paused".into(), scope: Scope::Orchestrator { request_id: None },
            body: "A message waits for a paused task".into(), options,
            card: CardSpec::new(CardKind::Problem, "A message waits for a paused task").why("Its node is paused").content(ContentKind::Diagnosis, "Paused"),
        });
        store.execute_from_batch(&Actor::Orchestrator { generation: 0 }, &raise, "batch").unwrap();
        let notice: i64 = store.conn.query_row("SELECT m.id FROM domain_messages m JOIN domain_message_user_targets u ON u.message_id=m.id \
            WHERE m.interaction_id='message-paused'", [], |row| row.get(0)).unwrap();
        assert!(store.user_notices_in_open_passes().unwrap().contains(&notice), "its notice waits while the pass runs");
        store.execute_from_batch(&Actor::Orchestrator { generation: 0 }, &envelope("withdraw", Command::Withdraw { interaction_id: "message-paused".into() }), "batch").unwrap();
        store.finish_batch("batch").unwrap();
        assert!(store.user_notices_in_open_passes().unwrap().is_empty(), "the notice goes on once the pass ended");
        assert!(store.load().unwrap().interactions[&InteractionId::from("message-paused")].withdrawn, "by then the card is gone, so the push gate drops it");
    }

    #[test]
    fn failure_to_record_a_batch_effect_rolls_back_the_mutation_and_receipt() {
        let fixture = Fixture::new();
        let mut store = DomainStore::open(&fixture.path()).unwrap();
        seeded(&mut store);
        let before = store.load().unwrap();
        store.conn.execute_batch("CREATE TRIGGER refuse_batch_effect BEFORE INSERT ON orchestrator_batch_effects BEGIN SELECT RAISE(ABORT,'injected effect failure'); END;").unwrap();
        let command = title(&mut store, "refused-command", "Must roll back");
        assert!(store.execute_from_batch(&Actor::Orchestrator { generation: 0 }, &command, "batch").is_err());
        assert_eq!(store.load().unwrap(), before);
        let receipts: i64 = store.conn.query_row("SELECT count(*) FROM semantic_receipts WHERE command_id='refused-command'", [], |row| row.get(0)).unwrap();
        assert_eq!(receipts, 0);
        assert!(store.batch_effects("batch").unwrap().is_empty());
    }

    #[test]
    fn pending_batch_receipts_survive_retention_until_the_batch_is_settled() {
        let fixture = Fixture::new();
        let mut store = DomainStore::open(&fixture.path()).unwrap();
        seeded(&mut store);
        let command = title(&mut store, "pinned-command", "Confirmed title");
        store.execute_from_batch(&Actor::Orchestrator { generation: 0 }, &command, "batch").unwrap();
        store.conn.execute("UPDATE semantic_receipts SET at=0 WHERE command_id='pinned-command'", []).unwrap();
        let policy = crate::domain_store::retention::POLICIES.iter().find(|policy| policy.name == "receipts").unwrap();
        let now = 10 * 24 * 60 * 60 * 1000;
        assert_eq!(store.retain(policy, now, 100).unwrap(), 0);
        store.finish_batch("batch").unwrap();
        assert_eq!(store.retain(policy, now, 100).unwrap(), 1);
    }

    #[test]
    fn batch_reports_reject_gaps_and_conflicting_replays() {
        let fixture = Fixture::new();
        let mut store = DomainStore::open(&fixture.path()).unwrap();
        let epoch = store.load().unwrap().data_epoch.clone();
        store.bind_batch_effects("batch", &epoch).unwrap();
        let report = BatchReport { kind: "turn/started".into(), native_turn: Some("turn".into()), ..Default::default() };
        assert!(store.record_batch_report("batch", 2, b"event", &report).is_err());
        store.record_batch_report("batch", 1, b"event", &report).unwrap();
        store.record_batch_report("batch", 1, b"event", &report).unwrap();
        assert!(store.record_batch_report("batch", 1, b"different event", &report).is_err());
        let changed = BatchReport { turn_completed: true, ..report };
        assert!(store.record_batch_report("batch", 1, b"event", &changed).is_err());
    }

    #[test]
    fn a_settled_batch_cannot_accept_or_replay_domain_mutations() {
        let fixture = Fixture::new(); let mut store = DomainStore::open(&fixture.path()).unwrap(); seeded(&mut store);
        let actor = Actor::Orchestrator { generation: 0 };
        let command = title(&mut store, "original-command", "Confirmed title");
        store.execute_from_batch(&actor, &command, "batch").unwrap();
        store.finish_batch("batch").unwrap();
        let before = store.load().unwrap();
        let fresh = title(&mut store, "late-command", "Forbidden late title");
        for denied in [&command, &fresh] {
            assert!(matches!(store.execute_from_batch(&actor, denied, "batch"), Err(StoreError::Domain(ref error)) if error.code == "forbidden"));
        }
        assert_eq!(store.load().unwrap().revision, before.revision);
        assert_eq!(store.batch_effects("batch").unwrap().len(), 1);
        assert_eq!(store.conn.query_row("SELECT count(*) FROM semantic_receipts WHERE command_id='late-command'", [], |row| row.get::<_, i64>(0)).unwrap(), 0);
        assert!(!store.batch_outcomes("batch", &before.data_epoch, "", 16).unwrap()["entries"].as_array().unwrap().is_empty(), "settled outcome reads remain available");
    }


    fn fill_operation_errors(store: &mut DomainStore, run: &str, headroom: i64) {
        let epoch = store.load().unwrap().data_epoch.clone(); store.bind_batch_effects(run, &epoch).unwrap();
        let overhead = serde_json::to_string(&DomainError::new("padding", "")).unwrap().len();
        let target = OPERATION_BYTES - headroom;
        let mut index = 0;
        let mut written = 0_i64;
        while written < target {
            let size = usize::try_from(target - written - 4).unwrap().min(OUTCOME_BYTES);
            assert!(size >= overhead);
            let message_bytes = size - overhead;
            let mut message = "é".repeat(message_bytes / 2); if message_bytes % 2 != 0 { message.push('x'); }
            let error = DomainError::new("padding", message);
            assert_eq!(serde_json::to_string(&error).unwrap().len(), size);
            let call = format!("padding-{index}"); index += 1;
            store.begin_tool_operation(run, &call, "padding", &Value::Null).unwrap();
            store.complete_tool_operation(run, &call, &Err(error)).unwrap();
            written += i64::try_from(size + 4).unwrap();
        }
        assert_eq!(operation_capacity(&store.conn, run).unwrap().1, written);
    }

    #[test]
    fn multibyte_error_evidence_counts_toward_admission_and_keeps_outcome_reservations() {
        let fixture = Fixture::new(); let mut store = DomainStore::open(&fixture.path()).unwrap();
        fill_operation_errors(&mut store, "budget", 768);
        let before = operation_capacity(&store.conn, "budget").unwrap();
        let request = Value::String("é".repeat(200));
        assert!(store.begin_tool_operation("budget", "too-large", "launch", &request).is_err());
        assert_eq!(operation_capacity(&store.conn, "budget").unwrap(), before, "old evidence is never trimmed to admit another operation");
    }

    #[test]
    fn competing_completions_serialize_the_shared_byte_budget_and_preserve_uncertain_results() {
        let fixture = Fixture::new(); let mut first = DomainStore::open(&fixture.path()).unwrap();
        fill_operation_errors(&mut first, "budget", 4096);
        for call in ["first", "second"] { first.begin_tool_operation("budget", call, "launch", &Value::Null).unwrap(); }
        let mut second = DomainStore::open(&fixture.path()).unwrap();
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(3));
        let outcomes = std::thread::scope(|scope| {
            let a = std::sync::Arc::clone(&barrier); let b = std::sync::Arc::clone(&barrier);
            let first = scope.spawn(move || { a.wait(); first.complete_tool_operation("budget", "first", &Ok(Value::String("é".repeat(1500)))).is_ok() });
            let second = scope.spawn(move || { b.wait(); second.complete_tool_operation("budget", "second", &Ok(Value::String("é".repeat(1500)))).is_ok() });
            barrier.wait(); [first.join().unwrap(), second.join().unwrap()]
        });
        assert_eq!(outcomes.into_iter().filter(|ok| *ok).count(), 1, "only one full outcome fits; the other is durably uncertain");
        let mut store = DomainStore::open(&fixture.path()).unwrap();
        let (_, bytes, pending) = operation_capacity(&store.conn, "budget").unwrap();
        assert!(bytes <= OPERATION_BYTES); assert_eq!(pending, 0);
        let mut statuses = vec![];
        for call in ["first", "second"] {
            let previous = store.begin_tool_operation("budget", call, "launch", &Value::Null).unwrap().unwrap();
            if previous.0 == "uncertain" {
                assert!(previous.1.is_none());
                assert_eq!(serde_json::from_str::<DomainError>(previous.2.as_deref().unwrap()).unwrap().code, "operation_uncertain");
            }
            statuses.push(previous.0);
        }
        statuses.sort(); assert_eq!(statuses, ["succeeded", "uncertain"]);
    }

    #[test]
    fn an_oversized_result_becomes_durable_uncertainty_instead_of_a_reexecutable_started_operation() {
        let fixture = Fixture::new(); let mut store = DomainStore::open(&fixture.path()).unwrap();
        let epoch = store.load().unwrap().data_epoch.clone(); store.bind_batch_effects("batch", &epoch).unwrap();
        store.begin_tool_operation("batch", "call", "launch", &Value::Null).unwrap();
        assert!(store.complete_tool_operation("batch", "call", &Ok(Value::String("x".repeat(OUTCOME_BYTES)))).is_err());
        drop(store); let mut store = DomainStore::open(&fixture.path()).unwrap();
        let previous = store.begin_tool_operation("batch", "call", "launch", &Value::Null).unwrap().unwrap();
        assert_eq!(previous.0, "uncertain"); assert!(previous.1.is_none()); assert!(previous.2.is_some());
        assert_eq!(operation_capacity(&store.conn, "batch").unwrap().0, 1);
    }

}
