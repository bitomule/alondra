//! Native Codex cumulative counters, scoped by account and thread before their measured deltas
//! enter the existing provider-neutral session metrics. The first unknown baseline is not newly measured usage.
use alondra_core::Millis;
use rusqlite::{Connection, OptionalExtension, params};
use serde_json::Value;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Counters { input: i64, output: i64, read: i64, write: i64 }
impl Counters {
    fn parse(value: &Value) -> Option<Self> {
        let number = |name| value.get(name)?.as_u64().and_then(|n| i64::try_from(n).ok());
        let input = number("inputTokens")?; let output = number("outputTokens")?;
        let read = number("cachedInputTokens")?;
        let write = match value.get("cacheWriteInputTokens") { None => 0, Some(_) => number("cacheWriteInputTokens")? };
        if write != 0 { return None; }
        let uncached = input.checked_sub(read)?;
        (uncached >= 0 && input.checked_add(output).is_some()).then_some(Self { input: uncached, output, read, write })
    }
    fn delta(self, before: Self) -> Option<Self> {
        let delta = Self { input: self.input.checked_sub(before.input)?, output: self.output.checked_sub(before.output)?, read: self.read.checked_sub(before.read)?, write: self.write.checked_sub(before.write)? };
        [delta.input, delta.output, delta.read, delta.write].into_iter().all(|n| n >= 0).then_some(delta)
    }
    fn empty(self) -> bool { self == Self { input: 0, output: 0, read: 0, write: 0 } }
}

