//! What a node on Antigravity is doing, from the messages of its host: `agy acp`'s `session/update`
//! notifications as agy sent them, and the host's own `alondra/*` notices of the session and of
//! each prompt. The answer, the calls under way and the plan go to the live view; nothing here
//! moves the node's state.
use crate::antigravity_readings::Readings;
use crate::node_reports::{Answer, Report, Reported, short_label};
use alondra_core::activity::Activity;
use alondra_core::{Millis, ToolInFlight};
use serde_json::Value;

/// The most calls kept under way at once, and the longest answer kept.
const CALLS: usize = 64;
const ANSWER: usize = 8_000;

fn text<'a>(value: &'a Value, key: &str) -> Option<&'a str> { value.get(key).and_then(Value::as_str) }

pub fn observe(session: &mut Reported, message: &Value, now: Millis) {
    let mut readings = session.antigravity.take().unwrap_or_default();
    let params = message.get("params").unwrap_or(&Value::Null);
    let method = text(message, "method").unwrap_or("");
    if method == "alondra/sessionReady" {
        readings.session = text(params, "sessionId").map(str::to_owned);
        session.cwd = text(params, "cwd").map(str::to_owned);
        if let Some(model) = text(params, "model").filter(|model| !model.is_empty()) {
            session.record(Report::Model { model: model.into(), agent_id: None }, now);
        }
    } else if readings.session.is_some() && text(params, "sessionId") == readings.session.as_deref() {
        match method {
            "alondra/promptSent" => {
                readings.turn = text(params, "turnId").map(str::to_owned);
                readings.answer.clear();
                readings.since = now;
                session.record(Report::TurnStart, now);
            }
            "session/update" => update(session, &mut readings, params.get("update").unwrap_or(&Value::Null), now),
            "alondra/promptEnded" | "alondra/promptFailed" if readings.turn.is_some() && text(params, "turnId") == readings.turn.as_deref() => {
                let aborted = method == "alondra/promptFailed" || text(params, "stopReason") == Some("cancelled");
                session.record(Report::TurnComplete { answer: readings.answer.clone(), aborted }, now);
                readings.calls.clear();
                session.tool = None;
                readings.turn = None;
            }
            _ => {}
        }
    }
    session.antigravity = Some(readings);
}

fn update(session: &mut Reported, readings: &mut Readings, update: &Value, now: Millis) {
    match text(update, "sessionUpdate") {
        Some("agent_message_chunk") => {
            let Some(chunk) = update.pointer("/content/text").and_then(Value::as_str) else { return };
            if readings.answer.chars().count() < ANSWER {
                readings.answer.push_str(chunk);
                readings.answer = readings.answer.chars().take(ANSWER).collect();
            }
            session.answer = Some(Answer { text: readings.answer.clone(), at: now, aborted: false });
        }
        Some("tool_call") => {
            let Some(id) = text(update, "toolCallId").filter(|id| !id.is_empty()) else { return };
            if readings.calls.len() < CALLS {
                let name = text(update, "name").or_else(|| text(update, "kind")).unwrap_or("tool").to_owned();
                readings.calls.insert(id.into(), ToolInFlight { tool_use_id: Some(id.into()), name, summary: text(update, "title").map(short_label), since: now });
            }
            session.tool = readings.calls.values().next().cloned();
        }
        Some("tool_call_update") if matches!(text(update, "status"), Some("completed" | "failed")) => {
            let Some(id) = text(update, "toolCallId") else { return };
            readings.calls.remove(id);
            session.tool = readings.calls.values().next().cloned();
        }
        Some("plan") => {
            let steps = update.get("entries").and_then(Value::as_array).into_iter().flatten().enumerate().take(50).map(|(index, entry)| crate::node_work::StepIn {
                id: format!("antigravity-plan-{index}"), subject: text(entry, "content").map(str::to_owned),
                status: match text(entry, "status") {
                    Some("completed") => Some(crate::node_work::StepStatus::Completed),
                    Some("in_progress") => Some(crate::node_work::StepStatus::InProgress),
                    Some("pending") => Some(crate::node_work::StepStatus::Pending),
                    _ => None,
                },
                active_form: None,
            }).collect();
            session.record(Report::Steps { steps }, now);
        }
        _ => {}
    }
}

pub fn activity(session: &Reported) -> Option<Vec<Activity>> {
    let readings = session.antigravity.as_ref()?;
    let main = (readings.turn.is_some() || session.tool.is_some()).then(|| Activity {
        agent_id: None, agent_type: None, task: None,
        tool: session.tool.as_ref().map(|tool| tool.name.clone()), summary: session.tool.as_ref().and_then(|tool| tool.summary.clone()),
        since: session.tool.as_ref().map_or(readings.since, |tool| tool.since),
    });
    Some(main.into_iter().collect())
}

