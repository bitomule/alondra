//! Claude's print protocol and owned process. No terminal or background-agent transport.
pub use alondra_foundation::uuid;
use std::path::Path;
use anyhow::{Context as _, Result, bail};
use serde_json::Value;

const MAX_DIAGNOSTIC: usize = 8192;

pub use alondra_process_group::owner::{Ownership, end_recorded, fence_recorded, recorded_run, terminate_orphan};

/// Records the batch's host as this daemon's child, with the births and write faults a test injected.
pub(crate) fn record_owner(directory: &Path, child_pid: u32, run_id: &str, injected: &crate::engine::Injected) -> Result<()> {
    Ok(alondra_process_group::owner::record(directory, child_pid, run_id, |pid| injected.birth(pid), |point| injected.write(point))?)
}

/// How a batch's turn ended, from what Claude reported and how its process exited; never from a
/// clock. `domain_orchestrator::tick` carries each one out.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BatchEnd {
    /// The turn finished with its result: the batch was handed over.
    Handled,
    /// Nothing the run did can have changed anything: the batch is offered again on its own.
    NotStarted { why: String },
    /// Claude did not finish and may have changed something: the user decides.
    Interrupted { calls: Vec<String>, why: String },
    /// One of Alondra's own tools was refused to Claude, after it may have changed something.
    Denied { calls: Vec<String>, why: String },
    /// Usage ran out before Claude answered: the batch waits for the reset.
    Limited { reset: Option<i64> },
    /// The user stopped the run; `effects` are the changes the daemon let it make, if it made any.
    Cancelled { effects: Option<Vec<String>> },
}

#[derive(Default, Debug)]
pub struct Stream {
    restricted: bool,
    /// The id the SDK gives the batch's one prompt: a result that names another is not this batch's.
    input: Option<String>,
    pub initialized: bool,
    pub result: bool,
    pub success: bool,
    pub error: Option<String>,
    /// Host bookkeeping failed; reconcile durable effects instead of asking about an IO error.
    pub(crate) storage_failure: bool,
    /// The context of the batch's first model call, and of its latest one.
    pub first_input_tokens: Option<u64>,
    pub input_tokens: Option<u64>,
    pub window: Option<u64>,
    /// What Claude said at the end of a successful turn.
    pub note: Option<String>,
    pub model: Option<String>,
    pub compacted: bool,
    pub post_compact_tokens: Option<u64>,
    pub acted: bool,
    pub denied: bool,
    pub(crate) native_request_rejected: bool,
    /// Claude reported that the plan's usage ran out, and the reset it gave in Unix milliseconds.
    pub limited: bool,
    pub limit_reset: Option<i64>,
    /// The model answered at least once, so the batch may have done something.
    pub worked: bool,
    /// The Alondra tools Claude called that change something, in order.
    pub alondra_calls: Vec<String>,
    /// What the session spent so far, by model, as its result said.
    pub spent: Vec<alondra_core::metrics::Usage>,
}
impl Stream {
    /// The parser of a batch run through the Agent SDK host, whose prompt the SDK calls `input`.
    pub fn for_input(input: &str) -> Self {
        Self { restricted: true, input: Some(input.to_owned()), ..Self::default() }
    }

