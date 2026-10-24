//! What a node's Agent SDK host observes, read as the live state a node mod reports for a
//! `claude --bg` session: the call it runs, what it said last, what it cost, what it runs in the
//! background, its subagents, workflows and task list. The SDK's messages say all of it (the
//! `SDKMessage` types of `@anthropic-ai/claude-agent-sdk`); nothing here makes a node connected,
//! starts its turn or moves its state, which the domain alone decides for a node of the SDK.
use crate::node_reports::{Report, Reported, short_label};
use crate::node_work::StepIn;
use alondra_core::activity::Activity;
use alondra_core::metrics::steps::StepCounts;
use alondra_core::views::RunOutcome;
use alondra_core::{BackgroundItem, BackgroundKind, Millis, ToolInFlight};
use serde_json::Value;

const ANSWER_LIMIT: usize = 8_000;
const SUMMARY_LIMIT: usize = 300;
const REMEMBERED: usize = 50;
const PROGRESS_TOOLS: [&str; 4] = ["TaskCreate", "TaskUpdate", "TaskList", "Workflow"];

#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct Readings {
    /// The main thread's calls whose result says what their input does not, by `tool_use_id`.
    calls: Vec<(String, String, Value)>,
    /// The subagent run each `Agent` call started, by the call's `tool_use_id`.
    runs: Vec<(String, String)>,
    /// The call each subagent runs now, by its run.
    tools: Vec<(String, ToolInFlight)>,
    /// The runs `background_tasks_changed` listed last.
    listed: Vec<String>,
    said: Option<String>,
    /// Between the main thread's first message of a turn and the turn's result.
    in_turn: bool,
    pub cost_cents: Option<u64>,
    /// Since when the main thread does what it does now.
    since: Option<Millis>,
    /// The model calls of the turn, its subagents' too, until the turn's result counts them.
    steps: alondra_core::metrics::steps::StepMeter,
}

impl Readings {
    fn run_of(&self, call: &str) -> Option<String> {
        self.runs.iter().find(|(known, _)| known == call).map(|(_, run)| run.clone())
    }
}

fn text(value: &Value) -> Option<String> {
    value.as_str().filter(|text| !text.is_empty()).map(|text| text.chars().take(SUMMARY_LIMIT).collect())
}

fn remember<T>(items: &mut Vec<T>, item: T) {
    items.push(item);
    if items.len() > REMEMBERED {
        items.remove(0);
    }
}

/// The steps of the turn a `result` ends, counted, and nothing for any other message.
pub(crate) fn observe(session: &mut Reported, message: &Value, now: Millis) -> Option<StepCounts> {
    let mut readings = session.sdk.take().unwrap_or_default();
    let mut counted = None;
    let parent = message.get("parent_tool_use_id").and_then(Value::as_str).map(str::to_owned);
    match (message["type"].as_str(), message["subtype"].as_str()) {
        (Some("system"), Some("init")) => {
            if let Some(cwd) = text(&message["cwd"]) {
                session.cwd = Some(cwd);
            }
            if let Some(model) = text(&message["model"]) {
                session.record(Report::Model { model, agent_id: None }, now);
            }
        }
        (Some("system"), Some("task_started")) => started(session, &mut readings, message, now),
        (Some("system"), Some("task_notification")) => {
            if let Some(id) = message["task_id"].as_str() {
                let outcome = outcome(message["status"].as_str()).unwrap_or(RunOutcome::Finished);
                finished(session, &mut readings, id, outcome, now);
            }
        }
        (Some("system"), Some("task_updated")) => {
            let outcome = outcome(message.pointer("/patch/status").and_then(Value::as_str));
            if let (Some(id), Some(outcome)) = (message["task_id"].as_str(), outcome) {
                finished(session, &mut readings, id, outcome, now);
            }
        }
        (Some("system"), Some("background_tasks_changed")) => listed(session, &mut readings, message["tasks"].as_array().map_or(&[][..], Vec::as_slice), now),
        (Some("assistant"), _) => {
            readings.in_turn |= parent.is_none();
            if let Some(id) = crate::metrics_capture::claude_step(message) {
                readings.steps.saw(id, parent.is_some(), crate::metrics_capture::claude_tools(message));
            }
            said(session, &mut readings, message, parent, now);
        }
        (Some("user"), _) => answered(session, &mut readings, message, parent, now),
        (Some("result"), _) => {
            let answer = message["result"].as_str().filter(|answer| !answer.trim().is_empty()).map(str::to_owned)
                .or_else(|| readings.said.take())
                .unwrap_or_default();
            let aborted = message["terminal_reason"].as_str().is_some_and(|reason| reason.starts_with("aborted"));
            session.record(Report::TurnComplete { answer: answer.chars().take(ANSWER_LIMIT).collect(), aborted }, now);
            readings.said = None;
            readings.in_turn = false;
            readings.since = Some(now);
            if let Some(cost) = message["total_cost_usd"].as_f64().filter(|cost| cost.is_finite() && *cost >= 0.0) {
                readings.cost_cents = Some((cost * 100.0).round() as u64);
            }
            counted = Some(readings.steps.take());
        }
        _ => {}
    }
    session.sdk = Some(readings);
    counted
}

