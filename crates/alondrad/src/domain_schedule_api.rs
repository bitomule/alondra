//! Schedule v2 presentation and trusted draft materialization. The launcher is P4.
use crate::domain_runtime::DomainRuntime;
use crate::domain_store::{StoreError, TransportIdentity};
use alondra_core::domain_v2 as d;
use alondra_core::options::{self, Choice, OptionsFile};
use alondra_core::now_millis;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::Deserialize;
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::path::Path;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ScheduleDraft {
    title: String,
    text: String,
    project: Option<String>,
    #[serde(default)]
    options: BTreeMap<String, Choice>,
    #[serde(default)]
    disallowed_tools: Vec<String>,
    frequency: d::ScheduleFrequency,
    hour: u8,
    minute: u8,
    weekday: Option<u8>,
    interval: Option<u32>,
    start_at: Option<i64>,
    #[serde(default = "enabled_default")]
    enabled: bool,
    #[serde(default)]
    revision: i64,
}

fn enabled_default() -> bool { true }

fn error(code: &str, message: &str) -> Value {
    let status = match code {
        "forbidden" => 403,
        "not_found" => 404,
        "invalid_input" => 422,
        "launch_unavailable" => 503,
        "storage_unavailable" => 500,
        _ => 409,
    };
    json!({"status":status,"body":d::DomainError::new(code, message)})
}

pub(crate) fn response(value: Value) -> Response {
    let status = value["status"].as_u64().and_then(|status| StatusCode::from_u16(status as u16).ok())
        .unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
    (status, Json(value["body"].clone())).into_response()
}

fn store_error(error_value: StoreError) -> Value {
    match error_value {
        StoreError::Domain(value) => error(&value.code, &value.message),
        StoreError::Storage(_) => error("storage_unavailable", "domain transaction could not be completed"),
    }
}

