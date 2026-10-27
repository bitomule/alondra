use crate::node_reports::{Answer, Report, Reported, short_label};
use alondra_core::activity::Activity;
use alondra_core::views::RunOutcome;
use alondra_core::{BackgroundItem, BackgroundKind, Millis, ToolInFlight};
use serde_json::Value;
use std::collections::BTreeMap;

#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct Readings {
    thread: Option<String>,
    turn: Option<String>,
    model:Option<String>,
    model_turn:Option<String>,
    rerouted_turn:Option<String>,
    answer: String,
    deltas: BTreeMap<String, String>,
    calls: BTreeMap<String, ToolInFlight>,
    since: Millis,
}
fn text<'a>(value: &'a Value, key: &str) -> Option<&'a str> { value.get(key).and_then(Value::as_str) }
fn clipped(text: &str) -> String { text.chars().take(8_000).collect() }

pub(crate) fn restore_scope(session:&mut Reported,scope:alondra_provider_codex::LiveScope,now:Millis){
    let mut readings=session.codex.take().filter(|readings|readings.thread.as_deref()==Some(&scope.thread)).unwrap_or_default();
    readings.thread=Some(scope.thread);readings.turn=scope.turn;readings.model=scope.model;readings.model_turn=scope.model_turn;readings.rerouted_turn=scope.rerouted_turn;readings.since=now;
    session.cwd=scope.cwd;session.model=readings.model.clone();session.working_since=readings.turn.as_ref().map(|_|now);session.codex=Some(readings);
}