/// How a task ended, by the status the SDK gives it once it is over.
fn outcome(status: Option<&str>) -> Option<RunOutcome> {
    match status? {
        "completed" => Some(RunOutcome::Finished),
        "failed" => Some(RunOutcome::Failed),
        "stopped" | "killed" => Some(RunOutcome::Stopped),
        _ => None,
    }
}

/// The host ended, and every call and background task its Claude ran ended with it.
pub(crate) fn ended(session: &mut Reported, now: Millis) {
    session.tool = None;
    session.end_background(|_| true, RunOutcome::Stopped, now);
    session.work.ended(now);
    if let Some(readings) = session.sdk.as_mut() {
        readings.tools.clear();
        readings.calls.clear();
        readings.listed.clear();
        readings.in_turn = false;
        readings.since = Some(now);
    }
}

/// What each thread of a node of the SDK does now: its main thread while its turn or a call runs,
/// then each subagent running.
pub(crate) fn activity(session: &Reported) -> Option<Vec<Activity>> {
    let readings = session.sdk.as_ref()?;
    let main = (readings.in_turn || session.tool.is_some()).then(|| Activity {
        agent_id: None,
        agent_type: None,
        task: None,
        tool: session.tool.as_ref().map(|tool| tool.name.clone()),
        summary: session.tool.as_ref().and_then(|tool| tool.summary.clone()),
        since: session.tool.as_ref().map(|tool| tool.since).or(readings.since).or(session.last_event_at).unwrap_or_default(),
    });
    let subagents = session.work.agents.iter().filter(|agent| agent.end.is_none()).map(|agent| {
        let tool = readings.tools.iter().find(|(run, _)| *run == agent.id).map(|(_, tool)| tool);
        Activity {
            agent_id: Some(agent.id.clone()),
            agent_type: agent.kind.clone(),
            task: None,
            tool: tool.map(|tool| tool.name.clone()),
            summary: tool.and_then(|tool| tool.summary.clone()),
            since: tool.map_or(agent.started_at, |tool| tool.since),
        }
    });
    Some(main.into_iter().chain(subagents).collect())
}

fn started(session: &mut Reported, readings: &mut Readings, message: &Value, now: Millis) {
    let Some(id) = message["task_id"].as_str().map(str::to_owned) else { return };
    if message["ambient"] == true || message["skip_transcript"] == true {
        return;
    }
    let description = text(&message["description"]);
    match message["task_type"].as_str() {
        Some("local_agent") => {
            if let Some(call) = message["tool_use_id"].as_str() {
                remember(&mut readings.runs, (call.to_owned(), id.clone()));
            }
            session.record(Report::SubagentStart { id, description, kind: text(&message["subagent_type"]), model: None }, now);
        }
        Some("local_workflow") => {
            let name = text(&message["workflow_name"]).or(description).unwrap_or_else(|| "workflow".into());
            session.record(Report::WorkflowStart { id, name, transcript_dir: None }, now);
        }
        _ => {}
    }
}

