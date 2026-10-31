//! The public strings of the report format, at the positions that produce them.
use serde_json::Value;

pub(super) fn key(section: &str, path: &[&str], key: &str, root: &Value) -> bool {
    let normalized: Vec<&str> = path.iter().map(|p| if p.parse::<usize>().is_ok() { "*" } else { p }).collect();
    let frame = frame(path, root);
    key_at(section, &normalized, key, root, frame)
}

fn key_at(section: &str, path: &[&str], key: &str, root: &Value, frame: &Value) -> bool {
    let fields: &[&str] = match (section, path) {
        ("versions.json", []) => &["cli", "daemon", "daemon_running", "launcher", "sdk_host", "claude", "views_protocol"],
        ("versions.json", ["sdk_host"]) => &["node", "sdk"],
        ("versions.json", ["views_protocol"]) => &["cli", "daemon"],
        ("versions.json", ["views_protocol", "daemon"]) => &["min", "max"],
        ("machine.json", []) => {
            &["model", "chip", "macos", "macos_build", "memory_bytes", "disk_free_bytes", "boot_time", "uptime_seconds", "launchd_job", "sleep_and_wake", "incident"]
        }
        ("machine.json", ["launchd_job"]) => &["state", "last_exit_code", "runs"],
        ("machine.json", ["sleep_and_wake", "*"]) => &["at", "kind"],
        ("machine.json", ["incident"]) => &["since", "cause", "cards", "nodes_shown", "healthy"],
        ("doctor.json", []) => &["checks"],
        ("doctor.json", ["checks", "*"]) => &["id", "status"],
        ("manifest.json", []) => {
            &["format", "format_version", "id", "created_at", "window", "alondra", "daemon_running", "description", "sections", "omitted", "trimmed", "log_read_capped_at_bytes"]
        }
        ("manifest.json", ["window"]) => &["from", "to", "hours"],
        ("manifest.json", ["description"]) => &["bytes", "sha256"],
        ("manifest.json", ["sections", "*"]) => &["name", "bytes", "sha256", "empty"],
        ("manifest.json", ["trimmed", "*"]) => &["section", "lines"],
        ("manifest.json", ["omitted"]) => &[
            "daemon_log_lines_without_template",
            "lifecycle_events_not_decoded",
            "sdk_host_instances_with_unknown_names",
            "sdk_host_frames_not_decoded",
            "sdk_host_frames_of_unknown_type",
        ],
        ("tasks.jsonl", []) => &[
            "task",
            "request",
            "node",
            "state",
            "started",
            "archived",
            "reviews_prs",
            "model",
            "pull_request",
            "states_in_window",
            "messages",
            "message_deliveries",
            "results",
            "open_questions",
            "executions",
            "closing",
        ],
        ("tasks.jsonl", ["states_in_window", "*"]) => &["at", "state"],
        ("tasks.jsonl", ["message_deliveries", "*"]) => &["state", "reason"],
        ("tasks.jsonl", ["results", "*"]) => &["final", "accepted", "withdrawn"],
        ("tasks.jsonl", ["open_questions", "*"]) => &["kind", "blocking", "for"],
        ("tasks.jsonl", ["executions", "*"]) => &["generation", "node", "instance", "host", "backend"],
        ("tasks.jsonl", ["closing"]) => &["since", "failures"],
        ("nodes.jsonl", []) => &["node", "task", "generation", "instance", "phase", "since", "model", "effort", "last_session_end", "api_failures"],
        ("nodes.jsonl", ["phase"]) => match root["phase"]["kind"].as_str() {
            Some("starting") => &["kind", "reviving", "idle_since"],
            Some("waiting_user" | "delivered") => &["kind", "in_turn"],
            Some("resuming") => &["kind", "idle_since", "after_wait"],
            Some("waiting_background") => &["kind", "since"],
            Some("paused") => &["kind", "since", "turn_since", "released", "reason_kind", "cleanup"],
            Some("lost" | "working" | "waiting_dependency" | "no_task" | "closing" | "closed" | "through_sdk") => &["kind"],
            _ => &[],
        },
        ("nodes.jsonl", ["last_session_end"]) => &["result", "process"],
        ("nodes.jsonl", ["last_session_end", "result"]) => &["terminal_reason", "subtype", "is_error"],
        ("nodes.jsonl", ["last_session_end", "process"]) => &["exit_code", "signal"],
        ("nodes.jsonl", ["api_failures"]) => &["failures", "first", "last"],
        ("sdk-host.jsonl", []) => &["instance", "last_written", "protocol_versions", "frames", "diagnostics"],
        ("sdk-host.jsonl", ["frames", "*"]) => match frame["type"].as_str() {
            Some("command") => &["type", "action"],
            Some("receipt") => &["type", "status", "phase", "code"],
            Some("event") => &["type", "cursor", "observation", "detail"],
            Some("hello" | "challenge" | "attach" | "attached" | "acknowledge") => &["type"],
            _ => &[],
        },
        ("sdk-host.jsonl", ["frames", "*", "detail"]) if frame["type"] == "event" && frame["observation"] == "process_exited" => &["exit_code", "signal"],
        ("lifecycle.jsonl", []) => &["task", "revision", "event", "fact", "generation", "cursor", "node", "instance", "data"],
        ("lifecycle.jsonl", _) => return lifecycle_key(path, key, root),
        _ => &[],
    };
    fields.contains(&key)
}

