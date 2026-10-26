//! The collectors: each one builds its section only from what is known to be public (versions,
//! numbers, states, the words of the code) and replaces every id by its pseudonym. A collector that
//! fails leaves its section empty with the reason; it never falls back to raw text.
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use rusqlite::{Connection, OpenFlags, params};
use serde_json::{Map, Value, json};

use super::daemon_log;
use super::redact::{Kind, Redactor, is_number, is_random_id, is_version};
use super::templates::Catalog;
use super::time;

/// What the collectors ask of the Mac: tests answer with fixtures.
pub trait Host {
    fn home(&self) -> &Path;
    fn now(&self) -> i64;
    /// `GET /daemon`, or `None` when the daemon does not answer.
    fn daemon(&self) -> Option<Value>;
    /// Each doctor check's id and status (`pass`, `warn`, `fail`).
    fn doctor(&self) -> Option<Vec<(String, String)>>;
    /// A command's standard output, or `None` when it cannot run or fails.
    fn run(&self, program: &str, args: &[&str]) -> Option<String>;
    /// Where the installed binaries are (`alondrad`, the launcher), next to this `alondra`.
    fn bin_dir(&self) -> Option<PathBuf>;
    fn service_label(&self) -> String;
    fn uid(&self) -> u32;
}

#[derive(Debug, Clone, Copy)]
pub struct Window {
    pub from: i64,
    pub to: i64,
    pub hours: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Status {
    Ok,
    Empty(String),
}

#[derive(Debug, Clone)]
pub struct Section {
    pub name: &'static str,
    pub text: String,
    pub status: Status,
}

impl Section {
    fn ok(name: &'static str, text: String) -> Self {
        Section { name, text, status: Status::Ok }
    }