    /// The context the batch read, once the model answered at least once.
    pub fn context(&self) -> Option<crate::domain_orchestrator::BatchContext> {
        Some(crate::domain_orchestrator::BatchContext { first: self.first_input_tokens?, last: self.input_tokens?, window: self.window })
    }
    /// Whether usage ran out, with the reset Claude reported if it did.
    pub fn limit(&self) -> Option<Option<i64>> {
        self.limited.then_some(self.limit_reset)
    }
    /// How the batch's turn ended.
    pub fn end(&self, exit_ok: bool, diagnostic: &str) -> BatchEnd {
        let effects = self.may_have_effects().then(|| self.alondra_calls.clone());
        if exit_ok && self.success && self.result && self.initialized && self.error.is_none() {
            return BatchEnd::Handled;
        }
        if let Some(reset) = self.limit() && !self.worked {
            return BatchEnd::Limited { reset };
        }
        let why = self.error.clone().unwrap_or_else(|| if diagnostic.trim().is_empty() {
            "Claude exited without a successful terminal result".into()
        } else {
            diagnostic.to_owned()
        });
        if self.native_request_rejected {
            return BatchEnd::Interrupted { calls: self.alondra_calls.clone(), why };
        }
        match effects {
            None => BatchEnd::NotStarted { why },
            Some(calls) if self.denied => BatchEnd::Denied { calls, why },
            Some(calls) => BatchEnd::Interrupted { calls, why },
        }
    }
    /// Whether the run may have changed something: it called a tool that does, or Claude was
    /// answering when the run ended without its result, so what came next is unknown.
    pub fn may_have_effects(&self) -> bool {
        !self.alondra_calls.is_empty() || (self.acted && !self.result)
    }
    /// Session fencing applies to every session-bearing record, including terminal results.
    pub fn accept(&mut self, line: &[u8], session: &str) -> Result<bool> {
        let v: Value = serde_json::from_slice(line).context("invalid Claude JSONL")?;
        if v.get("parent_tool_use_id").is_some_and(|p| !p.is_null()) { return Ok(false); }
        if let Some(id) = v.get("session_id").and_then(Value::as_str) {
            if id != session { bail!("Claude emitted a different session ID"); }
        }
        match (v["type"].as_str(), v["subtype"].as_str()) {
            (Some("system"), Some("init")) => {
                if v["session_id"] != session { bail!("Claude initialization omitted its session ID"); }
                if v["permissionMode"] != "auto" { bail!("Claude did not initialize in auto permission mode"); }
                let tools = v["tools"].as_array();
                // Claude prefixes plugin MCP names. Accept only the two exact launch forms
                // observed for this server, paired with their matching tool names.
                let required: &[&str] = if self.restricted { &["create_task", "ask"] } else { &["tasks_in_progress", "ask"] };
                let available = [
                    ("plugin:alondra-orchestrator:alondra-orchestrator", "mcp__plugin_alondra-orchestrator_alondra-orchestrator"),
                    ("alondra-orchestrator", "mcp__alondra-orchestrator"),
                ].iter().any(|(server, prefix)| {
                    v["mcp_servers"].as_array().is_some_and(|servers| servers.iter().any(|s| s["name"] == *server && s["status"] == "connected"))
                        && tools.is_some_and(|tools| required.iter().all(|name| {
                            let expected = format!("{prefix}__{name}");
                            tools.iter().any(|t| t.as_str() == Some(expected.as_str()))
                        }))
                });
                if !available {
                    let servers: Vec<String> = v["mcp_servers"].as_array().into_iter().flatten()
                        .map(|server| format!("{} {}", server["name"].as_str().unwrap_or("?"), server["status"].as_str().unwrap_or("?")))
                        .collect();
                    bail!("required Alondra MCP tools are unavailable (MCP servers: {})", if servers.is_empty() { "none".into() } else { servers.join(", ") });
                }
                let servers = v["mcp_servers"].as_array().expect("checked above");
                if self.restricted && servers.iter().any(|server| {
                    let name = server["name"].as_str().unwrap_or_default();
                    server["status"] == "connected"
                        && name != "alondra-orchestrator"
                        && name != "plugin:alondra-orchestrator:alondra-orchestrator"
                }) {
                    bail!("an unrelated MCP server is connected to the orchestrator");
                }
                if self.restricted && tools.expect("checked above").iter().any(|tool| {
                    let name = tool.as_str().unwrap_or_default();
                    !RESTRICTED_TOOLS.split(',').any(|allowed| allowed == name) && !alondra_tool(name)
                }) {
                    bail!("orchestrator has an unexpected built-in or MCP tool");
                }
                self.initialized = true;
                self.model = v["model"].as_str().map(str::to_owned);
                return Ok(true);
            }
            (Some("system"), Some("compact_boundary")) => {
                self.compacted = true;
                self.post_compact_tokens = v.pointer("/compact_metadata/post_tokens").and_then(Value::as_u64);
            }
            (Some("rate_limit_event"), _) => {
                let info = &v["rate_limit_info"];
                if info["status"] == "rejected" {
                    self.limited = true;
                    self.limit_reset = info["resetsAt"].as_i64().or_else(|| info["resets_at"].as_i64()).map(|seconds| seconds * 1000);
                }
            }
            (Some("assistant"), _) => {
                self.acted = true;
                let calls = v.pointer("/message/content").and_then(Value::as_array).into_iter().flatten()
                    .filter(|block| block["type"] == "tool_use")
                    .filter_map(|block| block["name"].as_str().filter(|name| alondra_tool(name)))
                    .filter_map(|name| name.rsplit("__").next())
                    .filter(|tool| alondra_domain_model::orchestrator_batch::changes_something(tool));
                self.alondra_calls.extend(calls.map(str::to_owned));
                if v["error"] == "rate_limit" {
                    self.limited = true;
                } else {
                    self.worked = true;
                }
                if let Some(u) = v.pointer("/message/usage").filter(|u| u.is_object()) {
                    // Latest main-agent call only; aggregate result usage can include many calls.
                    if let Some(input) = u["input_tokens"].as_u64() {
                        let context = input.saturating_add(u["cache_creation_input_tokens"].as_u64().unwrap_or(0)).saturating_add(u["cache_read_input_tokens"].as_u64().unwrap_or(0));
                        self.first_input_tokens.get_or_insert(context);
                        self.input_tokens = Some(context);
                    }
                }
                if let Some(model) = v.pointer("/message/model").and_then(Value::as_str) { self.model = Some(model.into()); }
            }
            (Some("result"), _) => {
                if self.result { bail!("multiple Claude terminal results"); }
                if v["session_id"] != session { bail!("Claude result omitted its session ID"); }
                if let Some(input) = &self.input
                    && !v["user_message_uuid"].as_str().is_some_and(|id| id.eq_ignore_ascii_case(input))
                    && !(v["subtype"] == "error_during_execution" && v["num_turns"] == 0)
                {
                    bail!("Claude reported the result of another input");
                }
                self.result = true;
                self.spent = crate::metrics_capture::claude_usage(&v);
                self.success = v["subtype"] == "success" && v["is_error"] == false;
                if self.success {
                    self.note = v["result"].as_str().map(str::to_owned);
                }
                // A refused optional tool is one Claude worked around; only Alondra's own
                // tools are required for a batch to be handled.
                if v["permission_denials"].as_array().is_some_and(|denials| denials.iter()
                    .any(|denial| denial["tool_name"].as_str().is_some_and(alondra_tool))) {
                    self.success = false;
                    self.denied = true;
                    self.error = Some("Claude denied a required tool permission in auto mode".into());
                }
                if !self.success && self.error.is_none() {
                    self.error = Some(bounded(v.get("errors").or_else(|| v.get("result")).unwrap_or(&Value::Null).to_string()));
                }
                if let Some(model) = &self.model { self.window = v["modelUsage"][model]["contextWindow"].as_u64(); }
            }
            _ => {}
        }
        Ok(false)
    }
}

