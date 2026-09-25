//! Durable, local-time schedules. Claiming a run is committed before handing it to the
//! orchestrator: a crash can leave an interrupted run, but cannot launch it twice.
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use alondra_core::now_millis;
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
    pub frequency: Frequency,
    pub hour: u8,
    pub minute: u8,
    /// Sunday is zero. Required for weekly schedules.
    pub weekday: Option<u8>,
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
pub enum Frequency { Daily, Weekly }

#[derive(Deserialize)]
pub struct Draft {
    title: String,
    text: String,
    project: Option<String>,
    #[serde(default)]
    options: std::collections::BTreeMap<String, alondra_core::options::Choice>,
    frequency: Frequency,
    hour: u8,
    minute: u8,
    weekday: Option<u8>,
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
        if self.project.as_ref().is_some_and(|p| p.trim().is_empty()) { return Err("project cannot be empty".into()); }
        let loaded = crate::options::load(hub);
        loaded.file.resolve(self.project.as_deref(), &self.options).map_err(|e| e.to_string())?;
        Ok(())
    }

    fn into_schedule(self, id: String, revision: i64) -> Schedule {
        let next_run = self.enabled.then(|| next_after(self.frequency, self.hour, self.minute, self.weekday, now_millis()));
        Schedule { id, title: self.title.trim().into(), text: self.text.trim().into(), project: self.project,
            options: self.options, frequency: self.frequency, hour: self.hour, minute: self.minute,
            weekday: self.weekday, enabled: self.enabled, next_run, last_run: None,
            last_request: None, last_error: None, status: "never_run".into(), revision }
    }
}