    fn empty(name: &'static str, reason: impl Into<String>) -> Self {
        Section { name, text: String::new(), status: Status::Empty(reason.into()) }
    }
}

/// What was left out, by reason, for the manifest.
pub type Omitted = BTreeMap<&'static str, u64>;

pub struct Collected {
    pub sections: Vec<Section>,
    pub omitted: Omitted,
    pub log_capped: bool,
    pub daemon_running: bool,
}

pub fn collect(host: &dyn Host, window: Window, catalog: &Catalog, redactor: &mut Redactor) -> Collected {
    let daemon = host.daemon();
    let mut omitted = Omitted::new();
    let mut sections = vec![
        versions(host, daemon.as_ref()),
        machine(host, window),
        samples(host, window, redactor),
        doctor(host),
    ];
    let (log, capped) = daemon_log_section(host, window, catalog, redactor, &mut omitted);
    sections.push(log);
    match open_domain(host.home()) {
        Ok(db) => {
            let tasks = touched_tasks(&db, window);
            sections.push(lifecycle(&db, &tasks, redactor, &mut omitted));
            sections.push(tasks_section(&db, &tasks, window, redactor));
            sections.push(nodes(&db, &tasks, host.home(), redactor));
        }
        Err(reason) => {
            for name in ["lifecycle.jsonl", "tasks.jsonl", "nodes.jsonl"] {
                sections.push(Section::empty(name, reason.clone()));
            }
        }
    }
    sections.push(sdk_host(host, window, daemon.is_some(), redactor, &mut omitted));
    Collected { sections, omitted, log_capped: capped, daemon_running: daemon.is_some() }
}

fn pretty(value: &Value) -> String {
    serde_json::to_string_pretty(value).unwrap_or_default() + "\n"
}

fn jsonl(rows: &[Value]) -> String {
    rows.iter().map(|row| serde_json::to_string(row).unwrap_or_default() + "\n").collect()
}

/// A version written anywhere in `text` (`alondrad 0.3.5`, `2.1.30 (Claude Code)`), or null.
fn version_in(text: Option<&str>) -> Value {
    text.and_then(|t| t.split_whitespace().find(|w| is_version(w)).map(|w| Value::from(w.trim_start_matches('v'))))
        .unwrap_or(Value::Null)
}

fn versions(host: &dyn Host, daemon: Option<&Value>) -> Section {
    let bin = host.bin_dir();
    let tool = |name: &str| bin.as_ref().and_then(|dir| host.run(&dir.join(name).to_string_lossy(), &["--version"]));
    let daemon_version = match daemon.and_then(|d| d["version"].as_str()) {
        Some(version) => version_in(Some(version)),
        None => version_in(tool("alondrad").as_deref()),
    };
    let launcher = tool("alondrad-launcher").and_then(|out| out.split_whitespace().last().filter(|w| is_number(w) || is_version(w)).map(str::to_owned));
    let host_manifest = bin
        .as_ref()
        .and_then(|dir| std::fs::read_to_string(dir.join("../libexec/alondra/sdk/alondra-host.json")).ok())
        .and_then(|text| serde_json::from_str::<Value>(&text).ok());
    let manifest_version = |key: &str| version_in(host_manifest.as_ref().and_then(|m| m[key].as_str()));
    let views = |value: &Value| if value.is_u64() { value.clone() } else { Value::Null };
    let value = json!({
        "cli": env!("CARGO_PKG_VERSION"),
        "daemon": daemon_version,
        "daemon_running": daemon.is_some(),
        "launcher": launcher,
        "sdk_host": {"node": manifest_version("node"), "sdk": manifest_version("sdk")},
        "claude": version_in(host.run("claude", &["--version"]).as_deref()),
        "views_protocol": {
            "cli": alondra_core::views::VIEWS_PROTOCOL,
            "daemon": daemon.map(|d| json!({"min": views(&d["views_protocol"]["min"]), "max": views(&d["views_protocol"]["max"])})),
        },
    });
    Section::ok("versions.json", pretty(&value))
}

fn machine(host: &dyn Host, window: Window) -> Section {
    let sysctl = |key: &str| host.run("sysctl", &["-n", key]).map(|s| s.trim().to_owned());
    let model = sysctl("hw.model").filter(|m| m.chars().all(|c| c.is_ascii_alphanumeric() || c == ',') && m.len() <= 16);
    let chip = sysctl("machdep.cpu.brand_string").filter(|c| is_chip(c));
    let macos = host.run("sw_vers", &["-productVersion"]).map(|v| v.trim().to_owned()).filter(|v| is_version(v) || is_number(v));
    let build = host.run("sw_vers", &["-buildVersion"]).map(|v| v.trim().to_owned()).filter(|b| (4..=10).contains(&b.len()) && b.chars().all(|c| c.is_ascii_alphanumeric()));
    let memory = sysctl("hw.memsize").and_then(|m| m.parse::<u64>().ok());
    let boot = sysctl("kern.boottime").and_then(|text| boot_seconds(&text));
    let label = host.service_label();
    let launchd = host.run("launchctl", &["print", &format!("gui/{}/{label}", host.uid())]).map(|text| launchd_job(&text));
    let power = host.run("pmset", &["-g", "log"]).map(|text| power_events(&text, window)).unwrap_or_default();
    let value = json!({
        "model": model,
        "chip": chip,
        "macos": macos,
        "macos_build": build,
        "memory_bytes": memory,
        "disk_free_bytes": disk_free(host.home()),
        "boot_time": boot.map(|s| time::utc(s * 1_000)),
        "uptime_seconds": boot.map(|s| (host.now() / 1_000 - s).max(0)),
        "launchd_job": launchd,
        "sleep_and_wake": power,
        "incident": incident(host.home()),
    });
    Section::ok("machine.json", pretty(&value))
}

fn is_chip(text: &str) -> bool {
    let mut words = text.split(' ');
    words.next() == Some("Apple")
        && words.next().is_some_and(|m| m.strip_prefix('M').is_some_and(|n| !n.is_empty() && n.chars().all(|c| c.is_ascii_digit())))
        && words.all(|w| matches!(w, "Pro" | "Max" | "Ultra"))
}

/// `{ sec = 1790000000, usec = 0 } Tue Oct 21 …` → its seconds.
fn boot_seconds(text: &str) -> Option<i64> {
    let after = text.split_once("sec = ")?.1;
    after.split(|c: char| !c.is_ascii_digit()).next()?.parse().ok()
}

fn launchd_job(text: &str) -> Value {
    let field = |name: &str| {
        text.lines().find_map(|line| line.trim().strip_prefix(name).and_then(|rest| rest.trim_start().strip_prefix('=')).map(|v| v.trim().to_owned()))
    };
    let state = field("state").filter(|s| matches!(s.as_str(), "running" | "not running" | "waiting" | "spawn scheduled" | "exited"));
    let last_exit = field("last exit code").map(|code| {
        let digits: String = code.chars().take_while(|c| c.is_ascii_digit() || *c == '-').collect();
        if let Ok(number) = digits.parse::<i64>() { Value::from(number) } else if code.contains("never exited") { Value::from("never exited") } else { Value::Null }
    });
    let runs = field("runs").and_then(|r| r.parse::<u64>().ok());
    json!({"state": state, "last_exit_code": last_exit, "runs": runs})
}

/// The sleeps and wakes of `pmset -g log` in the window: their time and kind, nothing else.
fn power_events(text: &str, window: Window) -> Vec<Value> {
    text.lines()
        .filter_map(|line| {
            let stamp = line.get(..25)?;
            let at = pmset_time(stamp)?;
            let kind = line[25..].split_whitespace().next()?;
            let kind = matches!(kind, "Sleep" | "Wake" | "DarkWake" | "Start" | "Shutdown" | "Restart").then_some(kind)?;
            (at >= window.from && at <= window.to).then(|| json!({"at": time::utc(at), "kind": kind}))
        })
        .collect()
}

/// `2026-10-24 23:10:05 +0100`.
fn pmset_time(stamp: &str) -> Option<i64> {
    let (date, rest) = stamp.split_once(' ')?;
    let (clock, offset) = rest.trim().split_once(' ')?;
    if offset.len() != 5 {
        return None;
    }
    time::parse_iso(&format!("{date}T{clock}{}:{}", &offset[..3], &offset[3..]))
}

fn disk_free(path: &Path) -> Option<u64> {
    use std::os::unix::ffi::OsStrExt;
    let path = std::ffi::CString::new(path.as_os_str().as_bytes()).ok()?;
    let mut stat: libc::statvfs = unsafe { std::mem::zeroed() };
    (unsafe { libc::statvfs(path.as_ptr(), &mut stat) } == 0).then(|| stat.f_bavail as u64 * stat.f_frsize as u64)
}

fn incident(home: &Path) -> Value {
    let Some(record) = std::fs::read_to_string(home.join("machine-incident.json")).ok().and_then(|t| serde_json::from_str::<Value>(&t).ok()) else {
        return Value::Null;
    };
    let incident = &record["incident"];
    if !incident.is_object() {
        return Value::Null;
    }
    let cause = incident["cause"].as_str().filter(|c| matches!(*c, "disk" | "memory" | "load" | "declared"));
    json!({
        "since": incident["since"].as_i64().map(time::utc),
        "cause": cause,
        "cards": incident["cards"].as_u64(),
        "nodes_shown": incident["nodes_shown"].as_u64(),
        "healthy": incident["healthy"].as_bool(),
    })
}

pub(crate) const SAMPLE_COLUMNS: &[&str] = &[
    "at", "memory_free_percent", "load_1m", "cpus", "health", "cpu_busy", "working", "memory_pressure", "compressor_share", "margin_gb",
    "compressor_pages", "uncompressed_pages", "stored_pages", "swap_used_mb", "swapins_per_s", "swapouts_per_s", "node_cost_gb",
];

/// The machine samples of the window: only the known columns, only numbers, booleans and the words
/// of the code.
fn samples(host: &dyn Host, window: Window, redactor: &mut Redactor) -> Section {
    let dir = host.home().join("machine-samples");
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return Section::empty("machine-samples.csv", tr!("no samples on this Mac", "no hay muestras en este Mac"));
    };
    let mut files: Vec<PathBuf> = entries.flatten().map(|e| e.path()).filter(|p| p.extension().is_some_and(|e| e == "csv")).collect();
    files.sort();
    let mut out = SAMPLE_COLUMNS.join(",") + "\n";
    let mut rows = 0;
    for file in files {
        let Ok(text) = std::fs::read_to_string(&file) else { continue };
        let mut lines = text.lines();
        let Some(header) = lines.next() else { continue };
        let columns: Vec<&str> = header.split(',').collect();
        for line in lines {
            let fields: Vec<&str> = line.split(',').collect();
            let get = |name: &str| columns.iter().position(|c| *c == name).and_then(|i| fields.get(i).copied()).unwrap_or("");
            let Ok(at) = get("at").parse::<i64>() else { continue };
            if at < window.from || at > window.to {
                continue;
            }
            let row: Vec<String> = SAMPLE_COLUMNS
                .iter()
                .map(|name| {
                    let value = get(name);
                    if *name == "at" {
                        time::utc(at)
                    } else if value.is_empty() || is_number(value) || value == "true" || value == "false" {
                        value.to_owned()
                    } else {
                        redactor.word(value)
                    }
                })
                .collect();
            out.push_str(&row.join(","));
            out.push('\n');
            rows += 1;
        }
    }
    if rows == 0 {
        return Section::empty("machine-samples.csv", tr!("no samples in the window", "no hay muestras en ese periodo"));
    }
    Section::ok("machine-samples.csv", out)
}