pub(crate) const RESTRICTED_TOOLS: &str = "Skill,Read,Glob,Grep";

fn alondra_tool(name: &str) -> bool {
    name.starts_with("mcp__alondra-orchestrator__") || name.starts_with("mcp__plugin_alondra-orchestrator_alondra-orchestrator__")
}

/// The projects root is readable and never writable: nodes do the work in it.
pub(crate) fn read_only(root: &Path) -> Value {
    let pattern = format!("/{}/**", root.display());
    serde_json::json!({
        "additionalDirectories": [root],
        "allow": [format!("Read({pattern})")],
        "deny": [format!("Edit({pattern})")],
    })
}

pub(crate) fn bounded(mut text: String) -> String {
    if text.len() > MAX_DIAGNOSTIC {
        let mut at = MAX_DIAGNOSTIC;
        while !text.is_char_boundary(at) { at -= 1; }
        text.truncate(at);
    }
    text
}

pub struct Output { pub stream: Stream, pub diagnostic: String, pub exit_ok: bool }

impl Output {
    /// What the host wrote beside the SDK: shown and logged, never decided on.
    pub(crate) fn diagnosed(&mut self, text: &str) -> crate::domain_store::batch_effects::BatchReport {
        self.diagnostic = bounded(format!("{}{text}\n", self.diagnostic));
        crate::domain_store::batch_effects::BatchReport { kind: "diagnostic".into(), failure: self.stream.error.clone(), ..Default::default() }
    }
    /// The native session asked for a decision the orchestrator never grants: the batch ends.
    pub(crate) fn unexpected_decision(&mut self) -> crate::domain_store::batch_effects::BatchReport {
        self.stream.error = Some("Unexpected native decision in the Alondra orchestrator".into());
        crate::domain_store::batch_effects::BatchReport { kind: "unexpected_decision".into(), failure: self.stream.error.clone(), ..Default::default() }
    }
    /// A batch that never started, and why.
    pub fn not_started(why: &str) -> Self {
        Self { stream: Stream::default(), diagnostic: why.to_owned(), exit_ok: false }
    }

