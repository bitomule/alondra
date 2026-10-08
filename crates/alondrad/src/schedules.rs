//! Durable, local-time schedules. A due run becomes one request and one task without
//! asking the orchestrator to interpret it. Claiming first prevents duplicate launches.
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use alondra_core::now_millis;
use alondra_core::tasks::{NewTask, Start, TaskOptions};
use alondra_core::Event;
use axum::extract::{Path as RoutePath, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::hub::Hub;

#[derive(Clone, Serialize, Deserialize)]
pub struct Schedule {
    pub id: String,
    pub title: String,
    pub text: String,
    pub project: Option<String>,
    #[serde(default)]
    pub options: std::collections::BTreeMap<String, alondra_core::options::Choice>,
    #[serde(default)]
    pub disallowed_tools: Vec<String>,
    pub frequency: Frequency,
    pub hour: u8,
    pub minute: u8,
    /// Sunday is zero. Required for weekly schedules.
    pub weekday: Option<u8>,
    #[serde(default)]
    pub interval: Option<u32>,
    #[serde(default)]
    pub start_at: Option<i64>,
    pub enabled: bool,
    pub next_run: Option<i64>,
    pub last_run: Option<i64>,
    pub last_request: Option<String>,
    pub last_error: Option<String>,
    pub status: String,
    pub revision: i64,
}

#[derive(Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Frequency { Daily, Weekly, EveryHours, EveryDays, EveryMonths, Once }

#[derive(Deserialize)]
pub struct Draft {
    title: String,
    text: String,
    project: Option<String>,
    #[serde(default)]
    options: std::collections::BTreeMap<String, alondra_core::options::Choice>,
    #[serde(default)]
    disallowed_tools: Vec<String>,
    frequency: Frequency,
    hour: u8,
    minute: u8,
    weekday: Option<u8>,
    interval: Option<u32>,
    start_at: Option<i64>,
    #[serde(default = "yes")]
    enabled: bool,
    /// Updates use optimistic concurrency so two clients cannot silently overwrite edits.
    #[serde(default)]
    revision: i64,
}
fn yes() -> bool { true }

impl Draft {
    fn validate(&self, hub: &Hub) -> Result<(), String> {
        if self.title.trim().is_empty() || self.title.len() > 120 { return Err("title must be 1–120 characters".into()); }
        if self.text.trim().is_empty() || self.text.len() > 20_000 { return Err("mission must be 1–20,000 characters".into()); }
        if self.hour > 23 || self.minute > 59 { return Err("time must be a valid local hour and minute".into()); }
        if self.frequency == Frequency::Weekly && !matches!(self.weekday, Some(0..=6)) { return Err("weekly schedule needs weekday 0–6".into()); }
        if matches!(self.frequency, Frequency::EveryHours | Frequency::EveryDays | Frequency::EveryMonths)
            && !matches!(self.interval, Some(1..=365)) { return Err("custom interval must be 1–365".into()); }
        if matches!(self.frequency, Frequency::EveryHours | Frequency::EveryDays | Frequency::EveryMonths | Frequency::Once)
            && self.start_at.is_none_or(|at| at < 0 || at > 4_102_444_800_000) { return Err("start date must be before 2100".into()); }
        if self.frequency == Frequency::Once && self.enabled && self.start_at.is_some_and(|at| at <= now_millis()) {
            return Err("one-time date must be in the future".into());
        }
        if self.project.as_ref().is_some_and(|p| p.trim().is_empty()) { return Err("project cannot be empty".into()); }
        if self.disallowed_tools.len() > 100 || self.disallowed_tools.iter().any(|p| p.is_empty() || p.len() > 200 || p.chars().any(char::is_whitespace) || p.starts_with('-')) {
            return Err("disallowed tools must be at most 100 nonempty tool patterns without spaces or flags".into());
        }
        let loaded = crate::options::load(hub);
        loaded.file.resolve(self.project.as_deref(), &self.options).map_err(|e| e.to_string())?;
        let root = loaded.file.projects_root(&crate::options::user_home());
        if let Some(project) = self.project.as_deref() {
            if !alondra_core::options::projects(&root).iter().any(|(name, _)| name == project) {
                return Err("project must be one of the available project folders".into());
            }
        } else if !root.is_dir() {
            return Err("projects directory does not exist".into());
        }
        Ok(())
    }

    fn into_schedule(self, id: String, revision: i64) -> Schedule {
        let next_run = self.enabled.then(|| next_after(self.frequency, self.hour, self.minute, self.weekday, self.interval, self.start_at, now_millis())).flatten();
        Schedule { id, title: self.title.trim().into(), text: self.text.trim().into(), project: self.project,
            options: self.options, disallowed_tools: self.disallowed_tools, frequency: self.frequency, hour: self.hour, minute: self.minute,
            weekday: self.weekday, interval: self.interval, start_at: self.start_at, enabled: self.enabled, next_run, last_run: None,
            last_request: None, last_error: None, status: "never_run".into(), revision }
    }
}

/// Uses the Mac's local calendar, including its daylight-saving rules. Search days rather
/// than adding 24 hours, which would drift across a clock change.
pub(crate) fn next_after(frequency: Frequency, hour: u8, minute: u8, weekday: Option<u8>, interval: Option<u32>, start_at: Option<i64>, after: i64) -> Option<i64> {
    if frequency == Frequency::Once { return start_at.filter(|at| *at > after); }
    if frequency == Frequency::EveryHours {
        let start = start_at?;
        let step = i64::from(interval?) * 3_600_000;
        return Some(if start > after { start } else { start + (after - start).div_euclid(step).saturating_add(1) * step });
    }
    let seconds = after.div_euclid(1000) as libc::time_t;
    let mut day = unsafe { std::mem::zeroed::<libc::tm>() };
    unsafe { libc::localtime_r(&seconds, &mut day) };
    if matches!(frequency, Frequency::EveryDays | Frequency::EveryMonths) {
        let start = start_at?;
        if start > after { return Some(start); }
        let mut anchor = unsafe { std::mem::zeroed::<libc::tm>() };
        let anchor_seconds = start.div_euclid(1000) as libc::time_t;
        unsafe { libc::localtime_r(&anchor_seconds, &mut anchor) };
        let stride = interval? as i32;
        let difference = if frequency == Frequency::EveryDays {
            // Local calendar dates, not fixed 24-hour periods: DST does not move the wall time.
            let mut a = anchor; a.tm_hour = 12; a.tm_min = 0; a.tm_sec = 0; a.tm_isdst = -1;
            let mut b = day; b.tm_hour = 12; b.tm_min = 0; b.tm_sec = 0; b.tm_isdst = -1;
            ((unsafe { libc::mktime(&mut b) - libc::mktime(&mut a) } + 43_200) / 86_400) as i32
        } else { (day.tm_year - anchor.tm_year) * 12 + day.tm_mon - anchor.tm_mon };
        let mut index = (difference.max(0) / stride).max(0);
        loop {
            let mut candidate = anchor;
            if frequency == Frequency::EveryDays { candidate.tm_mday += index * stride; }
            else {
                candidate.tm_mday = 1;
                candidate.tm_mon += index * stride;
                candidate.tm_isdst = -1;
                unsafe { libc::mktime(&mut candidate) };
                let last_day = days_in_month(candidate.tm_year + 1900, candidate.tm_mon + 1);
                candidate.tm_mday = anchor.tm_mday.min(last_day);
            }
            candidate.tm_isdst = -1;
            let at = unsafe { libc::mktime(&mut candidate) } as i64 * 1000 + start.rem_euclid(1000);
            if at > after { return Some(at); }
            index += 1;
        }
    }
    for offset in 0..=8 {
        let mut candidate = day;
        candidate.tm_mday += offset;
        candidate.tm_hour = hour as i32;
        candidate.tm_min = minute as i32;
        candidate.tm_sec = 0;
        candidate.tm_isdst = -1;
        let at = unsafe { libc::mktime(&mut candidate) } as i64 * 1000;
        if at <= after { continue; }
        if frequency == Frequency::Weekly && weekday != Some(candidate.tm_wday as u8) { continue; }
        // A nonexistent spring-forward time is moved by mktime to the next real minute.
        return Some(at);
    }
    unreachable!("one of the next eight local days matches")
}

fn days_in_month(year: i32, month: i32) -> i32 {
    let next_month = if month == 12 { (year + 1, 1) } else { (year, month + 1) };
    let mut first = unsafe { std::mem::zeroed::<libc::tm>() };
    first.tm_year = next_month.0 - 1900;
    first.tm_mon = next_month.1 - 1;
    first.tm_mday = 0;
    first.tm_hour = 12;
    first.tm_isdst = -1;
    unsafe { libc::mktime(&mut first) };
    first.tm_mday
}

pub struct Schedules(Mutex<Connection>);

impl Schedules {
    pub fn open(path: &Path) -> anyhow::Result<Self> {
        let conn = Connection::open(path)?;
        #[cfg(unix)] {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
        }
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "synchronous", "FULL")?;
        conn.execute_batch("CREATE TABLE IF NOT EXISTS schedules (id TEXT PRIMARY KEY, body TEXT NOT NULL);
            CREATE TABLE IF NOT EXISTS local_command_results (
                sender TEXT NOT NULL, request_id TEXT NOT NULL, digest BLOB NOT NULL,
                result TEXT NOT NULL, PRIMARY KEY(sender,request_id));
            CREATE TABLE IF NOT EXISTS schedule_failed_launches (request TEXT PRIMARY KEY);
            CREATE TABLE IF NOT EXISTS schedule_launch_tools (request TEXT PRIMARY KEY, patterns TEXT NOT NULL);")?;
        // A process killed after claiming must never replay that dispatch on restart.
        conn.execute("UPDATE schedules SET body=json_set(body, '$.status', 'interrupted', '$.last_error', 'Alondra stopped while sending this run') WHERE json_extract(body, '$.status')='launching'", [])?;
        // Older versions waited for an orchestrator. Keep their due time and dispatch directly.
        conn.execute("UPDATE schedules SET body=json_set(body, '$.status', 'never_run') WHERE json_extract(body, '$.status')='waiting'", [])?;
        Ok(Self(Mutex::new(conn)))
    }

    fn all(&self) -> anyhow::Result<Vec<Schedule>> {
        let conn = self.0.lock().unwrap();
        let mut stmt = conn.prepare("SELECT body FROM schedules ORDER BY id")?;
        Ok(stmt.query_map([], |r| r.get::<_, String>(0))?.collect::<rusqlite::Result<Vec<_>>>()?
            .into_iter().map(|body| serde_json::from_str(&body)).collect::<Result<Vec<_>, _>>()?)
    }

    #[cfg(test)]
    fn get(&self, id: &str) -> anyhow::Result<Option<Schedule>> {
        let conn = self.0.lock().unwrap();
        let body: Option<String> = conn.query_row("SELECT body FROM schedules WHERE id=?1", [id], |r| r.get(0)).optional()?;
        body.map(|b| serde_json::from_str(&b).map_err(Into::into)).transpose()
    }

    #[cfg(test)]
    fn save(&self, schedule: &Schedule) -> anyhow::Result<()> {
        let conn = self.0.lock().unwrap();
        conn.execute("INSERT INTO schedules(id,body) VALUES(?1,?2)",
            params![schedule.id, serde_json::to_string(schedule)?])?;
        Ok(())
    }

    pub fn tools_for_request(&self, request: &str) -> anyhow::Result<Vec<String>> {
        let conn = self.0.lock().unwrap();
        let patterns: Option<String> = conn.query_row("SELECT patterns FROM schedule_launch_tools WHERE request=?1", [request], |r| r.get(0)).optional()?;
        patterns.map(|p| serde_json::from_str(&p).map_err(Into::into)).transpose().map(|p| p.unwrap_or_default())
    }

    fn remember_tools(&self, request: &str, patterns: &[String]) -> anyhow::Result<()> {
        let conn = self.0.lock().unwrap();
        conn.execute("INSERT INTO schedule_launch_tools(request, patterns) VALUES(?1, ?2)", params![request, serde_json::to_string(patterns)?])?;
        Ok(())
    }

    fn replace(conn: &Connection, id: &str, revision: i64, draft: Draft) -> anyhow::Result<Result<Schedule, &'static str>> {
        let body: Option<String> = conn.query_row("SELECT body FROM schedules WHERE id=?1", [id], |r| r.get(0)).optional()?;
        let Some(body) = body else { return Ok(Err("not_found")); };
        let old: Schedule = serde_json::from_str(&body)?;
        if old.revision != revision || old.status == "launching" { return Ok(Err("conflict")); }
        let mut new = draft.into_schedule(id.into(), revision + 1);
        new.last_run = old.last_run;
        new.last_request = old.last_request;
        new.last_error = old.last_error;
        new.status = if old.status == "waiting" { "never_run".into() } else { old.status };
        conn.execute("UPDATE schedules SET body=?2 WHERE id=?1", params![id, serde_json::to_string(&new)?])?;
        Ok(Ok(new))
    }

    fn claim(&self, id: &str, due_only: bool) -> anyhow::Result<Result<Schedule, &'static str>> {
        let conn = self.0.lock().unwrap();
        let body: Option<String> = conn.query_row("SELECT body FROM schedules WHERE id=?1", [id], |r| r.get(0)).optional()?;
        let Some(body) = body else { return Ok(Err("not_found")); };
        let mut s: Schedule = serde_json::from_str(&body)?;
        if s.status == "launching" { return Ok(Err("already_running")); }
        let now = now_millis();
        if due_only && (!s.enabled || s.next_run.is_none_or(|at| at > now)) { return Ok(Err("not_due")); }
        s.status = "launching".into();
        s.last_error = None;
        s.next_run = s.enabled.then(|| next_after(s.frequency, s.hour, s.minute, s.weekday, s.interval, s.start_at, now)).flatten();
        s.revision += 1;
        conn.execute("UPDATE schedules SET body=?2 WHERE id=?1", params![id, serde_json::to_string(&s)?])?;
        Ok(Ok(s))
    }

    fn finish(&self, id: &str, request: Option<&str>, error: Option<&str>) -> anyhow::Result<()> {
        let conn = self.0.lock().unwrap();
        let body: String = conn.query_row("SELECT body FROM schedules WHERE id=?1", [id], |r| r.get(0))?;
        let mut s: Schedule = serde_json::from_str(&body)?;
        s.last_run = Some(now_millis());
        s.last_request = request.map(str::to_string);
        s.last_error = error.map(str::to_string);
        s.status = if error.is_some() { "failed" } else { "sent" }.into();
        s.revision += 1;
        conn.execute("UPDATE schedules SET body=?2 WHERE id=?1", params![id, serde_json::to_string(&s)?])?;
        Ok(())
    }

    pub fn launch_failed(&self, request: &str, reason: &str) -> anyhow::Result<()> {
        let mut conn = self.0.lock().unwrap();
        let tx = conn.transaction()?;
        tx.execute("INSERT OR IGNORE INTO schedule_failed_launches(request) VALUES(?1)", [request])?;
        tx.execute("UPDATE schedules SET body=json_set(body, '$.status', 'failed', '$.last_error', ?2, '$.revision', json_extract(body, '$.revision') + 1) WHERE json_extract(body, '$.last_request')=?1",
            params![request, reason])?;
        tx.commit()?;
        Ok(())
    }

    pub fn failed_launch(&self, request: &str) -> anyhow::Result<bool> {
        let conn = self.0.lock().unwrap();
        Ok(conn.query_row("SELECT 1 FROM schedule_failed_launches WHERE request=?1", [request], |_| Ok(())).optional()?.is_some())
    }

    fn delete(conn: &Connection, id: &str) -> anyhow::Result<bool> {
        let body: Option<String> = conn.query_row("SELECT body FROM schedules WHERE id=?1", [id], |r| r.get(0)).optional()?;
        if body.as_deref().is_some_and(|b| serde_json::from_str::<Schedule>(b).is_ok_and(|s| s.status == "launching")) { return Ok(false); }
        Ok(conn.execute("DELETE FROM schedules WHERE id=?1", [id])? > 0)
    }
}