fn doctor(host: &dyn Host) -> Section {
    let Some(checks) = host.doctor() else {
        return Section::empty("doctor.json", tr!("the checks could not run", "las comprobaciones no pudieron ejecutarse"));
    };
    let known: BTreeSet<&str> = crate::doctor::check_ids().iter().copied().collect();
    let checks: Vec<Value> = checks
        .into_iter()
        .filter(|(id, status)| known.contains(id.as_str()) && matches!(status.as_str(), "pass" | "warn" | "fail"))
        .map(|(id, status)| json!({"id": id, "status": status}))
        .collect();
    Section::ok("doctor.json", pretty(&json!({"checks": checks})))
}

fn daemon_log_section(host: &dyn Host, window: Window, catalog: &Catalog, redactor: &mut Redactor, omitted: &mut Omitted) -> (Section, bool) {
    let read = match daemon_log::read_window(host.home(), window.from, daemon_log::READ_CAP) {
        Ok(read) => read,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return (Section::empty("daemon.log", tr!("there is no alondrad.log on this Mac", "no hay alondrad.log en este Mac")), false),
        Err(error) => {
            let why = error.kind();
            return (Section::empty("daemon.log", trf!("alondrad.log could not be read ({why})", "alondrad.log no se pudo leer ({why})")), false);
        }
    };
    let redacted = daemon_log::redact(&read.lines, catalog, redactor);
    *omitted.entry("daemon_log_lines_without_template").or_default() += redacted.without_template;
    if redacted.text.is_empty() {
        return (Section::empty("daemon.log", tr!("no lines in the window", "no hay líneas en ese periodo")), read.capped);
    }
    (Section::ok("daemon.log", redacted.text.join("\n") + "\n"), read.capped)
}