fn finished(session: &mut Reported, readings: &mut Readings, id: &str, outcome: RunOutcome, now: Millis) {
    session.work.agent_finished(id, outcome, now);
    session.work.workflow_finished(id, outcome, now);
    session.end_background(|known| known == id, outcome, now);
    readings.tools.retain(|(run, _)| run != id);
    readings.listed.retain(|known| known != id);
}

/// Every live background task, as the SDK lists it after each change: what is no longer in it
/// ended, and a subagent or workflow running in the foreground stays until it ends.
fn listed(session: &mut Reported, readings: &mut Readings, tasks: &[Value], now: Millis) {
    let tasks: Vec<&Value> = tasks.iter().filter(|task| task["ambient"] != true && task["task_id"].is_string()).collect();
    let ids: Vec<String> = tasks.iter().filter_map(|task| task["task_id"].as_str().map(str::to_owned)).collect();
    for gone in std::mem::take(&mut readings.listed).into_iter().filter(|id| !ids.contains(id)) {
        finished(session, readings, &gone, RunOutcome::Finished, now);
    }
    let left: Vec<String> = session.background.iter().map(|(id, _)| id).filter(|id| !session.work.runs(id) && !ids.contains(id)).cloned().collect();
    session.end_background(|id| left.iter().any(|gone| gone == id), RunOutcome::Finished, now);
    let known = std::mem::take(&mut session.background);
    let started_at = |id: &str| known.iter().find(|(known, _)| known == id).map(|(_, item)| item.started_at);
    let running = |id: &str| session.work.runs(id);
    let mut background: Vec<(String, BackgroundItem)> = known.iter().filter(|(id, _)| running(id) && !ids.contains(id)).cloned().collect();
    for task in &tasks {
        let id = task["task_id"].as_str().unwrap_or_default();
        let description = task["description"].as_str().unwrap_or_default();
        let kind = match task["task_type"].as_str() {
            Some("local_bash") => BackgroundKind::Shell,
            Some("local_agent" | "remote_agent") => BackgroundKind::Agent,
            _ => BackgroundKind::Other,
        };
        let label = session.work.workflows.iter().find(|workflow| workflow.id == id).map_or(description, |workflow| workflow.name.as_str());
        background.push((id.to_owned(), BackgroundItem { kind, label: short_label(label), started_at: started_at(id).unwrap_or(now) }));
    }
    session.background = background;
    for task in &tasks {
        let id = task["task_id"].as_str().unwrap_or_default();
        match task["task_type"].as_str() {
            Some("local_agent") => session.work.agent_started(id.to_owned(), text(&task["description"]), text(&task["subagent_type"]), None, now),
            Some("local_workflow") if !session.work.workflows.iter().any(|workflow| workflow.id == id) => {
                session.work.workflow_started(id.to_owned(), text(&task["description"]).unwrap_or_else(|| "workflow".into()), None, now);
            }
            _ => {}
        }
    }
    session.background_known = true;
    readings.listed = ids;
}