fn error(status: StatusCode, message: impl Into<String>) -> Response {
    (status, Json(json!({"error": message.into()}))).into_response()
}

pub async fn list(State(hub): State<Arc<Hub>>) -> Response {
    match hub.schedules.all() {
        Ok(schedules) => {
            let rows: Vec<Value> = schedules.into_iter().map(|s| {
                let mut row = json!(s);
                if let Some(request) = &s.last_request {
                    hub.read(|v| {
                        if v.board.requests.contains_key(request) {
                            row["last_state"] = json!(v.board.request_state(request));
                            row["last_nodes"] = json!(v.board.tasks.values()
                                .filter(|t| &t.request == request)
                                .filter_map(|t| t.assignee.as_ref().map(|a| &a.session_id))
                                .collect::<Vec<_>>());
                        }
                    });
                }
                row
            }).collect();
            Json(json!({"schedules": rows})).into_response()
        },
        Err(e) => error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    }
}

enum Mutation { Create(Draft), Update(String, Draft), Delete(String) }

impl Schedules {
    /// The receipt is authoritative: validation and generated IDs run only for a new command.
    fn mutate(&self, identity: Option<(&str, &str, &[u8; 32])>,
        prepare: impl FnOnce() -> Result<Mutation, Value>) -> anyhow::Result<Value> {
        let mut conn = self.0.lock().unwrap();
        let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        if let Some((sender, id, digest)) = identity {
            if let Some(result) = crate::store::Store::command_result(&tx, sender, id, digest)? {
                return Ok(result);
            }
        }
        let result = match prepare() {
            Err(result) => result,
            Ok(Mutation::Create(draft)) => {
                let schedule = draft.into_schedule(crate::auth::random_hex(16), 1);
                tx.execute("INSERT INTO schedules(id,body) VALUES(?1,?2)", params![schedule.id, serde_json::to_string(&schedule)?])?;
                json!({"status":201,"body":schedule})
            }
            Ok(Mutation::Update(id, draft)) => match Self::replace(&tx, &id, draft.revision, draft)? {
                Ok(schedule) => json!({"status":200,"body":schedule}),
                Err("not_found") => failure(404, "schedule not found"),
                Err(_) => failure(409, "schedule changed elsewhere; refresh before editing"),
            },
            Ok(Mutation::Delete(id)) => if Self::delete(&tx, &id)? {
                json!({"status":200,"body":{"ok":true}})
            } else { failure(409, "schedule not found or a run is being sent") },
        };
        if let Some((sender, id, digest)) = identity {
            crate::store::Store::save_command_result(&tx, sender, id, digest, &result)?;
        }
        tx.commit()?;
        Ok(result)
    }
}

