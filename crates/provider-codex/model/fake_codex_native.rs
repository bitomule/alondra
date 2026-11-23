//! The repository's fake native Codex app-server (`agent-host/fake-codex.py`), played in the test's
//! process: it reads the same environment and the same sidecar file beside its executable, writes
//! every message it gets to the same transcript, and answers each request with what that script
//! prints. The turn modes that need real processes, FIFO barriers or a dying app-server are not
//! played: a sidecar or mode that asks for one is reported as trouble.

use serde_json::{Map, Value, json};
use std::collections::BTreeMap;
use std::io::Write as _;
use std::path::{Path, PathBuf};

const FIRST_THREAD: &str = "00000000-0000-4000-8000-000000000001";
const UNPLAYED_MODES: &[&str] = &[
    "replace_plan_with_work", "session_command", "blocked_command", "close_stdout", "exit_with_descendant", "idle_with_descendant", "complete_then_exit",
];
const UNPLAYED_SIDECAR: &[&str] = &[
    "threadOpenBarrier", "turnResponseBarrier", "checkpointRelease", "ownedCommand", "exitMcp", "exitMcpAfterFinal", "closeMcpStdout", "planImplementationRelease",
];

pub struct Native {
    env: BTreeMap<String, String>,
    executable: PathBuf,
    cwd: PathBuf,
    thread: String,
    turn: String,
    turn_number: u64,
    accepted_input: Option<Value>,
    refused_turn_start: bool,
    child_pending: bool,
    child_turn: String,
    mode: String,
}

/// What the app-server printed for a message: notifications, responses and requests, in order.
pub type Printed = Vec<Value>;

impl Native {
    pub fn new(executable: &Path, cwd: &Path, env: &BTreeMap<String, String>, process_id: u32) -> Self {
        let mode = env.get("ALONDRA_FAKE_MODE_FILE")
            .and_then(|file| std::fs::read_to_string(file).ok())
            .map(|mode| mode.trim().to_owned())
            .or_else(|| env.get("ALONDRA_FAKE_MODE").cloned())
            .unwrap_or_else(|| "complete".into());
        Self {
            env: env.clone(),
            executable: executable.to_path_buf(),
            cwd: cwd.to_path_buf(),
            thread: FIRST_THREAD.into(),
            turn: "native-turn".into(),
            turn_number: 0,
            accepted_input: None,
            refused_turn_start: false,
            child_pending: false,
            child_turn: format!("child-turn-{process_id}"),
            mode,
        }
    }

    /// What in the script's world this model does not play, if `message` asks for it.
    pub fn unplayed(&self) -> Option<String> {
        let fixture = self.fixture();
        if let Some(knob) = UNPLAYED_SIDECAR.iter().find(|knob| fixture.contains_key(**knob)) {
            return Some(format!("the Codex model plays no {knob}"));
        }
        let mode = fixture.get("turnMode").and_then(Value::as_str).unwrap_or(&self.mode);
        if UNPLAYED_MODES.contains(&mode) {
            return Some(format!("the Codex model plays no turn mode {mode}"));
        }
        self.env.contains_key("ALONDRA_FAKE_SILENT_ON").then(|| "the Codex model plays no silent method".to_owned())
    }

    fn sidecar(&self) -> PathBuf {
        self.env.get("ALONDRA_FAKE_ACCOUNT_FILE").map_or_else(|| PathBuf::from(format!("{}.account.json", self.executable.display())), PathBuf::from)
    }

    fn fixture(&self) -> Map<String, Value> {
        std::fs::read(self.sidecar()).ok().and_then(|bytes| serde_json::from_slice(&bytes).ok()).and_then(|value: Value| value.as_object().cloned()).unwrap_or_default()
    }

    fn codex_home(&self) -> PathBuf {
        self.env.get("CODEX_HOME").map_or_else(|| Path::new(self.env.get("HOME").map_or("", String::as_str)).join(".codex"), PathBuf::from)
    }

    fn thread_metadata(&self, identity: &str) -> PathBuf {
        self.codex_home().join("sessions").join(format!("fake-thread-{identity}.json"))
    }