fn id(path: &str) -> Option<&str> {
    (!path.is_empty() && path.len() <= 256 && path.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')).then_some(path)
}

fn schedule_path(path: &str) -> Option<(&str, Option<&str>)> {
    let suffix = path.strip_prefix("/v2/domain/schedules/")?;
    if let Some(schedule_id) = suffix.strip_suffix("/run") {
        return Some((id(schedule_id)?, Some("run")));
    }
    Some((id(suffix)?, None))
}

pub(crate) fn approved_read_path(path: &str) -> bool {
    path == "/v2/domain/schedules" || schedule_path(path).is_some_and(|(_, action)| action.is_none())
}

pub(crate) fn approved_mutation_path(method: &str, path: &str, body: bool) -> bool {
    match (method, path, body) {
        ("POST", "/v2/domain/schedules", true) => true,
        ("PUT", _, true) | ("DELETE", _, false) => schedule_path(path).is_some_and(|(_, action)| action.is_none()),
        ("POST", _, false) => schedule_path(path).is_some_and(|(_, action)| action == Some("run")),
        _ => false,
    }
}

fn visible(state: &d::DomainState, schedule: &d::Schedule) -> Value {
    let latest = state.schedule.runs.values()
        .filter(|run| run.schedule_id == schedule.id)
        .max_by_key(|run| run.claim_revision.unwrap_or(0));
    let status = latest.map_or("never_run", |run| match run.state {
        d::ScheduleRunState::Launching => "launching",
        d::ScheduleRunState::Sent => "sent",
        d::ScheduleRunState::Failed => "failed",
        d::ScheduleRunState::Interrupted => "interrupted",
    });
    let mut value = serde_json::to_value(schedule).expect("validated Schedule serializes");
    value["status"] = json!(status);
    value["last_run"] = json!(latest.and_then(|run| run.finished_at));
    value["last_request"] = json!(latest.and_then(|run| run.request_id.as_ref()));
    value["last_error"] = json!(latest.and_then(|run| run.error.as_ref()));
    value["last_run_id"] = json!(latest.map(|run| &run.id));
    value
}

pub(crate) fn read(domain: &DomainRuntime, path: &str) -> Value {
    let state = match domain.state() {
        Ok(state) => state,
        Err(_) => return error("storage_unavailable", "domain transaction could not be completed"),
    };
    let body = if path == "/v2/domain/schedules" {
        let schedules: Vec<_> = state.schedule.schedules.values()
            .filter(|schedule| schedule.deleted_at.is_none())
            .map(|schedule| visible(&state, schedule)).collect();
        json!({"protocol_version":d::PROTOCOL_VERSION,"data_epoch":state.data_epoch,
            "revision":state.revision,"schedules":schedules})
    } else if let Some((schedule_id, None)) = schedule_path(path) {
        let Some(schedule) = state.schedule.schedules.get(&d::ScheduleId(schedule_id.into()))
            .filter(|schedule| schedule.deleted_at.is_none()) else {
            return error("not_found", "Schedule does not exist");
        };
        json!({"protocol_version":d::PROTOCOL_VERSION,"data_epoch":state.data_epoch,
            "revision":state.revision,"schedule":visible(&state, schedule)})
    } else {
        return error("invalid_input", "invalid Schedule read path");
    };
    json!({"status":200,"body":body})
}

fn old_frequency(frequency: d::ScheduleFrequency) -> crate::schedules::Frequency {
    match frequency {
        d::ScheduleFrequency::Daily => crate::schedules::Frequency::Daily,
        d::ScheduleFrequency::Weekly => crate::schedules::Frequency::Weekly,
        d::ScheduleFrequency::EveryHours => crate::schedules::Frequency::EveryHours,
        d::ScheduleFrequency::EveryDays => crate::schedules::Frequency::EveryDays,
        d::ScheduleFrequency::EveryMonths => crate::schedules::Frequency::EveryMonths,
        d::ScheduleFrequency::Once => crate::schedules::Frequency::Once,
    }
}

impl ScheduleDraft {
    fn materialize(self, home: &Path, schedule_id: d::ScheduleId, revision: i64) -> Result<d::Schedule, String> {
        let file = match std::fs::read_to_string(options::path_in(home)) {
            Ok(contents) => OptionsFile::parse(&contents)?,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => OptionsFile::default(),
            Err(error) => return Err(error.to_string()),
        };
        file.resolve(self.project.as_deref(), &self.options)?;
        let root = file.projects_root(&crate::options::user_home());
        if let Some(project) = self.project.as_deref() {
            if !options::projects(&root).iter().any(|(name, _)| name == project) {
                return Err("project must be one of the available project folders".into());
            }
        } else if !root.is_dir() {
            return Err("projects directory does not exist".into());
        }
        let now = now_millis();
        if self.frequency == d::ScheduleFrequency::Once && self.enabled
            && self.start_at.is_some_and(|at| at <= now) {
            return Err("one-time date must be in the future".into());
        }
        let mut schedule = d::Schedule {
            id: schedule_id,
            title: self.title.trim().into(),
            text: self.text.trim().into(),
            project: self.project,
            options: self.options,
            disallowed_tools: self.disallowed_tools,
            frequency: self.frequency,
            hour: self.hour,
            minute: self.minute,
            weekday: self.weekday,
            interval: self.interval,
            start_at: self.start_at,
            enabled: self.enabled,
            next_run: None,
            revision,
            deleted_at: None,
        };
        schedule.validate().map_err(|error| error.message)?;
        schedule.next_run = schedule.enabled.then(|| crate::schedules::next_after(
            old_frequency(schedule.frequency), schedule.hour, schedule.minute, schedule.weekday,
            schedule.interval, schedule.start_at, now,
        )).flatten();
        schedule.validate().map_err(|error| error.message)?;
        Ok(schedule)
    }
}

pub(crate) fn encrypted_mutation(
    domain: &DomainRuntime, home: &Path, sender: &str,
    method: &str, path: &str, body: Option<&Value>,
    identity: TransportIdentity<'_>, authorize: impl Fn() -> anyhow::Result<()>,
) -> Value {
    if sender != identity.0 {
        return error("invalid_input", "invalid Schedule operation");
    }
    mutation(domain, home, method, path, body, Some(identity), authorize)
}

/// One Schedule write for a verified user device. The relay supplies a
/// transport identity whose receipt replays a retry; a loopback request has
/// none, so its retry is a new command like any local HTTP call.
pub(crate) fn mutation(
    domain: &DomainRuntime, home: &Path,
    method: &str, path: &str, body: Option<&Value>,
    identity: Option<TransportIdentity<'_>>, authorize: impl Fn() -> anyhow::Result<()>,
) -> Value {
    if !approved_mutation_path(method, path, body.is_some()) {
        return error("invalid_input", "invalid Schedule operation");
    }
    let receipt = |domain: &DomainRuntime| match identity {
        Some(identity) => domain.transport_receipt(identity, || {
            authorize().map_err(|_| d::DomainError::new("forbidden", "device approval was revoked").into())
        }),
        None => Ok(None),
    };
    if method == "POST" && path.ends_with("/run") {
        let prior = match receipt(domain) {
            Ok(prior) => prior,
            Err(error_value) => return store_error(error_value),
        };
        if let Some(outcome) = prior { return run_response(&outcome); }
        let state = match domain.state() {
            Ok(state) => state,
            Err(_) => return error("storage_unavailable", "domain transaction could not be completed"),
        };
        let (schedule_id, _) = schedule_path(path).expect("approved Schedule path");
        let Some(schedule) = state.schedule.schedules.get(&d::ScheduleId(schedule_id.into()))
            .filter(|schedule| schedule.deleted_at.is_none()) else {
            return error("not_found", "Schedule does not exist");
        };
        let now = now_millis();
        let envelope = d::CommandEnvelope {
            protocol_version: d::PROTOCOL_VERSION,
            data_epoch: state.data_epoch,
            command_id: format!("schedule-{}", crate::auth::random_hex(16)).into(),
            expected_revision: state.revision,
            command: d::Command::ClaimScheduleRun {
                schedule_id: schedule.id.clone(),
                run_id: format!("run-{}", crate::auth::random_hex(16)).into(),
                origin: d::ScheduleRunOrigin::Manual,
                now,
                next_after_now: crate::domain_schedule_runner::next_after(schedule, now),
                expected_schedule_revision: Some(schedule.revision),
            },
        };
        let outcome = match domain.execute(&d::Actor::System, &envelope, identity, || {
            authorize().map_err(|_| d::DomainError::new("forbidden", "device approval was revoked").into())
        }) {
            Ok(outcome) => outcome,
            Err(error_value) => return store_error(error_value),
        };
        return run_response(&outcome);
    }
    let prior = match receipt(domain) {
        Ok(prior) => prior,
        Err(error_value) => return store_error(error_value),
    };
    if let Some(outcome) = prior {
        return mutation_response(method, path, body, &outcome);
    }
    let state = match domain.state() {
        Ok(state) => state,
        Err(_) => return error("storage_unavailable", "domain transaction could not be completed"),
    };
    let command = if method == "DELETE" {
        let (schedule_id, _) = schedule_path(path).expect("approved Schedule path");
        let schedule_id = d::ScheduleId(schedule_id.into());
        let expected_schedule_revision =
            state.schedule.schedules.get(&schedule_id).map(|schedule| schedule.revision);
        d::Command::DeleteSchedule {
            schedule_id, deleted_at: now_millis(), expected_schedule_revision,
        }
    } else {
        let draft: ScheduleDraft = match body.cloned().and_then(|body| serde_json::from_value(body).ok()) {
            Some(draft) => draft,
            None => return error("invalid_input", "invalid Schedule draft"),
        };
        if method == "POST" {
            let revision = 1;
            let schedule = match draft.materialize(home, crate::auth::random_hex(16).into(), revision) {
                Ok(schedule) => schedule,
                Err(message) => return error("invalid_input", &message),
            };
            d::Command::CreateSchedule { schedule }
        } else {
            let (schedule_id, _) = schedule_path(path).expect("approved Schedule path");
            let revision = match draft.revision.checked_add(1) {
                Some(revision) if revision > 1 => revision,
                _ => return error("invalid_input", "positive Schedule revision required"),
            };
            let schedule = match draft.materialize(home, schedule_id.into(), revision) {
                Ok(schedule) => schedule,
                Err(message) => return error("invalid_input", &message),
            };
            d::Command::UpdateSchedule { schedule }
        }
    };
    let envelope = d::CommandEnvelope {
        protocol_version: d::PROTOCOL_VERSION,
        data_epoch: state.data_epoch,
        command_id: format!("schedule-{}", crate::auth::random_hex(16)).into(),
        expected_revision: state.revision,
        command,
    };
    let outcome = match domain.execute(&d::Actor::System, &envelope, identity, || {
        authorize().map_err(|_| d::DomainError::new("forbidden", "device approval was revoked").into())
    }) {
        Ok(outcome) => outcome,
        Err(error_value) => return store_error(error_value),
    };
    mutation_response(method, path, body, &outcome)
}

fn run_response(outcome: &d::CommandOutcome) -> Value {
    json!({"status":202,"body":{"ok":true,"run_id":outcome.created_ids.first()}})
}

fn mutation_response(method: &str, path: &str, body: Option<&Value>, outcome: &d::CommandOutcome) -> Value {
    let body = if method == "POST" {
        json!({"id":outcome.created_ids.first(),"revision":1})
    } else if method == "PUT" {
        let (schedule_id, _) = schedule_path(path).expect("approved Schedule path");
        let revision = body.and_then(|draft| draft.get("revision")).and_then(Value::as_i64)
            .and_then(|revision| revision.checked_add(1));
        json!({"id":schedule_id,"revision":revision})
    } else {
        json!({"ok":true})
    };
    json!({"status":if method == "POST" {201} else {200},"body":body})
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    struct Fixture { root: std::path::PathBuf, home: std::path::PathBuf, domain: DomainRuntime }
    impl Fixture {
        fn new() -> Self {
            let root = std::env::temp_dir().join(format!("alondra-schedule-api-{}", crate::auth::random_hex(8)));
            let home = root.join("alondra");
            let projects = root.join("projects");
            fs::create_dir_all(&home).unwrap();
            fs::create_dir(&projects).unwrap();
            fs::write(options::path_in(&home), format!("projects_root = {:?}\n", projects.to_str().unwrap())).unwrap();
            let domain = DomainRuntime::open(&home).unwrap();
            Self { root, home, domain }
        }
        fn draft() -> Value {
            json!({"title":"Review","text":"Review project","project":null,
                "options":{},"disallowed_tools":["Bash"],"frequency":"daily",
                "hour":9,"minute":0,"weekday":null,"interval":null,
                "start_at":null,"enabled":true,"revision":0})
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) { fs::remove_dir_all(&self.root).unwrap(); }
    }

    struct FakeClaude;
    impl crate::orchestrate::Launcher for FakeClaude {
        async fn run(&self, _: &str, _: &[String]) -> Result<String, crate::orchestrate::Refusal> {
            panic!("a v2 producer launch must carry its environment");
        }
        async fn run_with_env(&self, _: &str, _: &[String], _: &[(String, String)]) -> Result<String, crate::orchestrate::Refusal> {
            Ok("backgrounded · ab1234 · scheduled/manual".into())
        }
        fn usage(&self) -> Option<alondra_core::usage::Usage> { None }
        fn plugin(&self) -> Option<std::path::PathBuf> { Some("/synthetic/plugin".into()) }
    }

    #[tokio::test]
    async fn encrypted_crud_and_manual_run_replay_original_receipts() {
        let fixture = Fixture::new();
        let draft = Fixture::draft();
        let digest = [7u8; 32];
        let create = || encrypted_mutation(&fixture.domain, &fixture.home, "phone",
            "POST", "/v2/domain/schedules", Some(&draft), ("phone", "create", &digest), || Ok(()));
        let first = create();
        assert_eq!(first["status"], 201);
        let schedule_id = first["body"]["id"].as_str().unwrap();
        assert_eq!(first["body"]["revision"], 1);
        let initial_revision = fixture.domain.state().unwrap().revision;
        let options_path = options::path_in(&fixture.home);
        let original_options = fs::read(&options_path).unwrap();
        fs::write(&options_path, "invalid = [").unwrap();
        assert_eq!(create(), first);
        fs::write(&options_path, original_options).unwrap();
        assert_eq!(fixture.domain.state().unwrap().revision, initial_revision);
        let conflict = encrypted_mutation(&fixture.domain, &fixture.home, "phone",
            "POST", "/v2/domain/schedules", Some(&draft), ("phone", "create", &[6u8;32]), || Ok(()));
        assert_eq!(conflict["body"]["code"], "idempotency_conflict");
        let path = format!("/v2/domain/schedules/{schedule_id}");
        let run_path = format!("{path}/run");
        let run = encrypted_mutation(&fixture.domain, &fixture.home, "phone",
            "POST", &run_path, None, ("phone", "run", &[8u8;32]), || Ok(()));
        assert_eq!(run["status"], 202);
        let run_id = run["body"]["run_id"].as_str().unwrap();
        assert_eq!(encrypted_mutation(&fixture.domain, &fixture.home, "phone",
            "POST", &run_path, None, ("phone", "run", &[8u8;32]), || Ok(())), run);
        assert_eq!(fixture.domain.state().unwrap().schedule.runs.len(), 1);
        let detail = read(&fixture.domain, &path);
        assert_eq!(detail["body"]["schedule"]["status"], "launching");
        assert_eq!(detail["body"]["schedule"]["revision"], 2);
        crate::domain_schedule_runner::run_cycle(&fixture.domain, &fixture.home, false, &FakeClaude).await.unwrap();
        let finished = fixture.domain.state().unwrap();
        assert_eq!(finished.schedule.runs[&d::ScheduleRunId(run_id.into())].state, d::ScheduleRunState::Sent);
        assert_eq!(read(&fixture.domain, &path)["body"]["schedule"]["status"], "sent");
        let mut update = draft.clone();
        update["revision"] = json!(3);
        update["title"] = json!("Updated review");
        let changed = encrypted_mutation(&fixture.domain, &fixture.home, "phone", "PUT",
            &path, Some(&update), ("phone", "update", &[9u8;32]), || Ok(()));
        assert_eq!(changed["status"], 200);
        assert_eq!(changed["body"]["revision"], 4);
        assert_eq!(read(&fixture.domain, &path)["body"]["schedule"]["title"], "Updated review");
        let deleted = encrypted_mutation(&fixture.domain, &fixture.home, "phone", "DELETE",
            &path, None, ("phone", "delete", &[10u8;32]), || Ok(()));
        assert_eq!(deleted["body"]["ok"], true);
        assert_eq!(read(&fixture.domain, "/v2/domain/schedules")["body"]["schedules"].as_array().unwrap().len(), 0);
        assert_eq!(read(&fixture.domain, &path)["status"], 404);
    }

    #[tokio::test]
    async fn disabled_manual_claim_replays_same_run_after_interrupted_restart() {
        let fixture = Fixture::new();
        let mut draft = Fixture::draft();
        draft["enabled"] = json!(false);
        let created = encrypted_mutation(&fixture.domain, &fixture.home, "phone",
            "POST", "/v2/domain/schedules", Some(&draft),
            ("phone", "create-disabled", &[11u8; 32]), || Ok(()));
        let path = format!("/v2/domain/schedules/{}/run", created["body"]["id"].as_str().unwrap());
        let run = encrypted_mutation(&fixture.domain, &fixture.home, "phone",
            "POST", &path, None, ("phone", "manual-disabled", &[12u8; 32]), || Ok(()));
        assert_eq!(run["status"], 202);
        let run_id = d::ScheduleRunId(run["body"]["run_id"].as_str().unwrap().into());
        let before = fixture.domain.state().unwrap();
        assert_eq!(before.schedule.runs[&run_id].origin, d::ScheduleRunOrigin::Manual);
        assert_eq!(before.schedule.schedules.values().next().unwrap().next_run, None);
        let restarted = DomainRuntime::open(&fixture.home).unwrap();
        assert_eq!(restarted.state().unwrap().schedule.runs[&run_id].state,
            d::ScheduleRunState::Interrupted);
        assert_eq!(encrypted_mutation(&restarted, &fixture.home, "phone",
            "POST", &path, None, ("phone", "manual-disabled", &[12u8; 32]), || Ok(())), run);
        assert_eq!(restarted.state().unwrap().schedule.runs.len(), 1);
        let launcher = FakeClaude;
        crate::domain_schedule_runner::run_cycle(&restarted, &fixture.home, false, &launcher).await.unwrap();
        assert!(restarted.state().unwrap().requests.is_empty());
    }

    #[test]
    fn route_and_draft_boundaries_do_not_accept_client_due_times() {
        assert!(approved_read_path("/v2/domain/schedules"));
        assert!(approved_read_path("/v2/domain/schedules/s_1"));
        assert!(!approved_read_path("/v2/domain/schedules/s_1/run"));
        assert!(!approved_read_path("/v2/domain/schedules/%2e%2e"));
        assert!(approved_mutation_path("POST", "/v2/domain/schedules/s_1/run", false));
        assert!(!approved_mutation_path("POST", "/v2/domain/schedules/s_1/run", true));
        assert!(!approved_mutation_path("DELETE", "/v2/domain/schedules/s_1?x=1", false));
        let fixture = Fixture::new();
        let mut draft = Fixture::draft();
        draft["next_run"] = json!(1);
        let rejected = encrypted_mutation(&fixture.domain, &fixture.home, "phone", "POST",
            "/v2/domain/schedules", Some(&draft), ("phone", "bad", &[1u8;32]), || Ok(()));
        assert_eq!(rejected["status"], 422);
        assert!(fixture.domain.state().unwrap().schedule.schedules.is_empty());
        let mut bad_interval = Fixture::draft();
        bad_interval["frequency"] = json!("every_days");
        bad_interval["interval"] = json!(0);
        bad_interval["start_at"] = json!(1000);
        let rejected = encrypted_mutation(&fixture.domain, &fixture.home, "phone", "POST",
            "/v2/domain/schedules", Some(&bad_interval), ("phone", "bad-interval", &[2u8;32]), || Ok(()));
        assert_eq!(rejected["status"], 422);
    }
}