fn failure(status: u16, message: impl Into<String>) -> Value {
    json!({"status":status,"body":{"error":message.into()}})
}
fn validate(hub: &Hub, mutation: Mutation) -> Result<Mutation, Value> {
    match &mutation {
        Mutation::Create(draft) | Mutation::Update(_, draft) =>
            draft.validate(hub).map_err(|e| failure(400, e))?,
        Mutation::Delete(_) => (),
    }
    Ok(mutation)
}
fn response(result: anyhow::Result<Value>) -> Response {
    match result {
        Ok(result) => (StatusCode::from_u16(result["status"].as_u64().unwrap() as u16).unwrap(), Json(result["body"].clone())).into_response(),
        Err(e) => error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    }
}
pub async fn create(State(hub): State<Arc<Hub>>, Json(draft): Json<Draft>) -> Response {
    response(hub.schedules.mutate(None, || validate(&hub, Mutation::Create(draft))))
}
pub async fn update(State(hub): State<Arc<Hub>>, RoutePath(id): RoutePath<String>, Json(draft): Json<Draft>) -> Response {
    response(hub.schedules.mutate(None, || validate(&hub, Mutation::Update(id, draft))))
}
pub async fn delete(State(hub): State<Arc<Hub>>, RoutePath(id): RoutePath<String>) -> Response {
    response(hub.schedules.mutate(None, || Ok(Mutation::Delete(id))))
}