    fn retained_thread(&self, identity: &str) -> Map<String, Value> {
        std::fs::read(self.thread_metadata(identity)).ok().and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok()).and_then(|value| value.as_object().cloned()).unwrap_or_default()
    }

    fn retain_thread(&self, identity: &str, cwd: &Value) {
        let metadata = self.thread_metadata(identity);
        let Some(folder) = metadata.parent() else { return };
        let _ = std::fs::create_dir_all(folder);
        if !metadata.exists() {
            let rollout = folder.join(format!("fake-rollout-{identity}.jsonl"));
            let _ = std::fs::write(&metadata, json!({"id": identity, "cwd": cwd, "path": rollout}).to_string());
        }
    }

    fn account_read(fixture: &Map<String, Value>) -> Value {
        fixture.get("accountRead").cloned().unwrap_or_else(|| json!({"account": {"type": "chatgpt", "email": "first@example.test", "planType": "plus"}, "requiresOpenaiAuth": true}))
    }

    fn auth_status(fixture: &Map<String, Value>) -> Value {
        let account = Self::account_read(fixture)["account"].clone();
        let method = if account.is_null() { Value::Null } else if account["type"] == "apiKey" { json!("apikey") } else { json!("chatgpt") };
        fixture.get("authStatus").cloned().unwrap_or_else(|| json!({"authMethod": method, "requiresOpenaiAuth": true, "authToken": null}))
    }

    fn model_list(&self, fixture: &Map<String, Value>) -> Value {
        let names = self.env.get("ALONDRA_FAKE_MODELS").map_or("fixture-model", String::as_str);
        let models: Vec<Value> = names.split(',').enumerate().map(|(at, model)| json!({"id": model, "model": model, "displayName": model, "isDefault": at == 0, "hidden": false,
            "supportedReasoningEfforts": [{"reasoningEffort": "high", "description": "High"}], "defaultReasoningEffort": "high", "inputModalities": ["text"]})).collect();
        fixture.get("models").cloned().unwrap_or_else(|| json!({"data": models, "nextCursor": null}))
    }

    fn note(printed: &mut Printed, method: &str, params: Value) {
        printed.push(json!({"method": method, "params": params}));
    }

    fn user_item(&self, printed: &mut Printed, params: &Value) {
        Self::note(printed, "item/started", json!({"threadId": self.thread, "turnId": self.turn, "item": {"type": "userMessage", "id": format!("user-{}", self.turn),
            "clientId": params["clientUserMessageId"], "content": params.get("input").cloned().unwrap_or_else(|| json!([]))}}));
    }

    fn finish(&self, printed: &mut Printed) {
        let turn = &self.turn;
        if self.env.get("ALONDRA_FAKE_TOKEN_USAGE").is_some_and(|on| on == "1") {
            let n = self.turn_number;
            let total = json!({"totalTokens": 30 * n, "inputTokens": 20 * n, "cachedInputTokens": 0, "outputTokens": 10 * n, "reasoningOutputTokens": 0});
            Self::note(printed, "thread/tokenUsage/updated", json!({"threadId": self.thread, "turnId": turn, "tokenUsage": {"total": total,
                "last": {"totalTokens": 30, "inputTokens": 20, "cachedInputTokens": 0, "outputTokens": 10, "reasoningOutputTokens": 0}, "modelContextWindow": 32768}}));
        }
        Self::note(printed, "item/agentMessage/delta", json!({"threadId": self.thread, "turnId": turn, "itemId": "answer", "delta": "untrusted delta"}));
        let text = self.env.get("ALONDRA_FAKE_FINAL_TEXT").cloned().unwrap_or_default();
        Self::note(printed, "item/completed", json!({"threadId": self.thread, "turnId": turn, "item": {"type": "agentMessage", "id": "answer", "text": text, "phase": "final_answer"}}));
        Self::note(printed, "turn/completed", json!({"threadId": self.thread, "turn": {"id": turn, "status": "completed"}}));
    }

    fn record(&self, message: &Value, fixture: &Map<String, Value>) {
        let append = |path: PathBuf| {
            let written = std::fs::OpenOptions::new().create(true).append(true).open(path).and_then(|mut file| writeln!(file, "{message}"));
            drop(written);
        };
        if let Some(transcript) = self.env.get("ALONDRA_FAKE_TRANSCRIPT") {
            append(PathBuf::from(transcript));
        }
        if fixture.get("trace").is_some_and(|trace| trace.as_bool() != Some(false) && !trace.is_null()) {
            append(PathBuf::from(format!("{}.calls", self.sidecar().display())));
        }
    }

    /// The app-server reads `message` and prints what the script prints for it.
    pub fn read(&mut self, message: &Value) -> Printed {
        let fixture = self.fixture();
        self.record(message, &fixture);
        let mut printed = Vec::new();
        let identity = message.get("id").cloned().unwrap_or(Value::Null);
        let Some(method) = message.get("method").and_then(Value::as_str) else {
            if [json!(77), json!("77"), json!(88), json!("88")].contains(&identity) {
                self.finish(&mut printed);
            }
            return printed;
        };
        if method == "initialized" {
            return printed;
        }
        let params = message.get("params").cloned().unwrap_or_else(|| json!({}));
        let error = |code: i64, text: &str| json!({"id": identity, "error": {"code": code, "message": text}});
        if method == "thread/resume" && let Some(allowed) = fixture.get("failResumes").and_then(Value::as_u64).filter(|allowed| *allowed > 0) {
            let failures = PathBuf::from(format!("{}.resume-failures", self.sidecar().display()));
            let failed = std::fs::metadata(&failures).map_or(0, |meta| meta.len());
            if failed < allowed {
                let written = std::fs::OpenOptions::new().create(true).append(true).open(&failures).and_then(|mut file| file.write_all(b"x"));
                drop(written);
                printed.push(error(-32603, "Fixture thread resume unavailable"));
                return printed;
            }
        }
        let result = match method {
            "config/read" => fixture.get("configRead").cloned().unwrap_or_else(|| {
                let names: Vec<String> = self.env.get("ALONDRA_FAKE_INHERITED_MCP_NAMES").and_then(|names| serde_json::from_str(names).ok()).unwrap_or_else(|| vec!["inherited-danger".into()]);
                json!({"config": {"mcp_servers": names.into_iter().map(|name| (name, json!({"command": "/untrusted"}))).collect::<Map<String, Value>>()}})
            }),
            "thread/start" | "thread/resume" => {
                if method == "thread/start" {
                    if let Some(fresh) = fixture.get("freshThreadId").and_then(Value::as_str) {
                        self.thread = fresh.to_owned();
                    }
                } else if let Some(requested) = params["threadId"].as_str() {
                    self.thread = requested.to_owned();
                }
                self.retain_thread(&self.thread, &params["cwd"]);
                let model = params["model"].as_str().filter(|model| !model.is_empty()).unwrap_or("fixture-model");
                json!({"thread": {"id": self.thread}, "model": model, "modelProvider": "openai"})
            }
            "account/read" => Self::account_read(&fixture),
            "getAuthStatus" => Self::auth_status(&fixture),
            "account/rateLimits/read" => {
                if fixture.get("rateLimitsError").is_some_and(|on| on.as_bool() != Some(false) && !on.is_null()) {
                    printed.push(error(-32603, "Fixture usage metadata unavailable"));
                    return printed;
                }
                fixture.get("rateLimits").cloned().unwrap_or_else(|| json!({"accountId": "fixture-workspace", "ordinaryUsageAllowed": true,
                    "rateLimits": {"primary": {"usedPercent": 10, "windowDurationMins": 300, "resetsAt": 4_102_444_800_u64}, "secondary": {"usedPercent": 20, "windowDurationMins": 10080, "resetsAt": 4_102_444_800_u64}},
                    "rateLimitsByLimitId": null}))
            }
            "model/list" => self.model_list(&fixture),
            "thread/backgroundTerminals/list" => json!({"data": [], "nextCursor": null}),
            "thread/items/list" => json!({"data": [{"turnId": self.child_turn, "item": {"type": "agentMessage", "id": "child-final", "phase": "final_answer", "text": "Managed native child public result"}}], "nextCursor": null}),
            "thread/turns/list" => json!({"data": [{"id": self.child_turn, "status": "completed", "items": []}], "nextCursor": null}),
            "thread/read" => {
                let requested = params["threadId"].as_str().unwrap_or_default().to_owned();
                let retained = self.retained_thread(&requested);
                let path = retained.get("path").and_then(Value::as_str).filter(|path| Path::new(path).is_file() && !fixture.get("missingRollout").is_some_and(|on| on.as_bool() != Some(false) && !on.is_null()));
                json!({"thread": {"id": requested, "cwd": retained.get("cwd"), "path": path, "turns": []}})
            }
            "mcpServerStatus/list" => {
                let result = json!({"data": [{"name": "alondra", "runtimeStatus": "connected"}], "nextCursor": null});
                if self.child_pending {
                    self.child_pending = false;
                    Self::note(&mut printed, "turn/completed", json!({"threadId": "native-child", "turn": {"id": self.child_turn, "status": "completed"}}));
                    printed.push(json!({"id": identity, "result": result}));
                    Self::note(&mut printed, "item/completed", json!({"threadId": self.thread, "turnId": self.turn, "item": {"type": "subAgentActivity", "id": "child-completed",
                        "kind": "completed", "agentThreadId": "native-child", "agentPath": "root/child", "model": null, "reasoningEffort": null}}));
                    return printed;
                }
                if self.env.get("ALONDRA_FAKE_FINISH_ON_MCP_STATUS").is_some_and(|on| on == "1") && self.turn_number == 1 && self.mode == "hold" {
                    printed.push(json!({"id": identity, "result": result}));
                    self.finish(&mut printed);
                    return printed;
                }
                if let Some(accepted) = self.accepted_input.take() {
                    printed.push(json!({"id": identity, "result": result}));
                    self.user_item(&mut printed, &accepted);
                    self.finish(&mut printed);
                    return printed;
                }
                result
            }
            "turn/start" => {
                self.turn_started(&fixture, &identity, &params, &mut printed);
                return printed;
            }
            "turn/interrupt" => {
                Self::note(&mut printed, "turn/completed", json!({"threadId": self.thread, "turn": {"id": self.turn, "status": "interrupted"}}));
                json!({})
            }
            _ => json!({}),
        };
        printed.push(json!({"id": identity, "result": result}));
        printed
    }

    fn turn_started(&mut self, fixture: &Map<String, Value>, identity: &Value, params: &Value, printed: &mut Printed) {
        let turn_mode = fixture.get("turnMode").and_then(Value::as_str).unwrap_or(&self.mode).to_owned();
        if turn_mode == "refuse_first_turn_start" && !self.refused_turn_start {
            self.refused_turn_start = true;
            printed.push(json!({"id": identity, "error": {"code": -32600, "message": "Fixture turn start refused"}}));
            return;
        }
        let retained = self.retained_thread(&self.thread);
        if let Some(path) = retained.get("path").and_then(Value::as_str).map(PathBuf::from)
            && !path.exists()
        {
            let line = json!({"type": "session_meta", "payload": {"id": self.thread, "cwd": retained.get("cwd")}});
            let _ = std::fs::write(&path, format!("{line}\n"));
        }
        self.turn_number += 1;
        self.turn = format!("native-turn-{}", self.turn_number);
        let (thread, turn, number) = (self.thread.clone(), self.turn.clone(), self.turn_number);
        Self::note(printed, "turn/started", json!({"threadId": thread, "turn": {"id": turn, "status": "inProgress"}}));
        printed.push(json!({"id": identity, "result": {"turn": {"id": turn, "status": "inProgress"}}}));
        if turn_mode == "accepted_without_consumption" && number == 1 {
            self.accepted_input = Some(params.clone());
            return;
        }
        self.user_item(printed, params);
        let plan_text = "- [ ] Implement the change\n- [ ] Verify the result";
        match turn_mode.as_str() {
            "permission" if number == 1 => printed.push(json!({"id": 88, "method": "item/commandExecution/requestApproval", "params": {"threadId": thread, "turnId": turn,
                "itemId": "pending-permission", "command": "alondra-fixture-needs-consent", "cwd": self.cwd,
                "reason": "The fixture waits for explicit user permission", "availableDecisions": ["accept", "decline"]}})),
            "paused_cell" => {
                Self::note(printed, "item/started", json!({"threadId": thread, "turnId": turn, "item": {"type": "mcpToolCall", "id": format!("exec-paused-{turn}"),
                    "server": "fixture", "tool": "barrier", "status": "inProgress", "arguments": {}}}));
                self.finish(printed);
            }
            "bad_request" | "context_exceeded" => {
                let kind = if turn_mode == "bad_request" { "badRequest" } else { "contextWindowExceeded" };
                Self::note(printed, "turn/completed", json!({"threadId": thread, "turn": {"id": turn, "status": "failed", "error": {"message": "The fixture native request was rejected", "codexErrorInfo": kind}}}));
            }
            "auth" => Self::note(printed, "turn/completed", json!({"threadId": thread, "turn": {"id": turn, "status": "failed",
                "error": {"message": "The fixture native sign-in needs attention", "codexErrorInfo": "unauthorized"}}})),
            "quota" => Self::note(printed, "turn/completed", json!({"threadId": thread, "turn": {"id": turn, "status": "failed",
                "error": {"message": "The fixture account quota is exhausted", "codexErrorInfo": "usageLimitExceeded"}}})),
            "child" if number == 1 => {
                self.child_pending = true;
                let child_turn = self.child_turn.clone();
                Self::note(printed, "item/started", json!({"threadId": thread, "turnId": turn, "item": {"type": "collabAgentToolCall", "id": "spawn-child", "tool": "spawnAgent",
                    "status": "inProgress", "senderThreadId": thread, "receiverThreadIds": [], "agentsStates": {}}}));
                Self::note(printed, "turn/started", json!({"threadId": "native-child", "turn": {"id": child_turn, "status": "inProgress"}}));
                Self::note(printed, "item/completed", json!({"threadId": thread, "turnId": turn, "item": {"type": "collabAgentToolCall", "id": "spawn-child", "tool": "spawnAgent",
                    "status": "completed", "senderThreadId": thread, "receiverThreadIds": ["native-child"], "agentsStates": {"native-child": {"status": "running", "message": null}}}}));
                Self::note(printed, "item/completed", json!({"threadId": "native-child", "turnId": child_turn, "item": {"type": "plan", "id": "child-plan",
                    "text": "This child plan must never become the whole task plan"}}));
                Self::note(printed, "turn/completed", json!({"threadId": thread, "turn": {"id": turn, "status": "completed"}}));
            }
            "plan" | "replace_plan" if (turn_mode == "plan" && number == 1) || (turn_mode == "replace_plan" && number <= 2) => {
                let item = if turn_mode == "replace_plan" { format!("plan-item-{number}") } else { "plan-item".into() };
                Self::note(printed, "item/completed", json!({"threadId": thread, "turnId": turn, "item": {"type": "plan", "id": item, "text": plan_text}}));
                Self::note(printed, "turn/completed", json!({"threadId": thread, "turn": {"id": turn, "status": "completed"}}));
            }
            "question" => {
                let question = json!({"id": 77, "method": "item/tool/requestUserInput", "params": {"threadId": thread, "turnId": turn, "itemId": "question-item",
                    "questions": [{"id": "language", "header": "Language", "question": "Which language?", "options": [{"label": "English", "description": "English text"}], "isSecret": false, "isOther": true}]}});
                printed.push(question.clone());
                printed.push(question);
            }
            "hold" if !(self.env.get("ALONDRA_FAKE_FINISH_ON_MCP_STATUS").is_some_and(|on| on == "1") && number > 1) => {}
            _ => self.finish(printed),
        }
    }
}