pub(super) fn value(section: &str, path: &[&str], value: &str, root: &Value) -> bool {
    let normalized: Vec<&str> = path.iter().map(|p| if p.parse::<usize>().is_ok() { "*" } else { p }).collect();
    value_at(section, &normalized, value, root, frame(path, root))
}

fn frame<'a>(path: &[&str], root: &'a Value) -> &'a Value {
    if let ["frames", index, ..] = path
        && let Ok(index) = index.parse::<usize>()
    {
        return &root["frames"][index];
    }
    &Value::Null
}

fn value_at(section: &str, path: &[&str], value: &str, root: &Value, frame: &Value) -> bool {
    let allowed: &[&str] = match (section, path) {
        ("machine.json", ["launchd_job", "state"]) => &["running", "not running", "waiting", "spawn scheduled", "exited"],
        ("machine.json", ["launchd_job", "last_exit_code"]) => &["never exited"],
        ("machine.json", ["sleep_and_wake", "*", "kind"]) => &["Sleep", "Wake", "DarkWake", "Start", "Shutdown", "Restart"],
        ("machine.json", ["incident", "cause"]) => &["disk", "memory", "load", "declared"],
        ("doctor.json", ["checks", "*", "id"]) => {
            return crate::doctor::check_ids().contains(&value);
        }
        ("doctor.json", ["checks", "*", "status"]) => &["pass", "warn", "fail"],
        ("manifest.json", ["format"]) => &["alondra.report"],
        ("manifest.json", ["sections", "*", "name"] | ["trimmed", "*", "section"]) => {
            return super::super::bundle::SECTIONS.contains(&value);
        }
        ("manifest.json", ["sections", "*", "empty"]) => return empty_reason(value),
        ("tasks.jsonl", ["state"] | ["states_in_window", "*", "state"]) => {
            return serde_json::from_value::<alondra_core::domain::TaskState>(Value::String(value.into())).is_ok();
        }
        ("tasks.jsonl", ["pull_request"]) => {
            return serde_json::from_value::<alondra_core::domain::PrState>(Value::String(value.into())).is_ok();
        }
        ("tasks.jsonl", ["message_deliveries", "*", "state"]) => &["dispatching", "uncertain", "handed", "retryable", "discarded"],
        ("tasks.jsonl", ["open_questions", "*", "kind"]) => &["question", "action_request"],
        ("tasks.jsonl", ["open_questions", "*", "for"]) => &["user", "orchestrator"],
        ("tasks.jsonl", ["executions", "*", "backend"]) => &["sdk", "claude_bg"],
        ("tasks.jsonl", ["executions", "*", "host"]) => &["claude-bg"],
        ("tasks.jsonl" | "nodes.jsonl", ["model"]) => &["haiku", "sonnet", "opus", "fable"],
        ("nodes.jsonl", ["effort"]) => &["low", "medium", "high", "xhigh", "max"],
        ("nodes.jsonl", ["phase", "kind"]) => &[
            "starting",
            "lost",
            "working",
            "waiting_background",
            "waiting_user",
            "paused",
            "delivered",
            "resuming",
            "waiting_dependency",
            "no_task",
            "closing",
            "closed",
            "through_sdk",
        ],
        ("nodes.jsonl", ["phase", "reason_kind"]) if root["phase"]["kind"] == "paused" => &["memory", "declared", "usage", "by_user", "api_error", "disk"],
        ("nodes.jsonl", ["phase", "cleanup"]) if root["phase"]["kind"] == "paused" => &["pending", "terminated", "done"],
        ("nodes.jsonl", ["last_session_end", "result", "terminal_reason"]) => &["completed", "background_requested", "tool_deferred"],
        ("nodes.jsonl", ["last_session_end", "result", "subtype"]) => {
            &["success", "error_during_execution", "error_max_turns", "error_max_budget_usd", "error_max_structured_output_retries"]
        }
        ("sdk-host.jsonl", ["diagnostics"]) => &["not available: the daemon is not running"],
        ("sdk-host.jsonl", ["frames", "*", "type"]) => &["hello", "command", "receipt", "event", "challenge", "attach", "attached", "acknowledge"],
        ("sdk-host.jsonl", ["frames", "*", "action"]) => &[
            "start",
            "offer",
            "interrupt",
            "force_stop",
            "close",
            "respond",
            "set_model",
            "set_permission_mode",
            "mcp_status",
            "mcp_reconnect",
            "mcp_authenticate",
            "mcp_clear_auth",
        ],
        ("sdk-host.jsonl", ["frames", "*", "status"]) => &["accepted", "rejected"],
        ("sdk-host.jsonl", ["frames", "*", "phase"]) => &["admitted", "offered", "applied"],
        ("sdk-host.jsonl", ["frames", "*", "observation"]) => &["sdk", "decision_requested", "decision_settled", "process_exited"],
        ("sdk-host.jsonl", ["frames", "*", "detail"]) if frame["type"] == "event" => match frame["observation"].as_str() {
            Some("sdk") => &["initialized", "message", "result", "task", "status", "diagnostic", "control_result", "unsupported"],
            Some("decision_requested") => &["permission", "question", "plan", "elicitation"],
            Some("decision_settled") => &["answered", "cancelled"],
            _ => &[],
        },
        ("lifecycle.jsonl", ["event"]) => return EVENTS.contains(&value),
        ("lifecycle.jsonl", ["fact"]) if root["event"] == "runtime" => {
            return FACTS.contains(&value);
        }
        ("lifecycle.jsonl", _) => return lifecycle_value(path, value, root),
        _ => &[],
    };
    allowed.contains(&value)
}