pub(crate) fn atomic_local_path(method: &str, path: &str) -> bool {
    (method == "POST" && path == "/v1/schedules") ||
        (matches!(method, "PUT" | "DELETE") && path.strip_prefix("/v1/schedules/")
            .is_some_and(|id| !id.is_empty() && !id.contains('/')))
}
pub(crate) fn atomic_local_request(hub: &Hub, sender: &str, id: &str, digest: &[u8; 32],
    method: &str, path: &str, body: Option<&Value>) -> anyhow::Result<Value> {
    anyhow::ensure!(atomic_local_path(method, path), "unsupported atomic schedule route");
    hub.schedules.mutate(Some((sender, id, digest)), || {
        let mutation = if method == "DELETE" {
            Mutation::Delete(path.strip_prefix("/v1/schedules/").unwrap().into())
        } else {
            let draft = serde_json::from_value(body.cloned().unwrap_or(Value::Null))
                .map_err(|_| failure(422, "invalid schedule"))?;
            if method == "POST" { Mutation::Create(draft) }
            else { Mutation::Update(path.strip_prefix("/v1/schedules/").unwrap().into(), draft) }
        };
        validate(hub, mutation)
    })
}

async fn dispatch(hub: Arc<Hub>, id: &str, due_only: bool) -> Response {
    let s = match hub.schedules.claim(id, due_only) {
        Ok(Ok(s)) => s,
        Ok(Err(reason)) => return error(StatusCode::CONFLICT, reason),
        Err(e) => return error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    };
    let loaded = crate::options::load(&hub);
    let resolved = match loaded.file.resolve(s.project.as_deref(), &s.options) {
        Ok(options) => options,
        Err(e) => { let _ = hub.schedules.finish(id, None, Some(&e)); return error(StatusCode::BAD_REQUEST, e); }
    };
    let root = loaded.file.projects_root(&crate::options::user_home());
    let cwd = s.project.as_deref().map_or_else(|| root.clone(), |project| root.join(project));
    if !cwd.is_dir() {
        let message = format!("schedule project directory does not exist: {}", cwd.display());
        let _ = hub.schedules.finish(id, None, Some(&message));
        return error(StatusCode::BAD_REQUEST, message);
    }
    let request = format!("r-{}-{}", now_millis(), crate::auth::random_hex(4));
    if let Err(e) = hub.schedules.remember_tools(&request, &s.disallowed_tools) {
        let _ = hub.schedules.finish(id, None, Some(&e.to_string()));
        return error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string());
    }
    let node_name = format!("scheduled/{}-{}", &s.id[..s.id.len().min(8)], crate::auth::random_hex(4));
    let recorded = hub.append_all(|v| Ok::<_, String>(vec![
        Event::RequestSent { request_id: request.clone(), options: resolved.clone(), text: Some(s.text.clone()), attachments: vec![] },
        Event::RequestTitled { request_id: request.clone(), title: s.title.clone() },
        Event::TaskCreated(NewTask {
            id: format!("t-{}", v.seq + 1), request: request.clone(), title: s.title.clone(),
            brief: Some(s.text.clone()), depends_on: vec![],
            options: TaskOptions { auto_merge: resolved.auto_merge, effort: Some(resolved.effort.clone()), project: s.project.clone(), issue: None },
            session_id: None, launched: false, launched_as: None,
            start: Some(Start::NewNode { name: node_name.clone(), cwd: cwd.display().to_string(), model: None }),
        }),
    ]));
    if let Err(e) = recorded {
        let message = e.to_string();
        let _ = hub.schedules.finish(id, None, Some(&message));
        return error(StatusCode::INTERNAL_SERVER_ERROR, message);
    }
    if let Err(e) = hub.schedules.finish(id, Some(&request), None) {
        return error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string());
    }
    (StatusCode::OK, Json(json!({"ok": true, "request_id": request}))).into_response()
}