fn said(session: &mut Reported, readings: &mut Readings, message: &Value, parent: Option<String>, now: Millis) {
    let run = parent.as_deref().map(|call| readings.run_of(call).unwrap_or_else(|| call.to_owned()));
    if let Some(model) = text(&message["message"]["model"]).filter(|model| !model.starts_with('<')) {
        session.record(Report::Model { model, agent_id: run.clone() }, now);
    }
    let blocks = message["message"]["content"].as_array().map_or(&[][..], Vec::as_slice);
    let words: Vec<&str> = blocks.iter().filter(|block| block["type"] == "text").filter_map(|block| block["text"].as_str())
        .filter(|words| !words.trim().is_empty()).collect();
    if run.is_none() && !words.is_empty() {
        readings.said = Some(words.join("\n\n"));
    }
    for block in blocks.iter().filter(|block| block["type"] == "tool_use") {
        let (Some(name), Some(call)) = (block["name"].as_str(), block["id"].as_str()) else { continue };
        let input = &block["input"];
        match &run {
            Some(run) => {
                let tool = ToolInFlight { tool_use_id: Some(call.to_owned()), name: name.to_owned(), summary: crate::hook_text::tool_summary(Some(name), Some(input)), since: now };
                readings.tools.retain(|(known, _)| known != run);
                readings.tools.push((run.clone(), tool));
            }
            None => {
                session.record(Report::ToolStart { tool: name.to_owned(), tool_use_id: Some(call.to_owned()), input: input.clone() }, now);
                readings.since = Some(now);
                if name == "TodoWrite" {
                    let todos = input["todos"].as_array().map_or(&[][..], Vec::as_slice);
                    let steps = todos.iter().enumerate().filter_map(|(index, todo)| step((index + 1).to_string(), todo)).collect();
                    session.record(Report::Steps { steps }, now);
                }
                if PROGRESS_TOOLS.contains(&name) {
                    remember(&mut readings.calls, (call.to_owned(), name.to_owned(), input.clone()));
                }
            }
        }
    }
}

fn answered(session: &mut Reported, readings: &mut Readings, message: &Value, parent: Option<String>, now: Millis) {
    let blocks = message["message"]["content"].as_array().map_or(&[][..], Vec::as_slice);
    for call in blocks.iter().filter(|block| block["type"] == "tool_result").filter_map(|block| block["tool_use_id"].as_str()) {
        match parent.as_deref() {
            Some(parent) => {
                let run = readings.run_of(parent).unwrap_or_else(|| parent.to_owned());
                readings.tools.retain(|(known, tool)| *known != run || tool.tool_use_id.as_deref() != Some(call));
            }
            None => {
                if session.tool.as_ref().is_some_and(|tool| tool.tool_use_id.as_deref() == Some(call)) {
                    readings.since = Some(now);
                }
                session.record(Report::ToolEnd { tool_use_id: Some(call.to_owned()) }, now);
                let Some(index) = readings.calls.iter().position(|(known, _, _)| known == call) else { continue };
                let (_, name, input) = readings.calls.remove(index);
                let failed = blocks.iter().any(|block| block["tool_use_id"] == call && block["is_error"] == true);
                if let Some(report) = progress(&name, &input, &message["tool_use_result"]).filter(|_| !failed) {
                    session.record(report, now);
                }
            }
        }
    }
}

fn step(id: String, fields: &Value) -> Option<StepIn> {
    Some(StepIn {
        id,
        subject: text(&fields["subject"]).or_else(|| text(&fields["content"])),
        status: serde_json::from_value(fields["status"].clone()).ok(),
        active_form: text(&fields["activeForm"]),
    })
}

fn id_of(value: &Value) -> Option<String> {
    match value {
        Value::String(id) if !id.is_empty() => Some(id.clone()),
        Value::Number(id) => Some(id.to_string()),
        _ => None,
    }
}

/// What the result of a task-list or workflow call adds to the node's plan, as the node mod
/// reads it (`plugin/hooks/register.ts` `progress`).
fn progress(name: &str, input: &Value, result: &Value) -> Option<Report> {
    match name {
        "TaskCreate" => step(id_of(&result["task"]["id"])?, input).map(Report::StepCreate),
        "TaskUpdate" if result["success"] != false => step(id_of(&input["taskId"])?, input).map(Report::StepUpdate),
        "TaskList" => {
            let steps = result["tasks"].as_array()?.iter().filter_map(|task| step(id_of(&task["id"])?, task)).collect();
            Some(Report::Steps { steps })
        }
        "Workflow" => {
            let id = text(&result["taskId"])?;
            let name = text(&result["workflowName"]).or_else(|| text(&input["name"])).unwrap_or_else(|| "workflow".into());
            Some(Report::WorkflowStart { id, name, transcript_dir: text(&result["transcriptDir"]).map(Into::into) })
        }
        _ => None,
    }
}