const EVENTS: &[&str] = &[
    "admit",
    "runtime",
    "ask",
    "answer",
    "decide",
    "move_card",
    "plan_approved",
    "block_delivery",
    "delivery_unblocked",
    "present",
    "accept",
    "merge_observed",
    "hand_back",
    "wait_for_dependencies",
    "dependencies_resolved",
    "tell",
    "pause",
    "resume",
    "finish",
    "drain_observed",
    "post_merge_declared",
    "post_merge_ended",
    "retire_released_execution",
    "execution_released",
    "reopen",
    "hold_for_plan_review",
    "plan_review_settled",
    "doubt_handoff",
    "settle_handoff",
    "resend_untaken",
    "resend_doubtful",
    "discard",
    "await_ended",
    "request_control",
    "compact",
];
const FACTS: &[&str] = &[
    "ask",
    "resource_available",
    "turn_started",
    "input_consumed",
    "input_dropped",
    "work_started",
    "work_ended",
    "continuation_consumed",
    "turn_ended",
    "connection_lost",
    "resource_released",
    "callback_settled",
    "callback_aborted",
    "answered_by_policy",
    "effect_accepted",
    "effect_observed",
    "interrupt_accepted",
    "notification",
    "host_ended",
];

fn lifecycle_fields(root: &Value) -> &'static [&'static str] {
    let event = root["event"].as_str().unwrap_or_default();
    if event == "runtime" {
        return match root["fact"].as_str().unwrap_or_default() {
            "ask" => &["card", "callback", "reason"],
            "turn_started" => &["turn", "trigger"],
            "input_consumed" => &["input"],
            "input_dropped" => &["input", "card"],
            "work_started" => &["turn", "work"],
            "work_ended" => &["work", "continuation"],
            "continuation_consumed" => &["continuation"],
            "turn_ended" => &["turn", "unheard_card", "wake", "awaits", "stalled"],
            "connection_lost" | "resource_released" => &["problem_card"],
            "callback_settled" => &["callback", "continuation"],
            "callback_aborted" => &["callback", "unheard_card"],
            "answered_by_policy" => &["callback", "decision"],
            "effect_accepted" | "effect_observed" => &["operation"],
            "interrupt_accepted" => &["turn"],
            "host_ended" => &["unheard_card", "resume"],
            _ => &[],
        };
    }
    match event {
        "admit" => &["node", "conversation", "instance", "host"],
        "ask" | "hold_for_plan_review" => {
            if event == "ask" {
                &["card"]
            } else {
                &["review"]
            }
        }
        "answer" | "resume" | "doubt_handoff" => &["card", "input"],
        "decide" => &["card", "decision"],
        "move_card" => &["from", "to"],
        "block_delivery" => &["delivery", "dependencies"],
        "delivery_unblocked" | "present" => &["delivery"],
        "accept" => &["delivery", "merge"],
        "merge_observed" | "post_merge_declared" => &["operation"],
        "hand_back" => &["delivery", "input"],
        "wait_for_dependencies" => &["dependencies"],
        "dependencies_resolved" | "tell" | "execution_released" | "reopen" | "compact" => &["input"],
        "pause" => &["card", "owner"],
        "finish" => &["intent", "cleanup", "post_merge"],
        "drain_observed" => &["obligation"],
        "post_merge_ended" => &["operation", "fact"],
        "retire_released_execution" => &["problem_card", "input"],
        "plan_review_settled" => &["review", "plan_revision", "outcome"],
        "settle_handoff" => &["card", "resend", "unheard_card"],
        "resend_untaken" | "resend_doubtful" => &["input", "resend"],
        "discard" => &["card", "unheard_card", "stalled"],
        "await_ended" => &["unheard_card", "wake", "stalled"],
        "request_control" => &["control", "kind"],
        _ => &[],
    }
}