/// `domain.db` read only, also while the daemon writes it; a database of a newer schema is not read.
pub fn open_domain(home: &Path) -> Result<Connection, String> {
    let path = home.join("domain.db");
    if !path.exists() {
        return Err(tr!("there is no domain.db on this Mac", "no hay domain.db en este Mac").into());
    }
    let open = |uri: String| Connection::open_with_flags(uri, OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_URI | OpenFlags::SQLITE_OPEN_NO_MUTEX);
    let file = url::Url::from_file_path(&path).map_err(|()| tr!("domain.db has a path SQLite cannot open", "domain.db tiene una ruta que SQLite no puede abrir").to_owned())?;
    let db = open(format!("{file}?mode=ro"))
        .or_else(|_| open(format!("{file}?immutable=1")))
        .map_err(|error| {
            let why = sqlite_reason(&error);
            trf!("domain.db could not be opened ({why})", "domain.db no se pudo abrir ({why})")
        })?;
    let _ = db.busy_timeout(std::time::Duration::from_secs(5));
    let version: i64 = db.query_row("PRAGMA user_version", [], |r| r.get(0)).map_err(|e| {
        let why = sqlite_reason(&e);
        trf!("domain.db could not be read ({why})", "domain.db no se pudo leer ({why})")
    })?;
    if version > SCHEMA_VERSION {
        return Err(trf!(
            "domain.db is of a newer version ({version}) than this alondra reads ({SCHEMA_VERSION})",
            "domain.db es de una versión más nueva ({version}) que la que lee este alondra ({SCHEMA_VERSION})"
        ));
    }
    Ok(db)
}

const SCHEMA_VERSION: i64 = 5;

fn sqlite_reason(error: &rusqlite::Error) -> String {
    match error {
        rusqlite::Error::SqliteFailure(failure, _) => format!("{:?}", failure.code),
        _ => "not a database".into(),
    }
}

/// The tasks the report is about: every task whose state changed in the window and every task that
/// has not ended.
fn touched_tasks(db: &Connection, window: Window) -> Vec<String> {
    let mut tasks = BTreeSet::new();
    if let Ok(mut changed) = db.prepare("SELECT DISTINCT h.task_id FROM task_history h JOIN command_audit a ON a.revision = h.revision WHERE a.at >= ?1 AND a.at <= ?2")
        && let Ok(rows) = changed.query_map(params![window.from, window.to], |r| r.get::<_, String>(0))
    {
        tasks.extend(rows.flatten());
    }
    if let Ok(mut open) = db.prepare("SELECT id FROM tasks WHERE state NOT IN ('done','cancelled') AND archived = 0")
        && let Ok(rows) = open.query_map([], |r| r.get::<_, String>(0))
    {
        tasks.extend(rows.flatten());
    }
    tasks.into_iter().collect()
}

/// The event kinds of the lifecycle journal the report knows; any other one is left out.
const EVENT_KINDS: &[&str] = &[
    "admit", "runtime", "ask", "answer", "decide", "move_card", "plan_approved", "block_delivery", "delivery_unblocked", "present", "accept",
    "merge_observed", "hand_back", "wait_for_dependencies", "dependencies_resolved", "tell", "pause", "resume", "finish", "drain_observed",
    "post_merge_declared", "post_merge_ended", "retire_released_execution", "execution_released", "reopen", "hold_for_plan_review",
    "plan_review_settled", "doubt_handoff", "settle_handoff", "resend_untaken", "resend_doubtful", "discard", "await_ended", "request_control",
    "compact",
];

const FACT_KINDS: &[&str] = &[
    "ask", "resource_available", "turn_started", "input_consumed", "input_dropped", "work_started", "work_ended", "continuation_consumed",
    "turn_ended", "connection_lost", "resource_released", "callback_settled", "callback_aborted", "answered_by_policy", "effect_accepted",
    "effect_observed", "interrupt_accepted", "notification", "host_ended",
];

/// Fields of an event that hold text a person wrote or a name they chose: never in the report.
const TEXT_FIELDS: &[&str] = &["answers", "message", "text", "model", "server", "prompt", "body", "cwd", "payload"];

fn lifecycle(db: &Connection, tasks: &[String], redactor: &mut Redactor, omitted: &mut Omitted) -> Section {
    let Ok(mut query) = db.prepare("SELECT revision, event FROM lifecycle_events WHERE task_id = ?1 ORDER BY revision") else {
        return Section::empty("lifecycle.jsonl", tr!("domain.db has no lifecycle journal", "domain.db no tiene diario de ciclo de vida"));
    };
    let mut rows = Vec::new();
    for task in tasks {
        let Ok(events) = query.query_map([task], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?))) else { continue };
        for (revision, event) in events.flatten() {
            match project_event(&event, redactor) {
                Some(mut projected) => {
                    projected.insert("task".into(), Value::from(redactor.id(Kind::Task, task)));
                    projected.insert("revision".into(), Value::from(revision));
                    rows.push(Value::Object(projected));
                }
                None => *omitted.entry("lifecycle_events_not_decoded").or_default() += 1,
            }
        }
    }
    if rows.is_empty() {
        return Section::empty("lifecycle.jsonl", tr!("no lifecycle events for the tasks of the window", "no hay eventos de ciclo de vida de las tareas de ese periodo"));
    }
    Section::ok("lifecycle.jsonl", jsonl(&rows))
}