pub async fn run_now(State(hub): State<Arc<Hub>>, RoutePath(id): RoutePath<String>) -> Response {
    dispatch(hub, &id, false).await
}

pub async fn run(hub: Arc<Hub>) {
    loop {
        if let Ok(all) = hub.schedules.all() {
            for s in all.into_iter().filter(|s| s.enabled && s.next_run.is_some_and(|at| at <= now_millis())) {
                let result = dispatch(hub.clone(), &s.id, true).await;
                if !result.status().is_success() {
                    log!("scheduled task {}: {}", s.id, result.status());
                }
            }
        }
        tokio::time::sleep(Duration::from_secs(15)).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Scheduling validates and later launches inside projects_root. Keep that
    /// directory in the fixture rather than relying on the runner's ~/Projects.
    fn fixture_projects(hub: &Hub) {
        let projects = hub.home.join("projects");
        std::fs::create_dir(&projects).unwrap();
        let options = alondra_core::options::OptionsFile {
            projects_root: Some(projects.to_str().unwrap().to_owned()),
            ..Default::default()
        };
        std::fs::write(alondra_core::options::path_in(&hub.home), toml::to_string(&options).unwrap()).unwrap();
        assert_eq!(crate::options::load(hub).file.projects_root(std::path::Path::new("/unused-home")), projects);
    }

    fn receipt_draft() -> Draft {
        serde_json::from_value(json!({"title":"Review","text":"Review project","frequency":"daily","hour":10,"minute":0,"enabled":false})).unwrap()
    }

    #[test]
    fn schedule_receipts_recover_crud_and_reject_changed_digest() {
        let scratch_directory = crate::test_support::Directory::new();
        let path = scratch_directory.parent().unwrap().join(format!("schedule-receipts-{}.db", crate::auth::random_hex(6)));
        // The previous schema has no receipt table. Opening must preserve its schedules.
        let legacy = Connection::open(&path).unwrap();
        legacy.execute_batch("CREATE TABLE schedules(id TEXT PRIMARY KEY,body TEXT NOT NULL);").unwrap();
        let existing = receipt_draft().into_schedule("legacy".into(), 1);
        legacy.execute("INSERT INTO schedules VALUES(?1,?2)", params![existing.id, serde_json::to_string(&existing).unwrap()]).unwrap();
        drop(legacy);
        let store = Schedules::open(&path).unwrap();
        assert_eq!(store.get("legacy").unwrap().unwrap().title, "Review");
        let created = store.mutate(Some(("phone", "create", &[1;32])), || Ok(Mutation::Create(receipt_draft()))).unwrap();
        assert_eq!(created["status"], 201);
        let id = created["body"]["id"].as_str().unwrap().to_owned();
        drop(store);
        let store = Schedules::open(&path).unwrap();
        assert_eq!(store.mutate(Some(("phone", "create", &[1;32])), || panic!("replay must bypass validation and ID generation")).unwrap(), created);
        assert!(store.mutate(Some(("phone", "create", &[2;32])), || panic!("changed digest must not execute")).is_err());
        assert_eq!(store.all().unwrap().len(), 2);
        let updated = store.mutate(Some(("phone", "update", &[3;32])), || {
            let mut draft = receipt_draft(); draft.revision = 1; draft.title = "Updated".into();
            Ok(Mutation::Update(id.clone(), draft))
        }).unwrap();
        assert_eq!(updated["body"]["revision"], 2);
        assert_eq!(store.mutate(Some(("phone", "update", &[3;32])), || panic!("update replay")).unwrap(), updated);
        assert_eq!(store.get(&id).unwrap().unwrap().revision, 2);
        let deleted = store.mutate(Some(("phone", "delete", &[4;32])), || Ok(Mutation::Delete(id.clone()))).unwrap();
        assert_eq!(deleted, json!({"status":200,"body":{"ok":true}}));
        assert!(store.get(&id).unwrap().is_none());
        drop(store);
        let store = Schedules::open(&path).unwrap();
        assert_eq!(store.mutate(Some(("phone", "delete", &[4;32])), || panic!("delete replay")).unwrap(), deleted);
        // A different client uses its own namespace and observes the already-deleted target.
        assert_eq!(store.mutate(Some(("other-phone", "delete", &[4;32])), || Ok(Mutation::Delete(id))).unwrap()["status"], 409);
        drop(store); std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn schedule_receipt_failure_rolls_back_create_update_delete() {
        let scratch_directory = crate::test_support::Directory::new();
        let path = scratch_directory.parent().unwrap().join(format!("schedule-rollback-{}.db", crate::auth::random_hex(6)));
        let store = Schedules::open(&path).unwrap();
        let original = receipt_draft().into_schedule("existing".into(), 1);
        store.save(&original).unwrap();
        store.0.lock().unwrap().execute_batch("CREATE TRIGGER fail_receipt BEFORE INSERT ON local_command_results BEGIN SELECT RAISE(ABORT,'injected receipt failure'); END;").unwrap();
        assert!(store.mutate(Some(("phone", "create", &[1;32])), || Ok(Mutation::Create(receipt_draft()))).is_err());
        assert_eq!(store.all().unwrap().len(), 1);
        assert!(store.mutate(Some(("phone", "update", &[2;32])), || {
            let mut draft = receipt_draft(); draft.revision=1; draft.title="Lost edit".into();
            Ok(Mutation::Update("existing".into(), draft))
        }).is_err());
        assert_eq!(json!(store.get("existing").unwrap().unwrap()), json!(original));
        assert!(store.mutate(Some(("phone", "delete", &[3;32])), || Ok(Mutation::Delete("existing".into()))).is_err());
        drop(store);
        let store = Schedules::open(&path).unwrap();
        assert_eq!(json!(store.get("existing").unwrap().unwrap()), json!(original));
        assert_eq!(store.0.lock().unwrap().query_row("SELECT count(*) FROM local_command_results", [], |r|r.get::<_,i64>(0)).unwrap(), 0);
        store.0.lock().unwrap().execute_batch("DROP TRIGGER fail_receipt;").unwrap();
        assert_eq!(store.mutate(Some(("phone", "delete", &[3;32])), || Ok(Mutation::Delete("existing".into()))).unwrap()["status"], 200);
        drop(store); std::fs::remove_file(path).unwrap();
    }

    fn local_at(year: i32, month: i32, day: i32, hour: i32, minute: i32) -> i64 {
        let mut local = unsafe { std::mem::zeroed::<libc::tm>() };
        local.tm_year = year - 1900;
        local.tm_mon = month - 1;
        local.tm_mday = day;
        local.tm_hour = hour;
        local.tm_min = minute;
        local.tm_isdst = -1;
        unsafe { libc::mktime(&mut local) as i64 * 1000 }
    }

    #[test]
    fn custom_intervals_and_one_time_preserve_the_anchor() {
        let jan31 = local_at(2027, 1, 31, 9, 30);
        let feb28 = local_at(2027, 2, 28, 9, 30);
        let mar31 = local_at(2027, 3, 31, 9, 30);
        assert_eq!(next_after(Frequency::EveryMonths, 0, 0, None, Some(1), Some(jan31), jan31), Some(feb28));
        assert_eq!(next_after(Frequency::EveryMonths, 0, 0, None, Some(1), Some(jan31), feb28), Some(mar31));
        assert_eq!(next_after(Frequency::EveryDays, 0, 0, None, Some(3), Some(jan31), jan31), Some(local_at(2027, 2, 3, 9, 30)));
        assert_eq!(next_after(Frequency::EveryHours, 0, 0, None, Some(5), Some(jan31), jan31 + 5 * 3_600_000), Some(jan31 + 10 * 3_600_000));
        assert_eq!(next_after(Frequency::Once, 0, 0, None, None, Some(jan31), jan31 - 1), Some(jan31));
        assert_eq!(next_after(Frequency::Once, 0, 0, None, None, Some(jan31), jan31), None);
    }

    #[test]
    fn next_occurrence_respects_local_calendar_and_weekday() {
        let now = now_millis();
        for day in 0..7 {
            let next = next_after(Frequency::Weekly, 10, 30, Some(day), None, None, now).unwrap();
            assert!(next > now && next <= now + 8 * 86_400_000);
            let seconds = (next / 1000) as libc::time_t;
            let mut local = unsafe { std::mem::zeroed() };
            unsafe { libc::localtime_r(&seconds, &mut local) };
            assert_eq!((local.tm_wday, local.tm_hour, local.tm_min), (day as i32, 10, 30));
        }
    }

    #[test]
    fn claim_is_durable_and_never_claims_twice() {
        let scratch_directory = crate::test_support::Directory::new();
        let path = scratch_directory.parent().unwrap().join(format!("schedule-{}.db", crate::auth::random_hex(6)));
        let store = Schedules::open(&path).unwrap();
        let s = Draft { title: "Review".into(), text: "Review project".into(), project: None, disallowed_tools: vec![],
            options: Default::default(), frequency: Frequency::Daily, hour: 0, minute: 0,
            weekday: None, interval: None, start_at: None, enabled: true, revision: 0 }.into_schedule("one".into(), 1);
        store.save(&s).unwrap();
        assert!(store.claim("one", false).unwrap().is_ok());
        assert!(store.claim("one", false).unwrap().is_err());
        drop(store);
        let store = Schedules::open(&path).unwrap();
        assert_eq!(store.get("one").unwrap().unwrap().status, "interrupted");
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn scheduled_tool_policy_survives_a_restart_and_is_scoped_to_its_request() {
        let scratch_directory = crate::test_support::Directory::new();
        let path = scratch_directory.parent().unwrap().join(format!("schedule-tools-{}.db", crate::auth::random_hex(6)));
        let policy = vec!["mcp__revenuecat__create-*".to_string(), "mcp__revenuecat__update-*".to_string()];
        {
            let store = Schedules::open(&path).unwrap();
            store.remember_tools("r-protected", &policy).unwrap();
        }
        let store = Schedules::open(&path).unwrap();
        assert_eq!(store.tools_for_request("r-protected").unwrap(), policy);
        assert!(store.tools_for_request("r-other").unwrap().is_empty());
        drop(store);
        let _ = std::fs::remove_file(path);
    }

    #[tokio::test]
    async fn a_run_creates_a_task_without_an_orchestrator() {
        struct FakeLauncher(Mutex<Vec<Vec<String>>>);

        impl crate::orchestrate::Launcher for FakeLauncher {
            async fn run(&self, _cwd: &str, args: &[String]) -> Result<String, crate::orchestrate::Refusal> {
                self.0.lock().unwrap().push(args.to_vec());
                Ok("started".into())
            }

            async fn stop(&self, _: &str) {}
            fn plugin(&self) -> Option<std::path::PathBuf> { Some("/tmp/alondra-test-plugin".into()) }
        }

        let scratch_directory = crate::test_support::Directory::new();
        let home = scratch_directory.parent().unwrap().join(format!("schedule-e2e-{}", crate::auth::random_hex(6)));
        let config = alondra_core::config::Config::resolve(home.clone(), &Default::default(), |_| None, || Some("Tester".into()));
        let hub = Arc::new(Hub::open(config).unwrap());
        fixture_projects(&hub);
        #[cfg(unix)] {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(std::fs::metadata(&home).unwrap().permissions().mode() & 0o777, 0o700);
            assert_eq!(std::fs::metadata(home.join("schedules.db")).unwrap().permissions().mode() & 0o777, 0o600);
        }
        let draft = Draft { title: "Review".into(), text: "Review the project".into(), project: None, disallowed_tools: vec!["mcp__revenuecat__create-*".into()],
            options: Default::default(), frequency: Frequency::Daily, hour: 9, minute: 0,
            weekday: None, interval: None, start_at: None, enabled: true, revision: 0 };
        let created = create(State(hub.clone()), Json(draft)).await;
        assert_eq!(created.status(), StatusCode::CREATED);
        let bytes = axum::body::to_bytes(created.into_body(), 64 * 1024).await.unwrap();
        let schedule: Schedule = serde_json::from_slice(&bytes).unwrap();
        let sent = dispatch(hub.clone(), &schedule.id, false).await;
        assert_eq!(sent.status(), StatusCode::OK);
        let after = hub.schedules.get(&schedule.id).unwrap().unwrap();
        let request = after.last_request.unwrap();
        assert!(hub.read(|v| v.board.requests.contains_key(&request)));
        assert!(hub.read(|v| v.board.tasks.values().any(|t| t.request == request && t.brief.as_deref() == Some("Review the project") && matches!(&t.start, Some(Start::NewNode { .. })))));
        assert!(hub.read(|v| v.messages.is_empty()));
        assert_eq!(after.status, "sent");
        let launcher = FakeLauncher(Mutex::new(vec![]));
        let started = crate::orchestrate::start_due(&hub, &launcher, &crate::prs::GhCli, &mut Default::default()).await;
        assert_eq!(started.len(), 1);
        assert_eq!(launcher.0.lock().unwrap().len(), 1);
        let calls = launcher.0.lock().unwrap();
        let flags = &calls[0];
        let deny = flags.iter().position(|arg| arg == "--disallowedTools").expect("scheduled launch carries tool policy");
        assert_eq!(flags[deny + 1], "mcp__revenuecat__create-*");
        drop(calls);
        assert!(hub.read(|v| v.messages.is_empty()));
        std::fs::remove_dir_all(home).unwrap();
    }

    #[tokio::test]
    async fn failed_scheduled_launch_is_visible_and_not_retried_after_restart() {
        struct RefusingLauncher;
        impl crate::orchestrate::Launcher for RefusingLauncher {
            async fn run(&self, _cwd: &str, _args: &[String]) -> Result<String, crate::orchestrate::Refusal> {
                Err((StatusCode::BAD_GATEWAY, "launcher unavailable".into()))
            }
            async fn stop(&self, _: &str) {}
            fn plugin(&self) -> Option<std::path::PathBuf> { Some("/tmp/alondra-test-plugin".into()) }
        }

        let scratch_directory = crate::test_support::Directory::new();
        let home = scratch_directory.parent().unwrap().join(format!("schedule-failure-{}", crate::auth::random_hex(6)));
        let config = alondra_core::config::Config::resolve(home.clone(), &Default::default(), |_| None, || Some("Tester".into()));
        let hub = Arc::new(Hub::open(config.clone()).unwrap());
        fixture_projects(&hub);
        let draft = Draft { title: "Review".into(), text: "Review the project".into(), project: None, disallowed_tools: vec![],
            options: Default::default(), frequency: Frequency::Daily, hour: 9, minute: 0,
            weekday: None, interval: None, start_at: None, enabled: true, revision: 0 };
        let created = create(State(hub.clone()), Json(draft)).await;
        let bytes = axum::body::to_bytes(created.into_body(), 64 * 1024).await.unwrap();
        let schedule: Schedule = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(dispatch(hub.clone(), &schedule.id, false).await.status(), StatusCode::OK);
        assert!(crate::orchestrate::start_due(&hub, &RefusingLauncher, &crate::prs::GhCli, &mut Default::default()).await.is_empty());
        let after = hub.schedules.get(&schedule.id).unwrap().unwrap();
        assert_eq!(after.status, "failed");
        assert_eq!(after.last_error.as_deref(), Some("launcher unavailable"));
        assert!(hub.read(|v| v.messages.is_empty()));
        drop(hub);
        let hub = Hub::open(config).unwrap();
        assert!(crate::orchestrate::start_due(&hub, &RefusingLauncher, &crate::prs::GhCli, &mut Default::default()).await.is_empty());
        std::fs::remove_dir_all(home).unwrap();
    }

    #[tokio::test]
    async fn a_due_run_creates_only_one_task_without_an_orchestrator() {
        let scratch_directory = crate::test_support::Directory::new();
        let home = scratch_directory.parent().unwrap().join(format!("schedule-retry-{}", crate::auth::random_hex(6)));
        let config = alondra_core::config::Config::resolve(home.clone(), &Default::default(), |_| None, || Some("Tester".into()));
        let hub = Arc::new(Hub::open(config).unwrap());
        fixture_projects(&hub);
        let draft = Draft { title: "Review".into(), text: "Review the project".into(), project: None, disallowed_tools: vec![],
            options: Default::default(), frequency: Frequency::Daily, hour: 9, minute: 0,
            weekday: None, interval: None, start_at: None, enabled: true, revision: 0 };
        let created = create(State(hub.clone()), Json(draft)).await;
        let bytes = axum::body::to_bytes(created.into_body(), 64 * 1024).await.unwrap();
        let mut schedule: Schedule = serde_json::from_slice(&bytes).unwrap();
        schedule.next_run = Some(now_millis() - 1);
        hub.schedules.0.lock().unwrap().execute("UPDATE schedules SET body=?2 WHERE id=?1",
            params![schedule.id, serde_json::to_string(&schedule).unwrap()]).unwrap();

        assert_eq!(dispatch(hub.clone(), &schedule.id, true).await.status(), StatusCode::OK);
        assert_eq!(dispatch(hub.clone(), &schedule.id, true).await.status(), StatusCode::CONFLICT);
        let after = hub.schedules.get(&schedule.id).unwrap().unwrap();
        assert_eq!(after.status, "sent");
        assert!(after.last_request.is_some());
        assert_eq!(hub.read(|v| v.board.tasks.values().filter(|t| Some(&t.request) == after.last_request.as_ref()).count()), 1);
        std::fs::remove_dir_all(home).unwrap();
    }
}