fn lifecycle_key(path: &[&str], key: &str, root: &Value) -> bool {
    if path == ["data"] {
        return lifecycle_fields(root).contains(&key);
    }
    let field = match path {
        ["data", field] if lifecycle_fields(root).contains(field) => *field,
        ["data", "kind", "data"] if root["event"] == "request_control" => {
            return match root["data"]["kind"]["kind"].as_str() {
                Some("interrupt" | "force_stop") => key == "turn",
                Some("set_permission_mode") => key == "mode",
                _ => false,
            };
        }
        ["data", "kind", "data", "mode"] if root["event"] == "request_control" && root["data"]["kind"]["kind"] == "set_permission_mode" => {
            return key == "kind";
        }
        ["data", "decision", "data"] if lifecycle_fields(root).contains(&"decision") => {
            return false;
        }
        _ => return false,
    };
    match field {
        "reason" | "intent" | "fact" | "outcome" => key == "kind",
        "decision" | "trigger" | "awaits" | "obligation" | "kind" => {
            key == "kind"
                || (key == "data"
                    && root["data"][field]["kind"].as_str().is_some_and(|kind| {
                        lifecycle_value(&["data", field, "kind"], kind, root) && !matches!(kind, "allow" | "decline" | "unprompted" | "machine" | "read_mcp_status")
                    }))
        }
        _ => false,
    }
}

fn lifecycle_value(path: &[&str], value: &str, root: &Value) -> bool {
    let allowed: &[&str] = match path {
        ["data", "owner"] if root["event"] == "pause" => &["user", "gate", "machine", "disk", "usage", "claude_api", "orchestrator"],
        ["data", "host"] if root["event"] == "admit" => &["claude-bg"],
        ["data", "reason", "kind"] if root["event"] == "runtime" && root["fact"] == "ask" => &["question", "permission", "pause", "recovery", "unheard", "doubt", "dropped"],
        ["data", "trigger", "kind"] if root["event"] == "runtime" && root["fact"] == "turn_started" => &["input", "owned_continuation", "unprompted"],
        ["data", "awaits", "kind"] if root["event"] == "runtime" && root["fact"] == "turn_ended" => &["user", "orchestrator", "machine", "tasks"],
        ["data", "decision", "kind"] if root["event"] == "decide" || (root["event"] == "runtime" && root["fact"] == "answered_by_policy") => {
            &["allow", "answers", "deny", "accept", "decline"]
        }
        ["data", "intent", "kind"] if root["event"] == "finish" => &["complete", "cancel"],
        ["data", "obligation", "kind"] if root["event"] == "drain_observed" => &["execution", "work", "callback", "merge", "cleanup", "external_effect", "post_merge"],
        ["data", "fact", "kind"] if root["event"] == "post_merge_ended" => &["node_declared_end", "cleanup", "user_decision", "session_over", "turn_ended"],
        ["data", "outcome", "kind"] if root["event"] == "plan_review_settled" => &["unchanged", "plan_changed", "cancelled", "kept_by_user"],
        ["data", "kind", "kind"] if root["event"] == "request_control" => {
            &["interrupt", "force_stop", "set_model", "set_permission_mode", "read_mcp_status", "reconnect_mcp", "start_mcp_sign_in", "clear_mcp_sign_in"]
        }
        ["data", "kind", "data", "mode", "kind"] if root["event"] == "request_control" && root["data"]["kind"]["kind"] == "set_permission_mode" => &["default", "plan", "dont_ask"],
        _ => &[],
    };
    allowed.contains(&value)
}