pub fn feed(reports: &crate::node_reports::Reports, agent: &alondra_domain_rules::AgentId, message: &Value) {
    reports.apply_with(&agent.0, alondra_core::now_millis(), |session, now| observe(session, message, now));
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn send(session: &mut Reported, method: &str, params: Value, now: Millis) { observe(session, &json!({"method": method, "params": params}), now); }
    fn chunk(text: &str) -> Value { json!({"sessionId": "s", "update": {"sessionUpdate": "agent_message_chunk", "content": {"type": "text", "text": text}}}) }

    #[test]
    fn a_turn_shows_its_answer_and_calls_until_its_prompt_ends() {
        let mut session = Reported::default();
        send(&mut session, "alondra/sessionReady", json!({"sessionId": "s", "cwd": "/Users/ana/work", "model": "gemini-4-neon"}), 1);
        assert_eq!((session.cwd.as_deref(), session.model.as_deref()), (Some("/Users/ana/work"), Some("gemini-4-neon")));
        send(&mut session, "alondra/promptSent", json!({"sessionId": "s", "inputId": "i", "turnId": "t1"}), 2);
        assert!(session.working_since.is_some());
        send(&mut session, "session/update", json!({"sessionId": "s", "update": {"sessionUpdate": "tool_call", "toolCallId": "c1", "name": "run_command", "title": "ls -la", "kind": "execute"}}), 3);
        assert_eq!(session.tool.as_ref().map(|tool| (tool.name.as_str(), tool.summary.as_deref())), Some(("run_command", Some("ls -la"))));
        assert_eq!(activity(&session).unwrap()[0].tool.as_deref(), Some("run_command"));
        send(&mut session, "session/update", json!({"sessionId": "s", "update": {"sessionUpdate": "tool_call_update", "toolCallId": "c1", "status": "completed"}}), 4);
        assert!(session.tool.is_none());
        send(&mut session, "session/update", chunk("Added "), 5);
        send(&mut session, "session/update", chunk("the export."), 6);
        assert_eq!(session.answer.as_ref().unwrap().text, "Added the export.");
        send(&mut session, "alondra/promptEnded", json!({"sessionId": "s", "turnId": "t1", "stopReason": "end_turn"}), 7);
        assert!(session.working_since.is_none());
        assert!(activity(&session).unwrap().is_empty());
    }

    #[test]
    fn another_session_and_a_turn_never_started_change_nothing() {
        let mut session = Reported::default();
        send(&mut session, "session/update", chunk("before the session"), 1);
        assert!(session.answer.is_none(), "nothing counts before the session opened");
        send(&mut session, "alondra/sessionReady", json!({"sessionId": "s"}), 2);
        send(&mut session, "session/update", json!({"sessionId": "other", "update": {"sessionUpdate": "agent_message_chunk", "content": {"type": "text", "text": "other"}}}), 3);
        assert!(session.answer.is_none());
        send(&mut session, "alondra/promptEnded", json!({"sessionId": "s", "turnId": "never", "stopReason": "end_turn"}), 4);
        assert!(session.working_since.is_none());
    }

    #[test]
    fn a_cancelled_or_failed_prompt_ends_its_turn_and_an_ended_host_leaves_no_call_running() {
        let mut session = Reported::default();
        send(&mut session, "alondra/sessionReady", json!({"sessionId": "s"}), 1);
        send(&mut session, "alondra/promptSent", json!({"sessionId": "s", "turnId": "t1"}), 2);
        send(&mut session, "alondra/promptEnded", json!({"sessionId": "s", "turnId": "t1", "stopReason": "cancelled"}), 3);
        assert!(session.working_since.is_none());
        send(&mut session, "alondra/promptSent", json!({"sessionId": "s", "turnId": "t2"}), 4);
        send(&mut session, "alondra/promptFailed", json!({"sessionId": "s", "turnId": "t2", "error": {"code": -32603}}), 5);
        assert!(session.working_since.is_none());
        send(&mut session, "alondra/promptSent", json!({"sessionId": "s", "turnId": "t3"}), 6);
        send(&mut session, "session/update", json!({"sessionId": "s", "update": {"sessionUpdate": "tool_call", "toolCallId": "c1", "kind": "execute"}}), 7);
        let reports = crate::node_reports::Reports::default();
        reports.apply_with("n", 7, |reported, _| *reported = session);
        reports.sdk_ended("n", 8);
        let session = reports.session("n").unwrap();
        assert!(session.tool.is_none() && session.working_since.is_none());
        assert!(activity(&session).unwrap().is_empty());
    }

    #[test]
    fn the_plan_agy_sends_becomes_the_task_list() {
        let mut session = Reported::default();
        send(&mut session, "alondra/sessionReady", json!({"sessionId": "s"}), 1);
        send(&mut session, "session/update", json!({"sessionId": "s", "update": {"sessionUpdate": "plan", "entries": [
            {"content": "Read the report", "status": "completed"}, {"content": "Write the CSV", "status": "in_progress"}]}}), 2);
        let subjects: Vec<_> = session.work.steps.iter().map(|step| step.subject.clone()).collect();
        assert_eq!(subjects, ["Read the report", "Write the CSV"]);
    }
}