/// An event as its kind, its ids as pseudonyms and its typed reasons; `None` when its kind, or the
/// kind of the fact it carries, is not one the report knows.
fn project_event(event: &str, redactor: &mut Redactor) -> Option<Map<String, Value>> {
    let event: Value = serde_json::from_str(event).ok()?;
    let kind = event["kind"].as_str().filter(|k| EVENT_KINDS.contains(k))?;
    let mut out = Map::new();
    out.insert("event".into(), Value::from(kind));
    if kind == "runtime" {
        let fact = &event["data"]["fact"];
        let fact_kind = fact["kind"].as_str().filter(|k| FACT_KINDS.contains(k))?;
        out.insert("fact".into(), Value::from(fact_kind));
        out.insert("generation".into(), event["data"]["stamp"]["generation"].as_u64().map_or(Value::Null, Value::from));
        out.insert("cursor".into(), project(&event["data"]["cursor"], redactor));
        out.insert("node".into(), project_id(&event["data"]["stamp"]["node"], Kind::Node, redactor));
        out.insert("instance".into(), project(&event["data"]["stamp"]["instance"], redactor));
        out.insert("data".into(), project(&fact["data"], redactor));
    } else {
        out.insert("data".into(), project(&event["data"], redactor));
    }
    Some(out)
}

fn project_id(value: &Value, kind: Kind, redactor: &mut Redactor) -> Value {
    value.as_str().map_or(Value::Null, |id| Value::from(redactor.id(kind, id)))
}

/// Numbers and booleans as they are, `kind` tags as words of the code, every other string as an id
/// (its `:`-separated parts redacted one by one), and the text fields left out.
fn project(value: &Value, redactor: &mut Redactor) -> Value {
    match value {
        Value::Null | Value::Bool(_) | Value::Number(_) => value.clone(),
        Value::String(text) => Value::from(id_parts(text, redactor)),
        Value::Array(items) => Value::Array(items.iter().map(|item| project(item, redactor)).collect()),
        Value::Object(fields) => Value::Object(
            fields
                .iter()
                .filter(|(key, _)| !TEXT_FIELDS.contains(&key.as_str()))
                .map(|(key, value)| {
                    let projected = match (key.as_str(), value) {
                        ("kind" | "status" | "phase" | "outcome" | "owner", Value::String(word)) => Value::from(redactor.word(word)),
                        _ => project(value, redactor),
                    };
                    let field = if is_field_name(key) { key.clone() } else { redactor.word(key) };
                    (field, projected)
                })
                .collect(),
        ),
    }
}

/// A serde field name of the code (`unheard_card`); a key of any other shape may be data.
fn is_field_name(key: &str) -> bool {
    !key.is_empty() && key.len() <= 40 && key.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
}

/// `input:t-fix-invoice:16111` → `input:task-3:16111`.
fn id_parts(text: &str, redactor: &mut Redactor) -> String {
    text.split(':')
        .map(|part| {
            if is_number(part) || is_random_id(part) || redactor.vocabulary.contains(part) {
                part.to_owned()
            } else {
                redactor.value(part, Kind::Value)
            }
        })
        .collect::<Vec<_>>()
        .join(":")
}