fn empty_reason(value: &str) -> bool {
    if [
        "the app had nothing to add",
        "la app no tenía nada que añadir",
        "what the app sent is not one JSON object per line",
        "lo que mandó la app no es un objeto JSON por línea",
        "no samples on this Mac",
        "no hay muestras en este Mac",
        "no samples in the window",
        "no hay muestras en ese periodo",
        "the checks could not run",
        "las comprobaciones no pudieron ejecutarse",
        "there is no alondrad.log on this Mac",
        "no hay alondrad.log en este Mac",
        "no lines in the window",
        "no hay líneas en ese periodo",
        "there is no domain.db on this Mac",
        "no hay domain.db en este Mac",
        "domain.db has a path SQLite cannot open",
        "domain.db tiene una ruta que SQLite no puede abrir",
        "domain.db has no lifecycle journal",
        "domain.db no tiene diario de ciclo de vida",
        "no lifecycle events for the tasks of the window",
        "no hay eventos de ciclo de vida de las tareas de ese periodo",
        "no tasks in the window",
        "no hay tareas en ese periodo",
        "no nodes for the tasks of the window",
        "no hay nodos de las tareas de ese periodo",
        "no SDK hosts on this Mac",
        "no hay hosts del SDK en este Mac",
        "no SDK host wrote in the window",
        "ningún host del SDK escribió en ese periodo",
        "cut so the report fits in 5 MB",
        "nothing to report",
    ]
    .contains(&value)
    {
        return true;
    }
    const SQLITE_REASONS: &[&str] = &[
        "InternalMalfunction",
        "PermissionDenied",
        "OperationAborted",
        "DatabaseBusy",
        "DatabaseLocked",
        "OutOfMemory",
        "ReadOnly",
        "OperationInterrupted",
        "SystemIoFailure",
        "DatabaseCorrupt",
        "NotFound",
        "DiskFull",
        "CannotOpen",
        "FileLockingProtocolFailed",
        "SchemaChanged",
        "TooBig",
        "ConstraintViolation",
        "TypeMismatch",
        "ApiMisuse",
        "NoLargeFileSupport",
        "AuthorizationForStatementDenied",
        "ParameterOutOfRange",
        "NotADatabase",
        "Unknown",
        "not a database",
    ];
    for prefix in ["domain.db could not be opened (", "domain.db could not be read (", "domain.db no se pudo abrir (", "domain.db no se pudo leer ("] {
        if let Some(reason) = value.strip_prefix(prefix).and_then(|s| s.strip_suffix(')')) {
            return SQLITE_REASONS.contains(&reason);
        }
    }
    for prefix in ["alondrad.log could not be read (", "alondrad.log no se pudo leer ("] {
        if let Some(reason) = value.strip_prefix(prefix).and_then(|s| s.strip_suffix(')')) {
            use std::io::ErrorKind;
            return [
                ErrorKind::PermissionDenied,
                ErrorKind::ConnectionRefused,
                ErrorKind::ConnectionReset,
                ErrorKind::ConnectionAborted,
                ErrorKind::NotConnected,
                ErrorKind::AddrInUse,
                ErrorKind::AddrNotAvailable,
                ErrorKind::BrokenPipe,
                ErrorKind::AlreadyExists,
                ErrorKind::WouldBlock,
                ErrorKind::InvalidInput,
                ErrorKind::InvalidData,
                ErrorKind::TimedOut,
                ErrorKind::WriteZero,
                ErrorKind::Interrupted,
                ErrorKind::Unsupported,
                ErrorKind::UnexpectedEof,
                ErrorKind::OutOfMemory,
                ErrorKind::Other,
            ]
            .iter()
            .any(|kind| kind.to_string() == reason);
        }
    }
    for (prefix, suffix) in
        [("domain.db is of a newer version (", ") than this alondra reads (5)"), ("domain.db es de una versión más nueva (", ") que la que lee este alondra (5)")]
    {
        if let Some(version) = value.strip_prefix(prefix).and_then(|s| s.strip_suffix(suffix)) {
            return version.parse::<i64>().is_ok_and(|v| v > 5);
        }
    }
    false
}