pub(super) fn stage(conn: &Connection, account: &str, task: &str, message: &Value, at: Millis) -> rusqlite::Result<()> {
    let Some(method) = message.get("method").and_then(Value::as_str) else { return Ok(()) };
    let Some(p) = message.get("params") else { return Ok(()) };
    let Some(thread) = p.get("threadId").and_then(Value::as_str).filter(|id| !id.is_empty()) else { return Ok(()) };
    if method == "alondra/inputAccepted" {
        let Some(turn) = p.get("turnId").and_then(Value::as_str).filter(|id| !id.is_empty()) else { return Ok(()) };
        let model = p.get("model").and_then(Value::as_str).filter(|id| !id.is_empty());
        conn.execute("INSERT INTO codex_usage_baselines(account,thread_id,turn_id,model,task_id,last_at) VALUES(?1,?2,?3,?4,?6,?5)
            ON CONFLICT(account,thread_id) DO UPDATE SET turn_id=excluded.turn_id,model=excluded.model,last_at=excluded.last_at", params![account,thread,turn,model,at,task])?;
        return Ok(());
    }
    if method == "model/rerouted" {
        let text = |name| p.get(name).and_then(Value::as_str).filter(|value| !value.is_empty() && value.len() <= 512 && !value.chars().any(char::is_control));
        let (Some(turn), Some(_from), Some(target), Some(_reason)) = (text("turnId"), text("fromModel"), text("toModel"), text("reason")) else { return Ok(()) };
        conn.execute("UPDATE codex_usage_baselines SET model=?4,input=NULL,output=NULL,cache_read=NULL,cache_write=NULL,last_at=?5
            WHERE account=?1 AND thread_id=?2 AND turn_id=?3 AND model IS NOT ?4 AND task_id=?6", params![account,thread,turn,target,at,task])?;
        return Ok(());
    }
    if method != "thread/tokenUsage/updated" { return Ok(()) }
    let Some(turn) = p.get("turnId").and_then(Value::as_str).filter(|id| !id.is_empty()) else { return Ok(()) };
    let Some(current) = p.pointer("/tokenUsage/total").and_then(Counters::parse) else { return Ok(()) };
    type Row = (String, Option<String>, Option<i64>, Option<i64>, Option<i64>, Option<i64>);
    let prior: Option<Row> = conn.query_row("SELECT turn_id,model,input,output,cache_read,cache_write FROM codex_usage_baselines WHERE account=?1 AND thread_id=?2", params![account,thread], |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?,r.get(5)?))).optional()?;
    if prior.as_ref().is_some_and(|(known_turn, ..)| known_turn != turn) {
        conn.execute("UPDATE codex_usage_baselines SET input=NULL,output=NULL,cache_read=NULL,cache_write=NULL,last_at=?3 WHERE account=?1 AND thread_id=?2", params![account,thread,at])?;
        return Ok(());
    }
    let mut measured = None;
    if let Some((known_turn, Some(model), Some(input), Some(output), Some(read), Some(write))) = prior {
        if known_turn == turn { measured = current.delta(Counters { input,output,read,write }).map(|delta| (model,delta)); }
    }
    conn.execute("INSERT INTO codex_usage_baselines(account,thread_id,turn_id,input,output,cache_read,cache_write,task_id,last_at) VALUES(?1,?2,?3,?4,?5,?6,?7,?9,?8)
        ON CONFLICT(account,thread_id) DO UPDATE SET input=excluded.input,output=excluded.output,cache_read=excluded.cache_read,cache_write=excluded.cache_write,last_at=excluded.last_at", params![account,thread,turn,current.input,current.output,current.read,current.write,at,task])?;
    let Some((model,delta)) = measured.filter(|(_,delta)| !delta.empty()) else { return Ok(()) };
    let session = serde_json::to_string(&("codex",account,thread)).expect("string tuple serializes");
    conn.execute("INSERT INTO usage_sessions(session_id,model,provider,task_id,role,cost_micros,input,output,cache_read,cache_write,complete,first_at,last_at)
        VALUES(?1,?2,'codex',?3,'node',NULL,?4,?5,?6,?7,0,?8,?8)
        ON CONFLICT(session_id,model) DO UPDATE SET input=input+excluded.input,output=output+excluded.output,cache_read=cache_read+excluded.cache_read,cache_write=cache_write+excluded.cache_write,folded=0,last_at=excluded.last_at", params![session,format!("codex/{model}"),task,delta.input,delta.output,delta.read,delta.write,at])?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    fn connection() -> Connection { let conn = Connection::open_in_memory().unwrap(); conn.execute_batch("PRAGMA foreign_keys=ON; CREATE TABLE tasks(id TEXT PRIMARY KEY); INSERT INTO tasks VALUES('task');").unwrap(); conn.execute_batch(super::super::MODEL_METRICS).unwrap(); conn }
    fn accepted(conn: &Connection, account: &str, turn: &str, model: &str) { stage(conn,account,"task",&json!({"method":"alondra/inputAccepted","params":{"threadId":"thread","turnId":turn,"model":model}}),100).unwrap(); }
    fn sample(conn: &Connection, account: &str, turn: &str, input: i64, cached: i64, output: i64) { stage(conn,account,"task",&json!({"method":"thread/tokenUsage/updated","params":{"threadId":"thread","turnId":turn,"tokenUsage":{"total":{"inputTokens":input,"outputTokens":output,"cachedInputTokens":cached,"cacheWriteInputTokens":0},"last":{"inputTokens":999999},"modelContextWindow":400000}}}),101).unwrap(); }
    fn rows(conn: &Connection) -> Vec<(String,i64,i64,i64,Option<i64>,bool)> { let mut q=conn.prepare("SELECT model,input,output,cache_read,cost_micros,complete FROM usage_sessions ORDER BY model").unwrap(); q.query_map([],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?,r.get(5)?))).unwrap().collect::<Result<_,_>>().unwrap() }
    #[test]
    fn unknown_resumed_history_and_repeated_samples_are_not_new_spend() {
        let conn=connection(); accepted(&conn,"codex/default","turn1","native"); sample(&conn,"codex/default","turn1",100,20,10); assert!(rows(&conn).is_empty());
        sample(&conn,"codex/default","turn1",150,30,25); sample(&conn,"codex/default","turn1",150,30,25);
        assert_eq!(rows(&conn),vec![("codex/native".into(),40,15,10,None,false)]);
    }
    #[test]
    fn model_switch_uses_increment_and_account_namespace_remains_independent() {
        let conn=connection(); accepted(&conn,"codex/default","first","a"); sample(&conn,"codex/default","first",100,0,10); sample(&conn,"codex/default","first",130,0,15);
        accepted(&conn,"codex/default","second","b"); sample(&conn,"codex/default","second",150,0,21);
        accepted(&conn,"codex/work","first","a"); sample(&conn,"codex/work","first",1000,0,100);
        assert_eq!(rows(&conn),vec![("codex/a".into(),30,5,0,None,false),("codex/b".into(),20,6,0,None,false)]);
    }
    #[test]
    fn counter_reset_or_unknown_turn_is_not_attributed_to_the_current_model() {
        let conn=connection(); accepted(&conn,"codex/default","turn","a"); sample(&conn,"codex/default","turn",100,0,10);
        sample(&conn,"codex/default","turn",10,0,1); assert!(rows(&conn).is_empty());
        sample(&conn,"codex/default","old-turn",20,0,2); assert!(rows(&conn).is_empty());
        sample(&conn,"codex/default","turn",25,0,3); assert!(rows(&conn).is_empty(), "the interval crossing an unknown turn is not attributable");
        sample(&conn,"codex/default","turn",30,0,4); assert_eq!(rows(&conn)[0].1,5);
    }
    #[test]
    fn baseline_and_metrics_roll_back_in_the_same_transaction() {
        let conn=connection(); accepted(&conn,"codex/default","turn","a"); sample(&conn,"codex/default","turn",100,0,10);
        conn.execute_batch("BEGIN").unwrap(); sample(&conn,"codex/default","turn",140,0,14); assert!(!rows(&conn).is_empty()); conn.execute_batch("ROLLBACK").unwrap(); assert!(rows(&conn).is_empty());
        sample(&conn,"codex/default","turn",150,0,15); assert_eq!(rows(&conn)[0].1,50);
    }
    #[test]
    fn malformed_or_overflowing_native_counters_are_unknown() {
        assert!(Counters::parse(&json!({"inputTokens":10,"outputTokens":1,"cachedInputTokens":11,"cacheWriteInputTokens":0})).is_none());
        assert!(Counters::parse(&json!({"inputTokens":u64::MAX,"outputTokens":1,"cachedInputTokens":0,"cacheWriteInputTokens":0})).is_none());
        assert!(Counters::parse(&json!({"inputTokens":10,"outputTokens":1})).is_none());
        assert_eq!(Counters::parse(&json!({"totalTokens":11,"inputTokens":10,"cachedInputTokens":2,"outputTokens":1,"reasoningOutputTokens":1})),Some(Counters {input:8,output:1,read:2,write:0}));
        assert!(Counters::parse(&json!({"inputTokens":10,"outputTokens":1,"cachedInputTokens":0,"cacheWriteInputTokens":1})).is_none());
    }
    fn rerouted(conn: &Connection, account: &str, turn: &str, target: &str) {
        stage(conn, account, "task", &json!({"method":"model/rerouted","params":{"threadId":"thread","turnId":turn,"fromModel":"a","toModel":target,"reason":"highRiskCyberActivity"}}), 2).unwrap();
    }
    #[test]
    fn native_rerouting_skips_the_mixed_interval_and_measures_only_later_target_model_deltas() {
        let conn=connection(); accepted(&conn,"codex/default","turn","a"); sample(&conn,"codex/default","turn",100,0,10);
        sample(&conn,"codex/default","turn",130,0,13);
        rerouted(&conn,"codex/default","turn","b"); sample(&conn,"codex/default","turn",200,0,20);
        sample(&conn,"codex/default","turn",220,0,22);
        rerouted(&conn,"codex/default","turn","b"); sample(&conn,"codex/default","turn",230,0,23);
        assert_eq!(rows(&conn),vec![("codex/a".into(),30,3,0,None,false),("codex/b".into(),30,3,0,None,false)]);
    }
    #[test]
    fn a_reroute_for_an_unknown_turn_or_account_cannot_change_the_current_model_baseline() {
        let conn=connection(); accepted(&conn,"codex/default","turn","a"); sample(&conn,"codex/default","turn",100,0,10);
        rerouted(&conn,"codex/default","old-turn","b"); rerouted(&conn,"codex/work","turn","b");
        sample(&conn,"codex/default","turn",140,0,14);
        assert_eq!(rows(&conn),vec![("codex/a".into(),40,4,0,None,false)]);
    }
    #[test]
    fn a_native_reroute_baseline_rolls_back_with_its_owner_transaction() {
        let conn=connection(); accepted(&conn,"codex/default","turn","a"); sample(&conn,"codex/default","turn",100,0,10);
        conn.execute_batch("BEGIN").unwrap(); rerouted(&conn,"codex/default","turn","b"); conn.execute_batch("ROLLBACK").unwrap();
        sample(&conn,"codex/default","turn",110,0,11);
        assert_eq!(rows(&conn),vec![("codex/a".into(),10,1,0,None,false)]);
    }

    #[test]
    fn a_lower_late_total_from_an_unknown_turn_cannot_inflate_current_model_usage() {
        let conn=connection(); accepted(&conn,"codex/default","turn","a"); sample(&conn,"codex/default","turn",100,0,10);
        sample(&conn,"codex/default","turn",200,0,20);
        sample(&conn,"codex/default","old-turn",50,0,5);
        sample(&conn,"codex/default","turn",210,0,21);
        sample(&conn,"codex/default","turn",220,0,22);
        assert_eq!(rows(&conn),vec![("codex/a".into(),110,11,0,None,false)]);
    }

}