    pub fn end(&self) -> BatchEnd {
        self.stream.end(self.exit_ok, &self.diagnostic)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const INIT: &str = r#"{"type":"system","subtype":"init","session_id":"s","permissionMode":"auto","model":"opus","mcp_servers":[{"name":"alondra-orchestrator","status":"connected"}],"tools":["Skill","Read","mcp__alondra-orchestrator__create_task","mcp__alondra-orchestrator__ask"]}"#;
    const WORKED: &str = r#"{"type":"assistant","session_id":"s","message":{"model":"opus","content":[{"type":"text","text":"On it"}],"usage":{"input_tokens":2}}}"#;
    const CREATED: &str = r#"{"type":"assistant","session_id":"s","message":{"model":"opus","content":[{"type":"tool_use","id":"t","name":"mcp__alondra-orchestrator__create_task","input":{}}],"usage":{"input_tokens":2}}}"#;
    const DONE: &str = r#"{"type":"result","session_id":"s","subtype":"success","is_error":false,"permission_denials":[]}"#;

    fn ended(input: Option<&str>, lines: &[&str], exit_ok: bool, diagnostic: &str) -> BatchEnd {
        let mut stream = match input {
            Some(input) => Stream::for_input(input),
            None => Stream { restricted: true, ..Stream::default() },
        };
        for line in lines {
            if let Err(error) = stream.accept(line.as_bytes(), "s") {
                stream.error = Some(error.to_string());
                break;
            }
        }
        stream.end(exit_ok, diagnostic)
    }

    #[test]
    fn each_way_a_batch_run_ends_is_one_closed_outcome() {
        let denied = DONE.replace(r#""permission_denials":[]"#, r#""permission_denials":[{"tool_name":"mcp__alondra-orchestrator__create_task"}]"#);
        let limited = r#"{"type":"rate_limit_event","session_id":"s","rate_limit_info":{"status":"rejected","resetsAt":1900000000}}"#;
        let limited_result = r#"{"type":"result","session_id":"s","subtype":"success","is_error":true,"result":"You have hit your session limit"}"#;
        let missing = r#"{"type":"result","session_id":"s","subtype":"error_during_execution","is_error":true,"num_turns":0,"duration_api_ms":0,"errors":["No conversation found with session ID: s"]}"#;
        let created = vec!["create_task".to_owned()];
        assert_eq!(ended(None, &[INIT, WORKED, DONE], true, ""), BatchEnd::Handled);
        assert!(matches!(ended(None, &[], false, "cannot launch Claude"), BatchEnd::NotStarted { why } if why == "cannot launch Claude"));
        assert!(matches!(ended(None, &[INIT, WORKED], false, ""), BatchEnd::Interrupted { calls, .. } if calls.is_empty()),
            "Claude was answering when it ended: what came next is unknown");
        assert!(matches!(ended(None, &[INIT, CREATED], false, ""), BatchEnd::Interrupted { calls, .. } if calls == created));
        assert!(matches!(ended(None, &[INIT, CREATED, &denied], true, ""), BatchEnd::Denied { calls, .. } if calls == created));
        assert_eq!(ended(None, &[INIT, limited, limited_result], false, ""), BatchEnd::Limited { reset: Some(1_900_000_000_000) });
        for (input, diagnostic) in [(None, "No conversation found with session ID: s"), (Some("00000000-0000-4000-8000-000000000001"), "")] {
            assert!(matches!(ended(input, &[missing], false, diagnostic), BatchEnd::NotStarted { .. }),
                "what Claude wrote about the conversation decides nothing: the batch is offered again");
        }
    }

    #[test]
    fn a_result_names_the_batch_it_ends() {
        let input = "00000000-0000-4000-8000-000000000001";
        let bound = DONE.replace(r#""permission_denials":[]"#, &format!(r#""permission_denials":[],"user_message_uuid":"{input}""#));
        let other = DONE.replace(r#""permission_denials":[]"#, r#""permission_denials":[],"user_message_uuid":"00000000-0000-4000-8000-0000000000ff""#);
        assert_eq!(ended(Some(input), &[INIT, WORKED, &bound], true, ""), BatchEnd::Handled);
        assert!(matches!(ended(Some(input), &[INIT, WORKED, &other], true, ""),
            BatchEnd::Interrupted { why, .. } if why.contains("another input")));
        assert!(matches!(ended(Some(input), &[INIT, WORKED, DONE], true, ""), BatchEnd::Interrupted { .. }),
            "a result that names no input is not this batch's either");
    }

    #[test]
    fn latest_main_call_not_aggregate_or_child_usage() {
        let mut s = Stream::default();
        s.accept(br#"{"type":"assistant","message":{"model":"opus","usage":{"input_tokens":3,"cache_read_input_tokens":50,"cache_creation_input_tokens":7}}}"#, "a").unwrap();
        s.accept(br#"{"type":"assistant","parent_tool_use_id":"child","message":{"usage":{"input_tokens":999}}}"#, "a").unwrap();
        s.accept(br#"{"type":"result","session_id":"a","subtype":"success","is_error":false,"usage":{"input_tokens":9999},"modelUsage":{"opus":{"contextWindow":1000000}}}"#, "a").unwrap();
        assert_eq!(s.input_tokens, Some(60));
        assert_eq!(s.window, Some(1000000));
        assert!(s.success);
    }
    /// The result Claude 2.1.287 emits after `--restricted` refuses a Read outside the working directories.
    #[test]
    fn a_rejected_rate_limit_or_an_assistant_rate_limit_error_marks_the_stream_limited() {
        let accept = |lines: &[&str]| {
            let mut s = Stream::default();
            for line in lines {
                s.accept(line.as_bytes(), "a").unwrap();
            }
            s
        };
        let event = accept(&[r#"{"type":"rate_limit_event","session_id":"a","rate_limit_info":{"status":"rejected","resetsAt":1790000000}}"#]);
        assert_eq!((event.limited, event.limit_reset, event.worked), (true, Some(1_790_000_000_000), false));
        let warning = accept(&[r#"{"type":"rate_limit_event","session_id":"a","rate_limit_info":{"status":"allowed_warning","resetsAt":1790000000}}"#]);
        assert!(!warning.limited, "approaching the limit is not the limit");
        let assistant = accept(&[r#"{"type":"assistant","session_id":"a","error":"rate_limit","message":{"content":[]}}"#]);
        assert_eq!((assistant.limited, assistant.worked), (true, false));
        let result = accept(&[r#"{"type":"result","session_id":"a","subtype":"success","is_error":true,"result":"You've hit your session limit · resets 3:45pm"}"#]);
        assert!(!result.limited, "the result's words are not the limit: {result:?}");
    }

    const REFUSED_READ_RESULT: &[u8] = br#"{"type":"result","session_id":"a","subtype":"success","is_error":false,"terminal_reason":"completed","num_turns":4,"result":"Done.","permission_denials":[{"tool_name":"Read","tool_use_id":"toolu_01Hi3ayKKFGKNW3JAR2tb9Z2","tool_input":{"file_path":"/Users/example/.zshrc","limit":1}}]}"#;

    #[test]
    fn a_refused_optional_tool_in_a_successful_run_is_success() {
        let mut s = Stream::default();
        s.accept(REFUSED_READ_RESULT, "a").unwrap();
        assert!(s.success, "{s:?}");
        assert_eq!(s.error, None);
    }

    #[test]
    fn a_refused_alondra_tool_fails_the_run() {
        for tool in ["mcp__alondra-orchestrator__create_task", "mcp__plugin_alondra-orchestrator_alondra-orchestrator__launch"] {
            let mut s = Stream::default();
            let line = std::str::from_utf8(REFUSED_READ_RESULT).unwrap().replace(r#""tool_name":"Read""#, &format!(r#""tool_name":"{tool}""#));
            s.accept(line.as_bytes(), "a").unwrap();
            assert!(!s.success);
            assert_eq!(s.error.as_deref(), Some("Claude denied a required tool permission in auto mode"));
            assert!(s.denied);
        }
    }

    #[test]
    fn init_accepts_the_read_only_search_tools_and_nothing_that_writes() {
        let init = |tool: &str| format!(r#"{{"type":"system","subtype":"init","session_id":"a","permissionMode":"auto","mcp_servers":[{{"name":"alondra-orchestrator","status":"connected"}}],"tools":["Skill","Read","{tool}","mcp__alondra-orchestrator__create_task","mcp__alondra-orchestrator__ask"]}}"#);
        for tool in ["Glob", "Grep"] {
            let mut s = Stream { restricted: true, ..Stream::default() };
            assert!(s.accept(init(tool).as_bytes(), "a").unwrap());
        }
        for tool in ["Edit", "Write", "Bash", "NotebookEdit"] {
            let mut s = Stream { restricted: true, ..Stream::default() };
            assert!(s.accept(init(tool).as_bytes(), "a").is_err(), "{tool}");
        }
    }

    #[test]
    fn invalid_and_foreign_results_are_not_success() {
        let mut s = Stream::default();
        assert!(s.accept(b"oops", "a").is_err());
        assert!(s.accept(br#"{"type":"result","session_id":"b","subtype":"success"}"#, "a").is_err());
        assert!(!s.success);
    }
    #[test]
    fn compact_boundary_is_distinct_from_successful_noop() {
        let mut s = Stream::default();
        s.accept(br#"{"type":"system","subtype":"compact_boundary","session_id":"a","compact_metadata":{"post_tokens":100}}"#, "a").unwrap();
        assert!(s.compacted);
        assert_eq!(s.post_compact_tokens, Some(100));
        assert!(s.input_tokens.is_none());
    }
    #[test]
    fn orchestrator_init_rejects_node_and_accepts_exact_plugin_name() {
        let mut stream = Stream::default();
        let node_only = br#"{"type":"system","subtype":"init","session_id":"s","permissionMode":"auto","mcp_servers":[{"name":"plugin:alondra:alondra","status":"connected"}],"tools":["mcp__plugin_alondra_alondra__tasks_in_progress","mcp__plugin_alondra_alondra__ask"]}"#;
        assert!(stream.accept(node_only, "s").is_err());
        assert!(!stream.initialized);
        let orchestrator = br#"{"type":"system","subtype":"init","session_id":"s","permissionMode":"auto","mcp_servers":[{"name":"plugin:alondra-orchestrator:alondra-orchestrator","status":"connected"}],"tools":["mcp__plugin_alondra-orchestrator_alondra-orchestrator__tasks_in_progress","mcp__plugin_alondra-orchestrator_alondra-orchestrator__ask"]}"#;
        assert!(stream.accept(orchestrator, "s").is_ok());
        assert!(stream.initialized);
    }
    #[test]
    fn orchestrator_init_requires_its_own_mcp_server_and_tools() {
        let mut stream = Stream::default();
        let node_only = br#"{"type":"system","subtype":"init","session_id":"s","permissionMode":"auto","mcp_servers":[{"name":"alondra","status":"connected"}],"tools":["mcp__alondra__tasks_in_progress","mcp__alondra__ask"]}"#;
        assert!(stream.accept(node_only, "s").is_err());
        assert!(!stream.initialized);
        let orchestrator = br#"{"type":"system","subtype":"init","session_id":"s","permissionMode":"auto","mcp_servers":[{"name":"alondra-orchestrator","status":"connected"}],"tools":["mcp__alondra-orchestrator__tasks_in_progress","mcp__alondra-orchestrator__ask"]}"#;
        assert!(stream.accept(orchestrator, "s").is_ok());
        assert!(stream.initialized);
        let mut stream = Stream::default();
        let renamed_away = std::str::from_utf8(orchestrator).unwrap().replace("__tasks_in_progress", "__fleet");
        assert!(stream.accept(renamed_away.as_bytes(), "s").is_err(), "a catalog that still names the tool fleet lacks tasks_in_progress");
    }
    #[test]
    fn restricted_init_rejects_shell_agent_and_foreign_mcp() {
        for extra in [
            r#""Bash""#,
            r#""Agent""#,
            r#""mcp__foreign__shell""#,
        ] {
            let mut stream = Stream { restricted: true, ..Stream::default() };
            let init = format!(r#"{{"type":"system","subtype":"init","session_id":"s","permissionMode":"auto","mcp_servers":[{{"name":"alondra-orchestrator","status":"connected"}}],"tools":["Skill","Read","mcp__alondra-orchestrator__create_task","mcp__alondra-orchestrator__ask",{extra}]}}"#);
            assert!(stream.accept(init.as_bytes(), "s").is_err(), "{extra}");
        }
        let mut stream = Stream { restricted: true, ..Stream::default() };
        let foreign = br#"{"type":"system","subtype":"init","session_id":"s","permissionMode":"auto","mcp_servers":[{"name":"alondra-orchestrator","status":"connected"},{"name":"foreign","status":"connected"}],"tools":["Skill","Read","mcp__alondra-orchestrator__create_task","mcp__alondra-orchestrator__ask"]}"#;
        assert!(stream.accept(foreign, "s").is_err());
        let mut stream = Stream { restricted: true, ..Stream::default() };
        let legacy_catalog = br#"{"type":"system","subtype":"init","session_id":"s","permissionMode":"auto","mcp_servers":[{"name":"alondra-orchestrator","status":"connected"}],"tools":["Skill","Read","mcp__alondra-orchestrator__tasks_in_progress","mcp__alondra-orchestrator__ask"]}"#;
        assert!(stream.accept(legacy_catalog, "s").is_err(), "a v2 invocation needs the v2 catalog");
        let generic_catalog = br#"{"type":"system","subtype":"init","session_id":"s","permissionMode":"auto","mcp_servers":[{"name":"alondra-orchestrator","status":"connected"}],"tools":["Skill","Read","mcp__alondra-orchestrator__domain_command","mcp__alondra-orchestrator__ask"]}"#;
        assert!(stream.accept(generic_catalog, "s").is_err(), "a generic legacy tool is not a typed v2 catalog");
        let mut stream = Stream { restricted: true, ..Stream::default() };
        let v2 = br#"{"type":"system","subtype":"init","session_id":"s","permissionMode":"auto","mcp_servers":[{"name":"alondra-orchestrator","status":"connected"}],"tools":["Skill","Read","mcp__alondra-orchestrator__create_task","mcp__alondra-orchestrator__ask"]}"#;
        assert!(stream.accept(v2, "s").unwrap());
    }
    #[test]
    fn ownership_is_exclusive() {
        let scratch_directory = crate::test_support::Directory::new();
        let dir = scratch_directory.parent().unwrap().join(uuid().unwrap());
        let a = Ownership::acquire(&dir).unwrap().unwrap();
        assert!(Ownership::acquire(&dir).unwrap().is_none());
        drop(a);
        assert!(Ownership::acquire(&dir).unwrap().is_some());
        std::fs::remove_dir_all(dir).unwrap();
    }
    #[test]
    fn a_diagnostic_never_ends_a_batch_and_a_native_decision_does() {
        let mut output = Output::not_started("");
        let report = output.diagnosed("Unexpected native decision in the Alondra orchestrator");
        assert_eq!((report.failure, output.stream.error.as_deref()), (None, None), "stderr that quotes the phrase is only shown");
        assert!(output.diagnostic.contains("Unexpected native decision"));
        let report = output.unexpected_decision();
        assert!(report.failure.is_some() && output.stream.error.is_some());
        assert!(matches!(output.end(), BatchEnd::NotStarted { .. }));
    }

    #[test]
    fn a_limit_is_only_what_claude_reports_as_one_never_its_words() {
        let quoted = r#"{"type":"result","session_id":"s","subtype":"success","is_error":true,"result":"I hit your limit on retries"}"#;
        assert!(!matches!(ended(None, &[INIT, quoted], false, ""), BatchEnd::Limited { .. }));
        for words in ["You've hit your session limit · resets 3:45pm", "I hit your daily limit of 3 tasks"] {
            let mut stream = Stream::for_input("input"); stream.error = Some(words.into());
            assert_eq!(stream.limit(), None);
            let output = Output { stream, diagnostic: words.into(), exit_ok: false };
            assert!(!matches!(output.end(), BatchEnd::Limited { .. }), "{words}");
        }
        let mut reported = Stream::for_input("input"); reported.limited = true; reported.limit_reset = Some(20);
        assert_eq!(reported.limit(), Some(Some(20)));
    }
}