/// Uses the Mac's local calendar, including its daylight-saving rules. Search days rather
/// than adding 24 hours, which would drift across a clock change.
fn next_after(frequency: Frequency, hour: u8, minute: u8, weekday: Option<u8>, after: i64) -> i64 {
    let seconds = after.div_euclid(1000) as libc::time_t;
    let mut day = unsafe { std::mem::zeroed::<libc::tm>() };
    unsafe { libc::localtime_r(&seconds, &mut day) };
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
        return at;
    }
    unreachable!("one of the next eight local days matches")
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
        conn.execute_batch("CREATE TABLE IF NOT EXISTS schedules (id TEXT PRIMARY KEY, body TEXT NOT NULL);")?;
        // A process killed after claiming must never replay that dispatch on restart.
        conn.execute("UPDATE schedules SET body=json_set(body, '$.status', 'interrupted', '$.last_error', 'Alondra stopped while sending this run') WHERE json_extract(body, '$.status')='launching'", [])?;
        Ok(Self(Mutex::new(conn)))
    }

    fn all(&self) -> anyhow::Result<Vec<Schedule>> {
        let conn = self.0.lock().unwrap();
        let mut stmt = conn.prepare("SELECT body FROM schedules ORDER BY id")?;
        Ok(stmt.query_map([], |r| r.get::<_, String>(0))?.collect::<rusqlite::Result<Vec<_>>>()?
            .into_iter().map(|body| serde_json::from_str(&body)).collect::<Result<Vec<_>, _>>()?)
    }

    fn get(&self, id: &str) -> anyhow::Result<Option<Schedule>> {
        let conn = self.0.lock().unwrap();
        let body: Option<String> = conn.query_row("SELECT body FROM schedules WHERE id=?1", [id], |r| r.get(0)).optional()?;
        body.map(|b| serde_json::from_str(&b).map_err(Into::into)).transpose()
    }

    fn save(&self, schedule: &Schedule) -> anyhow::Result<()> {
        let conn = self.0.lock().unwrap();
        conn.execute("INSERT INTO schedules(id,body) VALUES(?1,?2)",
            params![schedule.id, serde_json::to_string(schedule)?])?;
        Ok(())
    }

    fn replace(&self, id: &str, revision: i64, draft: Draft) -> anyhow::Result<Result<Schedule, &'static str>> {
        let conn = self.0.lock().unwrap();
        let body: Option<String> = conn.query_row("SELECT body FROM schedules WHERE id=?1", [id], |r| r.get(0)).optional()?;
        let Some(body) = body else { return Ok(Err("not_found")); };
        let old: Schedule = serde_json::from_str(&body)?;
        if old.revision != revision || old.status == "launching" { return Ok(Err("conflict")); }
        let mut new = draft.into_schedule(id.into(), revision + 1);
        new.last_run = old.last_run;
        new.last_request = old.last_request;
        new.last_error = old.last_error;
        new.status = old.status;
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
        s.next_run = s.enabled.then(|| next_after(s.frequency, s.hour, s.minute, s.weekday, now));
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

    fn wait_for_orchestrator(&self, id: &str, due_only: bool) -> anyhow::Result<()> {
        let conn = self.0.lock().unwrap();
        let body: String = conn.query_row("SELECT body FROM schedules WHERE id=?1", [id], |r| r.get(0))?;
        let mut s: Schedule = serde_json::from_str(&body)?;
        if due_only && (!s.enabled || s.next_run.is_none_or(|at| at > now_millis())) { return Ok(()); }
        if !due_only && s.status != "launching" { return Ok(()); }
        if s.status == "waiting" { return Ok(()); }
        s.status = "waiting".into();
        s.last_error = None;
        // Keep this occurrence due. The next poll sends it once the orchestrator returns.
        if s.enabled { s.next_run = Some(now_millis()); }
        s.revision += 1;
        conn.execute("UPDATE schedules SET body=?2 WHERE id=?1", params![id, serde_json::to_string(&s)?])?;
        Ok(())
    }

    fn delete(&self, id: &str) -> anyhow::Result<bool> {
        let conn = self.0.lock().unwrap();
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

pub async fn create(State(hub): State<Arc<Hub>>, Json(draft): Json<Draft>) -> Response {
    if let Err(e) = draft.validate(&hub) { return error(StatusCode::BAD_REQUEST, e); }
    let s = draft.into_schedule(crate::auth::random_hex(16), 1);
    match hub.schedules.save(&s) {
        Ok(()) => (StatusCode::CREATED, Json(json!(s))).into_response(),
        Err(e) => error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    }
}

pub async fn update(State(hub): State<Arc<Hub>>, RoutePath(id): RoutePath<String>, Json(draft): Json<Draft>) -> Response {
    if let Err(e) = draft.validate(&hub) { return error(StatusCode::BAD_REQUEST, e); }
    let revision = draft.revision;
    match hub.schedules.replace(&id, revision, draft) {
        Ok(Ok(s)) => Json(json!(s)).into_response(),
        Ok(Err("not_found")) => error(StatusCode::NOT_FOUND, "schedule not found"),
        Ok(Err(_)) => error(StatusCode::CONFLICT, "schedule changed elsewhere; refresh before editing"),
        Err(e) => error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    }
}

pub async fn delete(State(hub): State<Arc<Hub>>, RoutePath(id): RoutePath<String>) -> Response {
    match hub.schedules.delete(&id) {
        Ok(true) => Json(json!({"ok": true})).into_response(),
        Ok(false) => error(StatusCode::CONFLICT, "schedule not found or a run is being sent"),
        Err(e) => error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    }
}

async fn dispatch(hub: Arc<Hub>, id: &str, due_only: bool) -> Response {
    if !hub.read(|v| v.nodes.values().any(|n| n.is_orchestrator)) {
        if due_only {
            if let Err(e) = hub.schedules.wait_for_orchestrator(id, true) {
                return error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string());
            }
        }
        return error(StatusCode::SERVICE_UNAVAILABLE, "The scheduled task is waiting for the orchestrator");
    }
    let s = match hub.schedules.claim(id, due_only) {
        Ok(Ok(s)) => s,
        Ok(Err(reason)) => return error(StatusCode::CONFLICT, reason),
        Err(e) => return error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    };
    let body = crate::http::RequestBody { text: s.text, attachments: vec![], project: s.project, options: s.options, issue: None };
    let response = crate::http::new_request_with(hub.clone(), body, &crate::prs::GhCli).await;
    let status = response.status();
    let bytes = match axum::body::to_bytes(response.into_body(), 64 * 1024).await {
        Ok(bytes) => bytes,
        Err(e) => { let _ = hub.schedules.finish(id, None, Some(&e.to_string())); return error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()); }
    };
    let value: Value = serde_json::from_slice(&bytes).unwrap_or_default();
    let request = value.get("request_id").and_then(Value::as_str);
    let message = value.get("error").and_then(Value::as_str).unwrap_or("could not send request");
    if status == StatusCode::CONFLICT && message == "the orchestrator is not connected to Alondra" {
        if let Err(e) = hub.schedules.wait_for_orchestrator(id, false) {
            return error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string());
        }
        return error(StatusCode::SERVICE_UNAVAILABLE, "The scheduled task is waiting for the orchestrator");
    }
    if let Err(e) = hub.schedules.finish(id, request, (!status.is_success()).then_some(message)) {
        return error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string());
    }
    (status, Json(value)).into_response()
}