/// What one observation of a node's host says, kept with what its node mod would have reported.
pub(crate) fn feed(domain: &crate::domain_runtime::DomainRuntime, agent: &alondra_core::domain::AgentId, observation: &alondra_provider_contract::OrderedObservation) {
    if let Some(message) = alondra_provider_claude::sdk_message(observation) {
        let now = alondra_core::now_millis();
        if let Some(steps) = domain.live.mods.apply_sdk(&agent.0, &message, now) {
            crate::metrics_capture::node_turn(domain, agent, &message, steps, now);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node_reports::Reports;
    use crate::node_work::StepStatus;
    use serde_json::json;

    const MINUTE: Millis = 60_000;

    fn reports(messages: &[(Value, Millis)]) -> Reports {
        let reports = Reports::default();
        for (message, at) in messages {
            reports.apply_sdk("sdk-a", message, *at);
        }
        reports
    }

    fn assistant(parent: Option<&str>, content: Value) -> Value {
        json!({"type": "assistant", "parent_tool_use_id": parent, "session_id": "native",
            "message": {"role": "assistant", "model": "claude-opus-5-5", "content": content}})
    }

    fn result_of(parent: Option<&str>, call: &str, result: Value) -> Value {
        json!({"type": "user", "parent_tool_use_id": parent, "session_id": "native", "tool_use_result": result,
            "message": {"role": "user", "content": [{"type": "tool_result", "tool_use_id": call, "content": "ok"}]}})
    }

    fn task_started(id: &str, kind: &str, call: &str, description: &str) -> Value {
        json!({"type": "system", "subtype": "task_started", "task_id": id, "tool_use_id": call, "task_type": kind,
            "description": description, "subagent_type": "general-purpose", "session_id": "native"})
    }

    #[test]
    fn the_main_threads_call_and_each_subagents_call_are_what_the_node_does_now() {
        let reports = reports(&[
            (json!({"type": "system", "subtype": "init", "cwd": "/w/task", "model": "claude-opus-5-5", "session_id": "native"}), 1),
            (assistant(None, json!([{"type": "tool_use", "id": "toolu_bash", "name": "Bash", "input": {"command": "cargo test", "description": "Run the tests"}}])), 2),
            (assistant(None, json!([{"type": "tool_use", "id": "toolu_agent", "name": "Agent", "input": {"description": "Fix the parser", "prompt": "…"}}])), 3),
            (task_started("a1", "local_agent", "toolu_agent", "Fix the parser"), 4),
            (assistant(Some("toolu_agent"), json!([{"type": "tool_use", "id": "toolu_read", "name": "Read", "input": {"file_path": "/w/task/src/parser.rs"}}])), 5),
        ]);
        let session = reports.session("sdk-a").unwrap();
        assert_eq!(session.cwd.as_deref(), Some("/w/task"));
        assert_eq!(session.model.as_deref(), Some("claude-opus-5-5"));
        let tool = session.tool.as_ref().unwrap();
        assert_eq!((tool.name.as_str(), tool.tool_use_id.as_deref(), tool.since), ("Agent", Some("toolu_agent"), 3));
        let threads = activity(&session).unwrap();
        assert_eq!(threads.len(), 2, "{threads:?}");
        assert_eq!((threads[0].agent_id.as_deref(), threads[0].tool.as_deref()), (None, Some("Agent")));
        assert_eq!((threads[1].agent_id.as_deref(), threads[1].agent_type.as_deref(), threads[1].tool.as_deref(), threads[1].summary.as_deref(), threads[1].since),
            (Some("a1"), Some("general-purpose"), Some("Read"), Some("/w/task/src/parser.rs"), 5));

        reports.apply_sdk("sdk-a", &result_of(Some("toolu_agent"), "toolu_read", json!({})), 6);
        reports.apply_sdk("sdk-a", &result_of(None, "toolu_agent", json!({})), 7);
        let session = reports.session("sdk-a").unwrap();
        assert!(session.tool.is_none(), "its result ends the main thread's call");
        let threads = activity(&session).unwrap();
        assert_eq!((threads[0].tool.as_deref(), threads[0].since), (None, 7));
        assert_eq!(threads[1].tool, None, "the subagent's call ended too");
        assert!(activity(&Reported::default()).is_none(), "a session no SDK host reported is the hooks' to show");
    }

    #[test]
    fn a_turns_result_is_what_the_node_said_last_with_its_time_and_what_the_session_cost() {
        let reports = reports(&[
            (assistant(None, json!([{"type": "text", "text": "Reading the notes"}])), 1),
            (assistant(None, json!([{"type": "text", "text": "The notes are written."}])), 2),
            (json!({"type": "result", "subtype": "success", "result": "The notes are written.", "is_error": false,
                "total_cost_usd": 1.234, "session_id": "native"}), 3),
        ]);
        let session = reports.session("sdk-a").unwrap();
        let answer = session.answer.clone().unwrap();
        assert_eq!((answer.text.as_str(), answer.at, answer.aborted), ("The notes are written.", 3, false));
        assert_eq!(session.sdk.as_ref().unwrap().cost_cents, Some(123));
        assert_eq!(activity(&session), Some(vec![]), "a node whose turn ended does nothing right now");

        reports.apply_sdk("sdk-a", &assistant(None, json!([{"type": "text", "text": "Half way through"}])), 4);
        reports.apply_sdk("sdk-a", &json!({"type": "result", "subtype": "error_during_execution", "is_error": true,
            "terminal_reason": "aborted_tools", "total_cost_usd": 2.5, "session_id": "native"}), 5);
        let session = reports.session("sdk-a").unwrap();
        let answer = session.answer.unwrap();
        assert_eq!((answer.text.as_str(), answer.at, answer.aborted), ("Half way through", 5, true), "an interrupted turn says what it said last");
        assert_eq!(session.sdk.unwrap().cost_cents, Some(250), "the cost is the session's running total, never a sum");
    }

    #[test]
    fn the_background_list_becomes_the_nodes_shells_and_subagents_and_each_ends_only_by_a_fact() {
        let listed = |tasks: Value| json!({"type": "system", "subtype": "background_tasks_changed", "tasks": tasks, "session_id": "native"});
        let reports = reports(&[
            (task_started("a1", "local_agent", "toolu_agent", "Fix the parser"), 1),
            (listed(json!([
                {"task_id": "b1", "task_type": "local_bash", "description": "sleep 600"},
                {"task_id": "b2", "task_type": "local_bash", "description": "until pgrep app"},
                {"task_id": "a1", "task_type": "local_agent", "description": "Fix the parser", "subagent_type": "general-purpose"},
                {"task_id": "m1", "task_type": "monitor", "description": "watch the build", "ambient": true},
            ])), 2),
        ]);
        let kinds = |reports: &Reports| reports.session("sdk-a").unwrap().background.iter()
            .map(|(id, item)| (id.clone(), item.kind, item.label.clone())).collect::<Vec<_>>();
        assert_eq!(kinds(&reports), [("b1".into(), BackgroundKind::Shell, "sleep 600".into()), ("b2".into(), BackgroundKind::Shell, "until pgrep app".into()),
            ("a1".into(), BackgroundKind::Agent, "Fix the parser".into())], "what is not activity stays out");
        let ended = |reports: &Reports| reports.session("sdk-a").unwrap().work.ended_tasks.iter()
            .map(|task| (task.item.label.clone(), task.end.outcome, task.end.at)).collect::<Vec<_>>();
        assert_eq!(ended(&reports), [], "nothing has ended: no fact says so");

        reports.apply_sdk("sdk-a", &json!({"type": "system", "subtype": "task_notification", "task_id": "b2", "status": "failed", "session_id": "native"}), 3);
        reports.apply_sdk("sdk-a", &listed(json!([{"task_id": "a1", "task_type": "local_agent", "description": "Fix the parser"}])), 4);
        assert_eq!(kinds(&reports).len(), 1, "the shells that ended or left the list no longer run");
        assert_eq!(ended(&reports), [("until pgrep app".into(), RunOutcome::Failed, 3), ("sleep 600".into(), RunOutcome::Finished, 4)],
            "each ended as its fact says");
        reports.apply_sdk("sdk-a", &listed(json!([])), 5);
        let session = reports.session("sdk-a").unwrap();
        assert!(session.background.is_empty());
        assert_eq!(session.work.agents[0].end, Some(crate::node_work::End { at: 5, outcome: RunOutcome::Finished }), "a subagent the list dropped ended");
        assert_eq!(ended(&reports).len(), 2, "a subagent ends in its own run, never as a shell");
    }

    #[test]
    fn a_stopped_task_and_one_its_ended_host_left_running_end_stopped() {
        let reports = reports(&[
            (task_started("a1", "local_agent", "toolu_agent", "Fix the parser"), 1),
            (json!({"type": "system", "subtype": "background_tasks_changed", "tasks": [{"task_id": "b1", "task_type": "local_bash", "description": "npm run dev"}], "session_id": "native"}), 2),
            (json!({"type": "system", "subtype": "task_updated", "task_id": "a1", "patch": {"status": "killed"}, "session_id": "native"}), 3),
        ]);
        assert_eq!(reports.session("sdk-a").unwrap().work.agents[0].end, Some(crate::node_work::End { at: 3, outcome: RunOutcome::Stopped }));
        reports.sdk_ended("sdk-a", 4);
        let session = reports.session("sdk-a").unwrap();
        assert_eq!(session.work.ended_tasks.iter().map(|task| (task.item.label.as_str(), task.end.outcome)).collect::<Vec<_>>(), [("npm run dev", RunOutcome::Stopped)]);
    }

    #[test]
    fn a_foreground_subagent_stays_until_its_notification_though_the_background_list_never_holds_it() {
        let reports = reports(&[
            (task_started("a1", "local_agent", "toolu_agent", "Fix the parser"), 1),
            (json!({"type": "system", "subtype": "background_tasks_changed", "tasks": [], "session_id": "native"}), 2),
        ]);
        let session = reports.session("sdk-a").unwrap();
        assert_eq!(session.work.agents[0].end.map(|end| end.at), None);
        assert_eq!(session.background.len(), 1);
        reports.apply_sdk("sdk-a", &json!({"type": "system", "subtype": "task_notification", "task_id": "a1", "status": "completed",
            "output_file": "", "summary": "Fixed", "session_id": "native"}), 3);
        let session = reports.session("sdk-a").unwrap();
        assert_eq!(session.work.agents[0].end.map(|end| end.at), Some(3));
        assert!(session.background.is_empty());
        assert_eq!(activity(&session), Some(vec![]), "nothing runs any more");
    }

    #[test]
    fn todo_writes_task_tools_and_workflows_become_the_nodes_steps_and_runs() {
        let reports = reports(&[
            (assistant(None, json!([{"type": "tool_use", "id": "toolu_todo", "name": "TodoWrite", "input": {"todos": [
                {"content": "Read the notes", "status": "completed", "activeForm": "Reading the notes"},
                {"content": "Write the changelog", "status": "in_progress", "activeForm": "Writing the changelog"},
            ]}}])), 1),
        ]);
        let steps = |reports: &Reports| reports.session("sdk-a").unwrap().work.steps.iter()
            .map(|step| (step.subject.clone(), step.status, step.active_form.clone())).collect::<Vec<_>>();
        assert_eq!(steps(&reports), [
            ("Read the notes".into(), StepStatus::Completed, Some("Reading the notes".into())),
            ("Write the changelog".into(), StepStatus::InProgress, Some("Writing the changelog".into())),
        ]);

        reports.apply_sdk("sdk-a", &assistant(None, json!([{"type": "tool_use", "id": "toolu_create", "name": "TaskCreate",
            "input": {"subject": "Tag the release", "description": "…", "activeForm": "Tagging the release"}}])), 2);
        reports.apply_sdk("sdk-a", &result_of(None, "toolu_create", json!({"task": {"id": "7", "subject": "Tag the release"}})), 3);
        assert_eq!(steps(&reports)[2], ("Tag the release".into(), StepStatus::Pending, Some("Tagging the release".into())), "the id comes with the result");

        reports.apply_sdk("sdk-a", &assistant(None, json!([{"type": "tool_use", "id": "toolu_flow", "name": "Workflow", "input": {"name": "review-loop"}}])), 4);
        reports.apply_sdk("sdk-a", &task_started("w1", "local_workflow", "toolu_flow", "Review the branch"), 5);
        reports.apply_sdk("sdk-a", &result_of(None, "toolu_flow", json!({"taskId": "w1", "workflowName": "review-loop", "transcriptDir": "/w/flows/w1"})), 6);
        let session = reports.session("sdk-a").unwrap();
        let workflow = &session.work.workflows[0];
        assert_eq!((workflow.name.as_str(), workflow.transcript_dir.as_deref(), workflow.started_at), ("review-loop", Some(std::path::Path::new("/w/flows/w1")), 5), "the call's result names it and says where it writes");
        reports.apply_sdk("sdk-a", &json!({"type": "system", "subtype": "task_notification", "task_id": "w1", "status": "completed",
            "output_file": "", "summary": "Done", "session_id": "native"}), 7);
        assert_eq!(reports.session("sdk-a").unwrap().work.workflows[0].end.map(|end| end.at), Some(7));

        reports.apply_sdk("sdk-a", &assistant(Some("toolu_agent"), json!([{"type": "tool_use", "id": "toolu_sub", "name": "TodoWrite", "input": {"todos": []}}])), 8);
        assert_eq!(steps(&reports).len(), 3, "a subagent's own list is not the node's plan");
    }

    #[test]
    fn a_node_of_the_sdk_is_never_connected_stalled_or_lost_whatever_its_host_reports() {
        let reports = reports(&[
            (json!({"type": "system", "subtype": "init", "cwd": "/w/task", "model": "claude-opus-5-5", "session_id": "native"}), 0),
            (assistant(None, json!([{"type": "tool_use", "id": "toolu_build", "name": "Bash", "input": {"command": "make build"}}])), 1),
        ]);
        let session = reports.session("sdk-a").unwrap();
        let hours = 5 * 60 * MINUTE;
        assert!(!reports.connected("sdk-a"));
        assert!(!reports.is_node("sdk-a"), "only a mod that loaded makes a session a node of Claude Code");
        assert!(!session.connection_lost(), "nothing brings an SDK node back as a claude --bg one");
        assert_eq!(session.halt(), None);
        assert_eq!(session.activity(), crate::node_reports::Activity::Working, "a call the SDK has not ended is still running");
        assert_eq!(session.working_since, None, "its turn is the domain's to start");
        reports.sdk_ended("sdk-a", hours);
        let session = reports.session("sdk-a").unwrap();
        assert!(session.tool.is_none() && session.background.is_empty());
        assert!(!reports.connected("sdk-a"));
    }
}