fn tasks_section(db: &Connection, tasks: &[String], window: Window, redactor: &mut Redactor) -> Section {
    let mut rows = Vec::new();
    for task in tasks {
        let Ok((request, agent, state, started, archived, review_prs, pr)) = db.query_row(
            "SELECT request_id, agent_id, state, started, archived, review_prs, pr_id FROM tasks WHERE id = ?1",
            [task],
            |r| Ok((r.get::<_, String>(0)?, r.get::<_, Option<String>>(1)?, r.get::<_, String>(2)?, r.get::<_, bool>(3)?, r.get::<_, bool>(4)?, r.get::<_, bool>(5)?, r.get::<_, Option<String>>(6)?)),
        ) else {
            continue;
        };
        let history = rows_of(db, "SELECT a.at, h.state FROM task_history h JOIN command_audit a ON a.revision = h.revision WHERE h.task_id = ?1 AND a.at >= ?2 ORDER BY h.revision", params![task, window.from], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?)))
            .into_iter()
            .map(|(at, state)| json!({"at": time::utc(at), "state": redactor.word(&state)}))
            .collect::<Vec<_>>();
        let messages = count(db, "SELECT COUNT(*) FROM domain_messages WHERE task_id = ?1", task);
        let deliveries = rows_of(db, "SELECT d.state, d.reason FROM delivery_attempts d JOIN domain_messages m ON m.id = d.message_id WHERE m.task_id = ?1 ORDER BY d.id", params![task], |r| Ok((r.get::<_, String>(0)?, r.get::<_, Option<String>>(1)?)))
            .into_iter()
            .map(|(state, reason)| json!({"state": redactor.word(&state), "reason": reason.map(|r| redactor.word(&r))}))
            .collect::<Vec<_>>();
        let results = rows_of(db, "SELECT is_final, acceptance_valid, withdrawn FROM interactions WHERE task_id = ?1 AND kind = 'delivery' ORDER BY ordering", params![task], |r| Ok((r.get::<_, Option<bool>>(0)?, r.get::<_, Option<bool>>(1)?, r.get::<_, bool>(2)?)))
            .into_iter()
            .map(|(is_final, accepted, withdrawn)| json!({"final": is_final, "accepted": accepted, "withdrawn": withdrawn}))
            .collect::<Vec<_>>();
        let questions = rows_of(db, "SELECT kind, blocking, recipient_kind FROM interactions WHERE task_id = ?1 AND kind IN ('question','action_request') AND withdrawn = 0 AND answer IS NULL", params![task], |r| Ok((r.get::<_, String>(0)?, r.get::<_, bool>(1)?, r.get::<_, String>(2)?)))
            .into_iter()
            .map(|(kind, blocking, recipient)| json!({"kind": redactor.word(&kind), "blocking": blocking, "for": redactor.word(&recipient)}))
            .collect::<Vec<_>>();
        let executions = rows_of(db, "SELECT generation, node_id, instance_id, host_id, backend FROM lifecycle_executions WHERE task_id = ?1 ORDER BY generation", params![task], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?, r.get::<_, String>(2)?, r.get::<_, String>(3)?, r.get::<_, String>(4)?)))
            .into_iter()
            .map(|(generation, node, instance, host_id, backend)| json!({
                "generation": generation,
                "node": redactor.id(Kind::Node, &node),
                "instance": redactor.id(Kind::Value, &instance),
                "host": redactor.word(&host_id),
                "backend": redactor.word(&backend),
            }))
            .collect::<Vec<_>>();
        let pr_state = pr.and_then(|pr| db.query_row("SELECT state FROM pull_requests WHERE id = ?1", [pr], |r| r.get::<_, String>(0)).ok());
        let closing = db
            .query_row("SELECT since, failures FROM task_closings WHERE task_id = ?1", [task], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?)))
            .ok()
            .map(|(since, failures)| json!({"since": time::utc(since), "failures": failures}));
        let model = db.query_row("SELECT model FROM task_models WHERE task_id = ?1", [task], |r| r.get::<_, String>(0)).ok();
        rows.push(json!({
            "task": redactor.id(Kind::Task, task),
            "request": redactor.id(Kind::Request, &request),
            "node": agent.map(|a| redactor.id(Kind::Node, &a)),
            "state": redactor.word(&state),
            "started": started,
            "archived": archived,
            "reviews_prs": review_prs,
            "model": model.map(|m| redactor.word(&m)),
            "pull_request": pr_state.map(|s| redactor.word(&s)),
            "states_in_window": history,
            "messages": messages,
            "message_deliveries": deliveries,
            "results": results,
            "open_questions": questions,
            "executions": executions,
            "closing": closing,
        }));
    }
    if rows.is_empty() {
        return Section::empty("tasks.jsonl", tr!("no tasks in the window", "no hay tareas en ese periodo"));
    }
    Section::ok("tasks.jsonl", jsonl(&rows))
}

fn rows_of<T>(db: &Connection, sql: &str, params: impl rusqlite::Params, map: impl FnMut(&rusqlite::Row) -> rusqlite::Result<T>) -> Vec<T> {
    let Ok(mut query) = db.prepare(sql) else { return Vec::new() };
    query.query_map(params, map).map(|rows| rows.flatten().collect()).unwrap_or_default()
}

fn count(db: &Connection, sql: &str, task: &str) -> Option<i64> {
    db.query_row(sql, [task], |r| r.get(0)).ok()
}