pub async fn run_now(State(hub): State<Arc<Hub>>, RoutePath(id): RoutePath<String>) -> Response {
    dispatch(hub, &id, false).await
}

pub async fn run(hub: Arc<Hub>) {
    loop {
        if let Ok(all) = hub.schedules.all() {
            for s in all.into_iter().filter(|s| s.enabled && s.next_run.is_some_and(|at| at <= now_millis())) {
                let result = dispatch(hub.clone(), &s.id, true).await;
                if !result.status().is_success() && result.status() != StatusCode::SERVICE_UNAVAILABLE {
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
    use alondra_core::{DaemonObservation, Event};

    #[test]
    fn next_occurrence_respects_local_calendar_and_weekday() {
        let now = now_millis();
        for day in 0..7 {
            let next = next_after(Frequency::Weekly, 10, 30, Some(day), now);
            assert!(next > now && next <= now + 8 * 86_400_000);
            let seconds = (next / 1000) as libc::time_t;
            let mut local = unsafe { std::mem::zeroed() };
            unsafe { libc::localtime_r(&seconds, &mut local) };
            assert_eq!((local.tm_wday, local.tm_hour, local.tm_min), (day as i32, 10, 30));
        }
    }

    #[test]
    fn claim_is_durable_and_never_claims_twice() {
        let path = std::env::temp_dir().join(format!("schedule-{}.db", crate::auth::random_hex(6)));
        let store = Schedules::open(&path).unwrap();
        let s = Draft { title: "Review".into(), text: "Review project".into(), project: None,
            options: Default::default(), frequency: Frequency::Daily, hour: 0, minute: 0,
            weekday: None, enabled: true, revision: 0 }.into_schedule("one".into(), 1);
        store.save(&s).unwrap();
        assert!(store.claim("one", false).unwrap().is_ok());
        assert!(store.claim("one", false).unwrap().is_err());
        drop(store);
        let store = Schedules::open(&path).unwrap();
        assert_eq!(store.get("one").unwrap().unwrap().status, "interrupted");
        std::fs::remove_file(path).unwrap();
    }

    #[tokio::test]
    async fn a_run_uses_the_real_request_path_and_records_its_request() {
        let home = std::env::temp_dir().join(format!("schedule-e2e-{}", crate::auth::random_hex(6)));
        let config = alondra_core::config::Config::resolve(home.clone(), &Default::default(), |_| None, || Some("Tester".into()));
        let hub = Arc::new(Hub::open(config, false).unwrap());
        #[cfg(unix)] {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(std::fs::metadata(&home).unwrap().permissions().mode() & 0o777, 0o700);
            assert_eq!(std::fs::metadata(home.join("schedules.db")).unwrap().permissions().mode() & 0o777, 0o600);
        }
        hub.append(Event::DaemonSeen(DaemonObservation {
            session_id: "orch".into(), short_id: "orch".into(), name: Some("orchestrator".into()), ..Default::default()
        })).unwrap();
        hub.append(Event::MarkedOrchestrator { session_id: "orch".into() }).unwrap();
        let draft = Draft { title: "Review".into(), text: "Review the project".into(), project: None,
            options: Default::default(), frequency: Frequency::Daily, hour: 9, minute: 0,
            weekday: None, enabled: true, revision: 0 };
        let created = create(State(hub.clone()), Json(draft)).await;
        assert_eq!(created.status(), StatusCode::CREATED);
        let bytes = axum::body::to_bytes(created.into_body(), 64 * 1024).await.unwrap();
        let schedule: Schedule = serde_json::from_slice(&bytes).unwrap();
        let sent = dispatch(hub.clone(), &schedule.id, false).await;
        assert_eq!(sent.status(), StatusCode::OK);
        let after = hub.schedules.get(&schedule.id).unwrap().unwrap();
        let request = after.last_request.unwrap();
        assert!(hub.read(|v| v.board.requests.contains_key(&request)));
        assert!(hub.read(|v| v.messages.values().any(|m| m.text.contains("Review the project") && m.text.contains(&request))));
        assert_eq!(after.status, "sent");
        std::fs::remove_dir_all(home).unwrap();
    }

    #[tokio::test]
    async fn a_due_run_waits_for_the_orchestrator_then_sends_once() {
        let home = std::env::temp_dir().join(format!("schedule-retry-{}", crate::auth::random_hex(6)));
        let config = alondra_core::config::Config::resolve(home.clone(), &Default::default(), |_| None, || Some("Tester".into()));
        let hub = Arc::new(Hub::open(config, false).unwrap());
        let draft = Draft { title: "Review".into(), text: "Review the project".into(), project: None,
            options: Default::default(), frequency: Frequency::Daily, hour: 9, minute: 0,
            weekday: None, enabled: true, revision: 0 };
        let created = create(State(hub.clone()), Json(draft)).await;
        let bytes = axum::body::to_bytes(created.into_body(), 64 * 1024).await.unwrap();
        let mut schedule: Schedule = serde_json::from_slice(&bytes).unwrap();
        schedule.next_run = Some(now_millis() - 1);
        hub.schedules.0.lock().unwrap().execute("UPDATE schedules SET body=?2 WHERE id=?1",
            params![schedule.id, serde_json::to_string(&schedule).unwrap()]).unwrap();

        assert_eq!(dispatch(hub.clone(), &schedule.id, true).await.status(), StatusCode::SERVICE_UNAVAILABLE);
        let waiting = hub.schedules.get(&schedule.id).unwrap().unwrap();
        assert_eq!(waiting.status, "waiting");
        assert_eq!(waiting.last_run, None);
        assert!(waiting.next_run.unwrap() <= now_millis());

        hub.append(Event::DaemonSeen(DaemonObservation {
            session_id: "orch".into(), short_id: "orch".into(), name: Some("orchestrator".into()), ..Default::default()
        })).unwrap();
        hub.append(Event::MarkedOrchestrator { session_id: "orch".into() }).unwrap();
        assert_eq!(dispatch(hub.clone(), &schedule.id, true).await.status(), StatusCode::OK);
        assert_eq!(dispatch(hub.clone(), &schedule.id, true).await.status(), StatusCode::CONFLICT);
        let after = hub.schedules.get(&schedule.id).unwrap().unwrap();
        assert_eq!(after.status, "sent");
        assert!(after.last_request.is_some());
        std::fs::remove_dir_all(home).unwrap();
    }
}