pub(crate) fn observe(session: &mut Reported, message: &Value, now: Millis) {
    let mut readings = session.codex.take().unwrap_or_default();
    let params = message.get("params").unwrap_or(&Value::Null);
    let method = text(message, "method").unwrap_or("");
    // Only the host creates inputAccepted for its root. It also restores live scope after adoption.
    if readings.thread.is_none() && method=="alondra/inputAccepted" {readings.thread=text(params,"threadId").filter(|id|!id.is_empty()).map(str::to_owned);}
    if method == "alondra/threadReady" {
        readings.thread = text(params, "threadId").map(str::to_owned);
        session.cwd = text(params, "cwd").map(str::to_owned);
        readings.model=text(params,"model").filter(|model|!model.is_empty()).map(str::to_owned);
        session.model=readings.model.clone();readings.model_turn=None;
    } else if text(params, "threadId") == readings.thread.as_deref() && readings.thread.is_some() {
        match method {
            "alondra/inputAccepted" | "turn/started" => {
                let turn = text(params, "turnId").or_else(|| params.pointer("/turn/id").and_then(Value::as_str));
                if turn != readings.turn.as_deref() {
                    readings.turn = turn.map(str::to_owned); readings.answer.clear(); readings.deltas.clear(); readings.since = now;readings.model_turn=None;readings.rerouted_turn=None;
                    session.record(Report::TurnStart, now);
                }
                if method=="alondra/inputAccepted" && turn.is_some() {
                    if readings.rerouted_turn.as_deref()!=turn && let Some(model)=text(params,"model").filter(|model|!model.is_empty()){readings.model=Some(model.into());session.record(Report::Model{model:model.into(),agent_id:None},now);}
                    if text(params,"model").is_some_and(|model|!model.is_empty()) || readings.rerouted_turn.as_deref()==turn{readings.model_turn=turn.map(str::to_owned);}
                }
            }
            "model/rerouted" => {
                if let (Some(turn),Some(from),Some(to))=(text(params,"turnId"),text(params,"fromModel"),text(params,"toModel"))
                    && Some(turn)==readings.turn.as_deref() && !to.is_empty() && text(params,"reason")==Some("highRiskCyberActivity")
                    && readings.model.as_deref().is_none_or(|known|known==from) {
                    readings.model=Some(to.into());readings.model_turn=Some(turn.into());readings.rerouted_turn=Some(turn.into());session.record(Report::Model{model:to.into(),agent_id:None},now);
                }
            }
            "item/agentMessage/delta" => {
                if let (Some(id), Some(delta)) = (text(params, "itemId"), text(params, "delta")) {
                    if readings.deltas.contains_key(id) || readings.deltas.len() < 8 {
                        let current = readings.deltas.entry(id.to_owned()).or_default();
                        *current = clipped(&(current.clone() + delta));
                        session.answer = Some(Answer { text: current.clone(), at: now, aborted: false });
                    }
                }
            }
            "item/started" | "item/completed" => {
                let item = params.get("item").unwrap_or(&Value::Null);
                let kind = text(item, "type").unwrap_or("");
                let id = text(item, "id").unwrap_or("");
                let completed = method == "item/completed";
                if completed && kind == "agentMessage" {
                    if let Some(text) = text(item, "text") {
                        readings.answer = clipped(text);
                        readings.deltas.remove(id);
                        session.answer = Some(Answer { text: readings.answer.clone(), at: now, aborted: false });
                    }
                }
                let tool = match kind {
                    "commandExecution" => Some(("exec_command".to_owned(), text(item, "command").map(short_label))),
                    "fileChange" => Some(("apply_patch".to_owned(), None)),
                    "mcpToolCall" => Some((format!("mcp__{}__{}", text(item, "server").unwrap_or("unknown"), text(item, "tool").unwrap_or("unknown")), None)),
                    "dynamicToolCall" => text(item, "tool").map(|name| (name.into(), None)),
                    _ => None,
                };
                if let Some((name, summary)) = tool {
                    if !completed && !id.is_empty() && readings.calls.len() < 64 {
                        readings.calls.insert(id.into(), ToolInFlight { tool_use_id: Some(id.into()), name, summary, since: now });
                    } else if completed { readings.calls.remove(id); }
                    session.tool = readings.calls.values().next().cloned();
                    if completed { session.end_background(|known| known == id, RunOutcome::Finished, now); }
                }
                if kind == "collabAgentToolCall" {
                    for (thread, state) in item.get("agentsStates").and_then(Value::as_object).into_iter().flatten() {
                        let status = text(state, "status").unwrap_or("");
                        if ["completed", "errored", "shutdown", "notFound"].contains(&status) {
                            session.work.agent_finished(thread, if status == "completed" { RunOutcome::Finished } else { RunOutcome::Stopped }, now);
                            session.end_background(|known| known == thread, RunOutcome::Finished, now);
                        } else if !session.background.iter().any(|(id, _)| id == thread) && session.background.len() < 64 {
                            session.record(Report::SubagentStart { id: thread.clone(), description: text(item, "prompt").map(short_label), kind: Some("codex".into()), model: text(item, "model").map(str::to_owned) }, now);
                        }
                    }
                }
            }
            "turn/plan/updated" => {
                let steps = params.get("plan").and_then(Value::as_array).into_iter().flatten().enumerate().take(50).map(|(index, step)| crate::node_work::StepIn {
                    id: format!("codex-plan-{index}"), subject: text(step, "step").map(str::to_owned),
                    status: match text(step, "status") { Some("completed") => Some(crate::node_work::StepStatus::Completed), Some("inProgress" | "in_progress") => Some(crate::node_work::StepStatus::InProgress), Some("pending") => Some(crate::node_work::StepStatus::Pending), _ => None }, active_form: None,
                }).collect();
                session.record(Report::Steps { steps }, now);
            }
            "turn/completed" if params.pointer("/turn/id").and_then(Value::as_str) == readings.turn.as_deref() => {
                let aborted = params.pointer("/turn/status").and_then(Value::as_str) == Some("interrupted");
                session.record(Report::TurnComplete { answer: readings.answer.clone(), aborted }, now);
                for (id, call) in &readings.calls {
                    if call.name == "exec_command" && !session.background.iter().any(|(known, _)| known == id) && session.background.len() < 64 {
                        session.background.push((id.clone(), BackgroundItem { kind: BackgroundKind::Shell, label: call.summary.clone().unwrap_or_else(|| "Command".into()), started_at: call.since }));
                    }
                }
                readings.turn = None;
            }
            _ => {}
        }
    }
    session.codex = Some(readings);
}
pub(crate) fn activity(session: &Reported) -> Option<Vec<Activity>> {
    let readings = session.codex.as_ref()?;
    let main = (readings.turn.is_some() || session.tool.is_some()).then(|| Activity { agent_id: None, agent_type: None, task: None, tool: session.tool.as_ref().map(|tool| tool.name.clone()), summary: session.tool.as_ref().and_then(|tool| tool.summary.clone()), since: session.tool.as_ref().map_or(readings.since, |tool| tool.since) });
    let children = session.work.agents.iter().filter(|agent| agent.end.is_none()).map(|agent| Activity { agent_id: Some(agent.id.clone()), agent_type: Some("codex".into()), task: None, tool: None, summary: Some(agent.description.clone()), since: agent.started_at });
    Some(main.into_iter().chain(children).collect())
}
pub(crate) fn ended(session: &mut Reported, now: Millis) {
    if session.codex.is_none() { return; }
    session.tool = None; session.working_since = None;
    session.end_background(|_| true, RunOutcome::Stopped, now); session.work.ended(now);
    if let Some(readings) = session.codex.as_mut() { readings.calls.clear(); readings.turn = None; readings.deltas.clear(); }
}
fn update_context(contexts:&crate::idle_compaction::Contexts,task:&alondra_core::domain::TaskId,before:Option<&Readings>,session:&Reported,message:&Value,now:Millis){
    use alondra_provider_claude::ContextEvent;
    let Some(current)=session.codex.as_ref() else{return};
    if before.is_some_and(|prior|prior.thread!=current.thread || prior.model!=current.model || (prior.turn!=current.turn && current.turn.is_some() && current.model_turn.is_none())){contexts.forget(task);}
    let params=message.get("params").unwrap_or(&Value::Null);
    if current.thread.is_none() || text(params,"threadId")!=current.thread.as_deref(){return}
    match text(message,"method"){
        Some("alondra/inputAccepted")=>contexts.observed(task,ContextEvent::Requested,now),
        Some("thread/tokenUsage/updated") if text(params,"turnId").is_some() && text(params,"turnId")==current.turn.as_deref() && text(params,"turnId")==current.model_turn.as_deref()=>{
            let Some(tokens)=params.pointer("/tokenUsage/last/totalTokens").and_then(Value::as_u64) else{return};
            if let Some(model)=current.model.as_ref(){
                if let Some(window)=params.pointer("/tokenUsage/modelContextWindow").and_then(Value::as_u64).filter(|window|*window>0){contexts.observed(task,ContextEvent::Windows(vec![(model.clone(),window)]),now);}
                contexts.observed(task,ContextEvent::Answered{tokens,cache:None,model:Some(model.clone())},now);
            }
        }
        Some("thread/compacted")=>contexts.observed(task,ContextEvent::Compacted,now),
        Some("item/completed") if params.pointer("/item/type").and_then(Value::as_str)==Some("contextCompaction")=>contexts.observed(task,ContextEvent::Compacted,now),
        _=>{}
    }
}
pub(crate) fn feed(domain: &crate::domain_runtime::DomainRuntime, agent: &alondra_core::domain::AgentId, observation: &alondra_provider_contract::OrderedObservation) {
    if let Some(message) = alondra_provider_codex::native_message(observation) {
        let before=domain.live.mods.session(&agent.0).and_then(|session|session.codex);
        let now=alondra_core::now_millis();domain.live.mods.apply_codex(&agent.0,&message,now);
        let task=domain.read().state().tasks.values().find(|task|task.agent_id.as_ref()==Some(agent)).map(|task|task.id.clone());
        if let (Some(task),Some(session))=(task,domain.live.mods.session(&agent.0)){update_context(&domain.contexts,&task,before.as_ref(),&session,&message,now);}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    fn send(session: &mut Reported, method: &str, params: Value, now: Millis) { observe(session, &json!({"method":method,"params":params}), now); }
    #[test]
    fn empty_native_final_replaces_deltas_and_foreign_threads_do_not_change_the_root_answer() {
        let mut session = Reported::default();
        send(&mut session, "alondra/threadReady", json!({"threadId":"root"}), 1);
        send(&mut session, "alondra/inputAccepted", json!({"threadId":"root","turnId":"turn"}), 2);
        send(&mut session, "item/agentMessage/delta", json!({"threadId":"root","itemId":"answer","delta":"draft"}), 3);
        assert_eq!(session.answer.as_ref().unwrap().text, "draft");
        send(&mut session, "item/completed", json!({"threadId":"root","item":{"id":"answer","type":"agentMessage","text":""}}), 4);
        send(&mut session, "item/completed", json!({"threadId":"child","item":{"id":"answer","type":"agentMessage","text":"child only"}}), 5);
        send(&mut session, "turn/completed", json!({"threadId":"root","turn":{"id":"turn","status":"completed"}}), 6);
        assert_eq!(session.answer.unwrap().text, "");
        assert!(session.working_since.is_none());
    }
    #[test]
    fn current_native_model_changes_and_reroutes_invalidate_previous_model_context(){
        let mut session=Reported::default();let contexts=crate::idle_compaction::Contexts::default();let task="native-live".into();
        let mut apply=|method:&str,params:Value,now|{let before=session.codex.clone();let message=json!({"method":method,"params":params});observe(&mut session,&message,now);update_context(&contexts,&task,before.as_ref(),&session,&message,now);};
        apply("alondra/threadReady",json!({"threadId":"root","model":"first"}),1);
        apply("alondra/inputAccepted",json!({"threadId":"root","turnId":"one","model":"first"}),2);
        apply("thread/tokenUsage/updated",json!({"threadId":"root","turnId":"one","tokenUsage":{"last":{"totalTokens":100000},"modelContextWindow":200000}}),3);
        assert_eq!(contexts.used(&task).unwrap().window,Some(200000));
        apply("alondra/inputAccepted",json!({"threadId":"root","turnId":"two","model":"second"}),4);assert!(contexts.used(&task).is_none());
        apply("thread/tokenUsage/updated",json!({"threadId":"root","turnId":"one","tokenUsage":{"last":{"totalTokens":199999},"modelContextWindow":200000}}),5);assert!(contexts.used(&task).is_none());
        apply("thread/tokenUsage/updated",json!({"threadId":"child","turnId":"two","tokenUsage":{"last":{"totalTokens":199999},"modelContextWindow":200000}}),6);assert!(contexts.used(&task).is_none());
        apply("thread/tokenUsage/updated",json!({"threadId":"root","turnId":"two","tokenUsage":{"last":{"totalTokens":500}}}),7);assert_eq!(contexts.used(&task).unwrap().window,None);
        apply("model/rerouted",json!({"threadId":"root","turnId":"two","fromModel":"second","toModel":"third","reason":"highRiskCyberActivity"}),8);assert!(contexts.used(&task).is_none());
        apply("model/rerouted",json!({"threadId":"root","turnId":"one","fromModel":"third","toModel":"stale","reason":"highRiskCyberActivity"}),9);
        apply("thread/tokenUsage/updated",json!({"threadId":"root","turnId":"two","tokenUsage":{"last":{"totalTokens":500},"modelContextWindow":64000}}),10);
        assert_eq!(contexts.used(&task).unwrap().window,Some(64000));assert!(contexts.readings().is_empty(),"Codex has no proven cache expiry");
        assert_eq!(session.model.as_deref(),Some("third"));
    }
    #[test]
    fn native_reroute_before_request_receipt_is_preserved_and_adoption_scope_is_root_only(){
        let mut session=Reported::default();
        send(&mut session,"thread/tokenUsage/updated",json!({"threadId":"child","turnId":"foreign"}),1);assert!(session.codex.as_ref().unwrap().thread.is_none());
        send(&mut session,"alondra/inputAccepted",json!({"threadId":"root","turnId":"previous","model":"first"}),2);
        send(&mut session,"turn/started",json!({"threadId":"root","turn":{"id":"next"}}),3);
        send(&mut session,"model/rerouted",json!({"threadId":"root","turnId":"next","fromModel":"first","toModel":"actual","reason":"highRiskCyberActivity"}),4);
        send(&mut session,"alondra/inputAccepted",json!({"threadId":"root","turnId":"next","model":"first"}),5);
        assert_eq!(session.model.as_deref(),Some("actual"));assert_eq!(session.codex.as_ref().unwrap().model_turn.as_deref(),Some("next"));
    }

    #[test]
    fn adoption_restores_compacted_root_scope_before_next_native_frame_without_waiting_for_new_input(){
        let mut session=Reported::default();restore_scope(&mut session,alondra_provider_codex::LiveScope{thread:"root".into(),turn:Some("active".into()),model:Some("actual".into()),model_turn:Some("active".into()),rerouted_turn:Some("active".into()),cwd:Some("/workspace".into())},1);
        send(&mut session,"item/agentMessage/delta",json!({"threadId":"child","turnId":"active","itemId":"private-child","delta":"must not enter parent"}),2);assert!(session.answer.is_none());
        send(&mut session,"item/agentMessage/delta",json!({"threadId":"root","turnId":"active","itemId":"public","delta":"Continued public answer"}),3);assert_eq!(session.answer.as_ref().unwrap().text,"Continued public answer");assert_eq!(session.model.as_deref(),Some("actual"));assert_eq!(session.cwd.as_deref(),Some("/workspace"));
        let contexts=crate::idle_compaction::Contexts::default();let task="adopted".into();let message=json!({"method":"thread/tokenUsage/updated","params":{"threadId":"root","turnId":"active","tokenUsage":{"last":{"totalTokens":31},"modelContextWindow":32768}}});update_context(&contexts,&task,None,&session,&message,4);
        assert!(contexts.used(&task).is_some(),"new current-turn usage is scoped immediately after adoption");
    }

}