fn nodes(db: &Connection, tasks: &[String], home: &Path, redactor: &mut Redactor) -> Section {
    let mut rows = Vec::new();
    let mut seen = BTreeSet::new();
    for task in tasks {
        let executions = rows_of(db, "SELECT node_id, generation, instance_id FROM lifecycle_executions WHERE task_id = ?1 ORDER BY generation", params![task], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?, r.get::<_, String>(2)?)));
        let current: Option<String> = db.query_row("SELECT agent_id FROM tasks WHERE id = ?1", [task], |r| r.get(0)).ok().flatten();
        let mut agents: Vec<(String, Option<i64>, Option<String>)> = executions.into_iter().map(|(n, g, i)| (n, Some(g), Some(i))).collect();
        if let Some(current) = current
            && !agents.iter().any(|(n, _, _)| *n == current)
        {
            agents.push((current, None, None));
        }
        for (agent, generation, instance) in agents {
            if !seen.insert((agent.clone(), generation)) {
                continue;
            }
            let Ok((phase, since)) = db.query_row("SELECT phase, since FROM agents WHERE id = ?1", [&agent], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?))) else {
                continue;
            };
            let phase: Value = serde_json::from_str(&phase).unwrap_or(Value::Null);
            let options = instance.as_ref().map(|i| launch_options(home, i, redactor));
            let ended = instance.as_ref().and_then(|i| last_session_end(home, i, redactor));
            let api_failures = db
                .query_row("SELECT failures, first_at, last_at FROM api_failures WHERE agent_id = ?1", [&agent], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?, r.get::<_, i64>(2)?)))
                .ok()
                .map(|(failures, first, last)| json!({"failures": failures, "first": time::utc(first), "last": time::utc(last)}));
            rows.push(json!({
                "node": redactor.id(Kind::Node, &agent),
                "task": redactor.id(Kind::Task, task),
                "generation": generation,
                "instance": instance.map(|i| redactor.id(Kind::Value, &i)),
                "phase": phase_projection(&phase, redactor),
                "since": (since > 0).then(|| time::utc(since)),
                "model": options.as_ref().map(|o| o["model"].clone()),
                "effort": options.as_ref().map(|o| o["effort"].clone()),
                "last_session_end": ended,
                "api_failures": api_failures,
            }));
        }
    }
    if rows.is_empty() {
        return Section::empty("nodes.jsonl", tr!("no nodes for the tasks of the window", "no hay nodos de las tareas de ese periodo"));
    }
    Section::ok("nodes.jsonl", jsonl(&rows))
}

/// A node's phase as its label, the times it carries and its flags: never the command it waits on or
/// a reason someone wrote.
fn phase_projection(phase: &Value, redactor: &mut Redactor) -> Value {
    let Some(kind) = phase["kind"].as_str() else { return Value::Null };
    let mut out = Map::new();
    out.insert("kind".into(), Value::from(redactor.word(kind)));
    let mut stack = vec![phase];
    while let Some(value) = stack.pop() {
        let Some(fields) = value.as_object() else { continue };
        for (key, value) in fields {
            match value {
                Value::Bool(_) => {
                    out.insert(redactor.word(key), value.clone());
                }
                Value::Number(n) if key.contains("since") || key == "at" => {
                    out.insert(redactor.word(key), n.as_i64().map_or(Value::Null, |ms| Value::from(time::utc(ms))));
                }
                Value::Object(_) if matches!(key.as_str(), "reason" | "pause" | "cleanup") => {
                    if let Some(word) = value["kind"].as_str() {
                        out.insert(format!("{}_kind", redactor.word(key)), Value::from(redactor.word(word)));
                    }
                }
                Value::String(word) if matches!(key.as_str(), "cleanup") => {
                    out.insert(redactor.word(key), Value::from(redactor.word(word)));
                }
                _ => {}
            }
        }
    }
    Value::Object(out)
}

fn launch_options(home: &Path, instance: &str, redactor: &mut Redactor) -> Value {
    if !is_instance_dir(instance) {
        return json!({"model": null, "effort": null});
    }
    let options = std::fs::read_to_string(home.join("sdk").join(instance).join("launch-options.json")).ok().and_then(|t| serde_json::from_str::<Value>(&t).ok()).unwrap_or(Value::Null);
    let word = |key: &str, redactor: &mut Redactor| options[key].as_str().map_or(Value::Null, |w| Value::from(redactor.word(w)));
    json!({
        "model": word("model", redactor),
        "effort": word("effort", redactor),
        "disallowed_tools": options["disallowed_tools"].as_array().map(Vec::len),
    })
}

fn is_instance_dir(name: &str) -> bool {
    is_random_id(name) && !name.contains('/')
}

/// How the last Claude session of an SDK instance ended: the reason the SDK gave for its last result
/// and the exit of its process, if any.
fn last_session_end(home: &Path, instance: &str, redactor: &mut Redactor) -> Option<Value> {
    if !is_instance_dir(instance) {
        return None;
    }
    let frames = outbox_frames(&home.join("sdk").join(instance).join("outbox")).ok()?;
    let mut reason = None;
    let mut exit = None;
    for frame in &frames.frames {
        let observation = &frame["body"]["observation"];
        match observation["type"].as_str() {
            Some("sdk") if observation["kind"] == "result" => {
                let payload = &observation["payload"];
                reason = Some(json!({
                    "terminal_reason": payload["terminal_reason"].as_str().map(|w| redactor.word(w)),
                    "subtype": payload["subtype"].as_str().map(|w| redactor.word(w)),
                    "is_error": payload["is_error"].as_bool(),
                }));
            }
            Some("process_exited") => exit = Some(json!({"exit_code": observation["exit_code"].as_i64(), "signal": observation["signal"].as_i64().map(Value::from).or_else(|| observation["signal"].as_str().map(|s| Value::from(redactor.word(s))))})),
            _ => {}
        }
    }
    (reason.is_some() || exit.is_some()).then(|| json!({"result": reason, "process": exit}))
}

pub struct Outbox {
    pub frames: Vec<Value>,
    pub undecoded: u64,
}

const MAX_FRAME: usize = 1_048_576;
const MAX_OUTBOX_READ: u64 = 64 * 1024 * 1024;

/// The frames of an SDK host's outbox: a 4-byte big-endian length, then that many bytes of JSON. A
/// truncated tail is how a host stops writing, not an error.
pub fn outbox_frames(path: &Path) -> std::io::Result<Outbox> {
    use std::io::Read;
    let file = std::fs::File::open(path)?;
    let mut reader = std::io::BufReader::new(file.take(MAX_OUTBOX_READ));
    let mut frames = Vec::new();
    let mut undecoded = 0;
    loop {
        let mut header = [0u8; 4];
        if reader.read_exact(&mut header).is_err() {
            break;
        }
        let length = u32::from_be_bytes(header) as usize;
        if length == 0 || length > MAX_FRAME {
            undecoded += 1;
            break;
        }
        let mut payload = vec![0u8; length];
        if reader.read_exact(&mut payload).is_err() {
            break;
        }
        match serde_json::from_slice::<Value>(&payload) {
            Ok(frame) => frames.push(frame),
            Err(_) => undecoded += 1,
        }
    }
    Ok(Outbox { frames, undecoded })
}

const BODY_TYPES: &[&str] = &["hello", "command", "receipt", "event", "challenge", "attach", "attached", "acknowledge"];
const OBSERVATION_TYPES: &[&str] = &["sdk", "decision_requested", "decision_settled", "process_exited"];

/// Each SDK host instance written in the window: the type of each frame of its outbox, with the
/// protocol version and how it ended. Every body is dropped.
fn sdk_host(host: &dyn Host, window: Window, daemon_running: bool, redactor: &mut Redactor, omitted: &mut Omitted) -> Section {
    let root = host.home().join("sdk");
    let Ok(entries) = std::fs::read_dir(&root) else {
        return Section::empty("sdk-host.jsonl", tr!("no SDK hosts on this Mac", "no hay hosts del SDK en este Mac"));
    };
    let mut instances: Vec<(i64, String)> = entries
        .flatten()
        .filter_map(|entry| {
            let name = entry.file_name().to_string_lossy().into_owned();
            let modified = entry.path().join("outbox").metadata().and_then(|m| m.modified()).ok()?;
            let at = modified.duration_since(std::time::UNIX_EPOCH).ok()?.as_millis() as i64;
            (at >= window.from).then_some((at, name))
        })
        .collect();
    instances.sort();
    let mut rows = Vec::new();
    for (written, name) in instances {
        if !is_instance_dir(&name) {
            *omitted.entry("sdk_host_instances_with_unknown_names").or_default() += 1;
            continue;
        }
        let Ok(outbox) = outbox_frames(&root.join(&name).join("outbox")) else { continue };
        *omitted.entry("sdk_host_frames_not_decoded").or_default() += outbox.undecoded;
        let mut frames = Vec::new();
        let mut versions = BTreeSet::new();
        for frame in &outbox.frames {
            let body = &frame["body"];
            let Some(kind) = body["type"].as_str().filter(|t| BODY_TYPES.contains(t)) else {
                *omitted.entry("sdk_host_frames_of_unknown_type").or_default() += 1;
                continue;
            };
            if let Some(version) = frame["protocol_version"].as_u64() {
                versions.insert(version);
            }
            frames.push(frame_projection(kind, body, redactor));
        }
        rows.push(json!({
            "instance": name,
            "last_written": time::utc(written),
            "protocol_versions": versions,
            "frames": frames,
        }));
    }
    if rows.is_empty() {
        return Section::empty("sdk-host.jsonl", tr!("no SDK host wrote in the window", "ningún host del SDK escribió en ese periodo"));
    }
    if !daemon_running {
        rows.push(json!({"diagnostics": "not available: the daemon is not running"}));
    }
    Section::ok("sdk-host.jsonl", jsonl(&rows))
}

fn frame_projection(kind: &str, body: &Value, redactor: &mut Redactor) -> Value {
    let word = |value: &Value, redactor: &mut Redactor| value.as_str().map_or(Value::Null, |w| Value::from(redactor.word(w)));
    match kind {
        "command" => json!({"type": kind, "action": word(&body["action"]["type"], redactor)}),
        "receipt" => json!({"type": kind, "status": word(&body["outcome"]["status"], redactor), "phase": word(&body["outcome"]["phase"], redactor), "code": word(&body["outcome"]["code"], redactor)}),
        "event" => {
            let observation = &body["observation"];
            let observation_type = observation["type"].as_str().filter(|t| OBSERVATION_TYPES.contains(t));
            let detail = match observation_type {
                Some("sdk") => word(&observation["kind"], redactor),
                Some("decision_requested") => word(&observation["kind"], redactor),
                Some("decision_settled") => word(&observation["outcome"], redactor),
                Some("process_exited") => json!({"exit_code": observation["exit_code"].as_i64(), "signal": observation["signal"].as_i64()}),
                _ => Value::Null,
            };
            json!({"type": kind, "cursor": body["cursor"].as_u64(), "observation": observation_type, "detail": detail})
        }
        _ => json!({"type": kind}),
    }
}
