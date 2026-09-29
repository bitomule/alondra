//! Schedule v2's local-calendar and launch preparation adapter. The command
//! carrying this intent must be committed with its Run, Request, and Task before
//! a technical launch claim can call Claude.
use alondra_core::domain_v2::{ScheduleDefinitionSnapshot, ScheduleFrequency, ScheduleId, ScheduleLaunchIntent, ScheduleRunId};
use alondra_core::options::{self, OptionsFile};
use alondra_core::domain_v2::{Actor, AgentId, Command, CommandEnvelope, CommandOutcome, DomainState, Request, RequestId, ScheduleLaunchState, ScheduleRunOrigin, ScheduleRunState, Task, TaskId, TaskState, PROTOCOL_VERSION};
use alondra_core::options::Resolved;
use crate::domain_runtime::DomainRuntime;
use crate::domain_store::StoreError;
use anyhow::{Context, Result};
use axum::http::StatusCode;
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

pub(crate) fn next_after(schedule: &alondra_core::domain_v2::Schedule, now: i64) -> Option<i64> {
    if !schedule.enabled {
        return None;
    }
    let frequency = match schedule.frequency {
        ScheduleFrequency::Daily => crate::schedules::Frequency::Daily,
        ScheduleFrequency::Weekly => crate::schedules::Frequency::Weekly,
        ScheduleFrequency::EveryHours => crate::schedules::Frequency::EveryHours,
        ScheduleFrequency::EveryDays => crate::schedules::Frequency::EveryDays,
        ScheduleFrequency::EveryMonths => crate::schedules::Frequency::EveryMonths,
        ScheduleFrequency::Once => crate::schedules::Frequency::Once,
    };
    crate::schedules::next_after(
        frequency,
        schedule.hour,
        schedule.minute,
        schedule.weekday,
        schedule.interval,
        schedule.start_at,
        now,
    )
}

/// Capture the exact technical new-node command at dispatch time. Neither a
/// later Schedule edit nor an options.toml edit can change this saved intent.
pub(crate) fn prepare_intent(
    schedule_id: &ScheduleId,
    run_id: &ScheduleRunId,
    definition: &ScheduleDefinitionSnapshot,
    file: &OptionsFile,
    projects_root: &Path,
) -> Result<ScheduleLaunchIntent, String> {
    let resolved = file.resolve(definition.project.as_deref(), &definition.options)?;
    let cwd = match definition.project.as_deref() {
        Some(project) => {
            if !options::projects(projects_root)
                .iter()
                .any(|(name, _)| name == project)
            {
                return Err("schedule project is not an available project folder".into());
            }
            projects_root.join(project)
        }
        None => projects_root.to_path_buf(),
    };
    if !cwd.is_dir() {
        return Err("schedule project directory does not exist".into());
    }
    let cwd = cwd.to_str().ok_or("schedule directory path is not UTF-8")?;
    let name = format!(
        "scheduled/{}-{}",
        schedule_id.0.chars().take(8).collect::<String>(),
        run_id.0
    );
    let launch = ScheduleLaunchIntent {
        name,
        cwd: cwd.into(),
        prompt: definition.text.clone(),
        options_json: serde_json::to_string(&resolved).map_err(|error| error.to_string())?,
    };
    launch.validate(definition).map_err(|error| error.to_string())?;
    Ok(launch)
}

pub(crate) struct ProducerLaunch<'a> {
    pub task_id: &'a TaskId,
    pub request_id: &'a RequestId,
    pub state: &'a DomainState,
}

/// The mission a scheduled v2 node starts with: the Schedule text, its identity and
/// cursor in the domain, and the options the Request's policy picked.
pub(crate) fn producer_prompt(text: &str, resolved: &Resolved, producer: &ProducerLaunch<'_>) -> String {
    let mut lines = vec![
        text.trim_end().to_owned(),
        String::new(),
        "## Alondra domain v2".into(),
        String::new(),
        format!("Load the alondra-domain-v2 skill. This session is the Agent assigned to Task {} of Request {}.",
            producer.task_id, producer.request_id),
        format!("Cursor: data_epoch={} revision={}. Send an expected_revision of 0 when you hold no newer cursor; the daemon fills the current revision for a producer.",
            producer.state.data_epoch.0, producer.state.revision),
        String::new(),
        "## Launch options".into(),
        String::new(),
        format!("The user chose these for this work (request {}):", producer.request_id),
    ];
    for picked in resolved.picked.iter().filter(|p| p.id != alondra_core::options::PR && p.id != alondra_core::options::EFFORT) {
        lines.push(match &picked.mission {
            Some(mission) => format!("- {}: {}. {mission}", picked.label, picked.value_label),
            None => format!("- {}: {}.", picked.label, picked.value_label),
        });
    }
    if let Some(issue) = &resolved.issue {
        lines.push(format!("- This work comes from issue {issue}: write `Fixes {issue}` in the body of the PR that solves it."));
    }
    lines.join("\n")
}

/// Invoke only after the technical intent has been claimed durably. The caller
/// records the result against that claim; this function never retries Claude.
async fn invoke_claimed(
    launch: &ScheduleLaunchIntent,
    producer: &ProducerLaunch<'_>,
    disallowed_tools: &[String],
    home: &Path,
    key: &str,
    launcher: &impl crate::orchestrate::Launcher,
) -> Result<(Option<String>, PathBuf), crate::orchestrate::Refusal> {
    if let Some(reason) = crate::http::launch_blocked(launcher.usage().as_ref(), false) {
        return Err((StatusCode::CONFLICT, reason));
    }
    if !Path::new(&launch.cwd).is_dir() {
        return Err((StatusCode::BAD_REQUEST, "schedule project directory does not exist".into()));
    }
    let Some(plugin) = launcher.plugin() else {
        return Err((StatusCode::SERVICE_UNAVAILABLE, "Alondra's Claude plugin is unavailable".into()));
    };
    let resolved: Resolved = serde_json::from_str(&launch.options_json)
        .map_err(|error| (StatusCode::INTERNAL_SERVER_ERROR, error.to_string()))?;
    let settings = crate::domain_sessions::prepare(home, key, &[plugin.as_path()])
        .map_err(|error| (StatusCode::INTERNAL_SERVER_ERROR, format!("Could not write the session's settings: {error}")))?;
    let mut args: Vec<String> = ["--bg", "--name", &launch.name, "--permission-mode", "auto"].map(String::from).into();
    args.extend(resolved.launch_flags(None));
    args.extend(crate::domain_sessions::flags(&[plugin.as_path()], &settings));
    if !disallowed_tools.is_empty() {
        args.extend(["--disallowedTools".into(), disallowed_tools.join(" ")]);
    }
    args.push("--".into());
    args.push(producer_prompt(&launch.prompt, &resolved, producer));
    match launcher.run(&launch.cwd, &args).await {
        Ok(output) => Ok((alondra_core::sessions::launched_short(&output), settings)),
        Err(refusal) => {
            crate::domain_sessions::remove(&settings);
            Err(refusal)
        }
    }
}

fn execute_system(domain: &DomainRuntime, command: Command) -> Result<CommandOutcome, StoreError> {
    let command_id: alondra_core::domain_v2::CommandId =
        format!("schedule-worker-{}", crate::auth::random_hex(16)).into();
    for _ in 0..5 {
        let state = domain.state().map_err(StoreError::Storage)?;
        let envelope = CommandEnvelope {
            protocol_version: PROTOCOL_VERSION,
            data_epoch: state.data_epoch,
            command_id: command_id.clone(),
            expected_revision: state.revision,
            command: command.clone(),
        };
        match domain.execute(&Actor::System, &envelope, None, || Ok(())) {
            Err(StoreError::Domain(error)) if error.code == "stale_revision" => continue,
            result => return result,
        }
    }
    Err(alondra_core::domain_v2::DomainError::new("stale_revision", "Schedule writer remained busy").into())
}

fn ordinary_conflict(error: &StoreError) -> bool {
    matches!(error, StoreError::Domain(domain) if matches!(domain.code.as_str(), "conflict" | "not_found" | "stale_revision"))
}

fn options_at(home: &Path) -> Result<OptionsFile> {
    match std::fs::read_to_string(options::path_in(home)) {
        Ok(contents) => OptionsFile::parse(&contents).map_err(anyhow::Error::msg),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(OptionsFile::default()),
        Err(error) => Err(error.into()),
    }
}

fn request_task(snapshot: &ScheduleDefinitionSnapshot) -> (Request, Task) {
    let request_id: RequestId = format!("r-{}", crate::auth::random_hex(16)).into();
    let request = Request { id: request_id.clone(), body: snapshot.text.clone(), closed: false };
    let task = Task {
        id: format!("t-{}", crate::auth::random_hex(16)).into(),
        request_id,
        agent_id: None,
        title: snapshot.title.clone(),
        state: TaskState::NotStarted,
        revision: 0,
        started: false,
        review_prs: false,
        final_presentation: None,
        pr_id: None,
        dependencies: BTreeSet::new(),
        stack: None,
        documents: BTreeMap::new(),
        archived: false,
    };
    (request, task)
}

fn fail_run(domain: &DomainRuntime, run_id: &ScheduleRunId, reason: String) -> Result<()> {
    match execute_system(domain, Command::RecordScheduleRunFailed {
        run_id: run_id.clone(), reason, finished_at: alondra_core::now_millis(),
    }) {
        Ok(_) => (),
        Err(error) if ordinary_conflict(&error) => (),
        Err(error) => return Err(error.into()),
    }
    Ok(())
}

/// A persisted Run claim either reaches an atomic Request/Task/intent commit or
/// receives an explicit failure. A crash before either is interrupted on open.
fn finish_claim(domain: &DomainRuntime, home: &Path, run_id: &ScheduleRunId) -> Result<()> {
    let state = domain.state()?;
    let Some(run) = state.schedule.runs.get(run_id).filter(|run| run.state == ScheduleRunState::Launching) else {
        return Ok(());
    };
    let snapshot = run.definition_snapshot.as_ref().context("claimed Run has no definition")?;
    let prepared = (|| -> Result<ScheduleLaunchIntent> {
        let file = options_at(home)?;
        let root = file.projects_root(&crate::options::user_home());
        prepare_intent(&run.schedule_id, run_id, snapshot, &file, &root).map_err(anyhow::Error::msg)
    })();
    let launch = match prepared {
        Ok(launch) => launch,
        Err(error) => {
            fail_run(domain, run_id, error.to_string())?;
            return Ok(());
        }
    };
    let (request, task) = request_task(snapshot);
    let sent = execute_system(domain, Command::RecordScheduleRunSent {
        run_id: run_id.clone(), request, task, launch, finished_at: alondra_core::now_millis(),
    });
    if let Err(error) = sent {
        if ordinary_conflict(&error) { return Ok(()); }
        let reason = format!("Could not record scheduled Request and Task: {error}");
        fail_run(domain, run_id, reason)?;
    }
    Ok(())
}

/// The successful ClaimScheduleLaunch commit is the irreversible boundary:
/// exactly one caller may invoke Claude, even if the process dies afterward.
async fn dispatch_pending(
    domain: &DomainRuntime, home: &Path, run_id: &ScheduleRunId,
    launcher: &impl crate::orchestrate::Launcher,
) -> Result<()> {
    let state = domain.state()?;
    let Some(run) = state.schedule.runs.get(run_id).filter(|run| run.state == ScheduleRunState::Sent) else {
        return Ok(());
    };
    let Some(launch) = run.launch.as_ref().filter(|launch| launch.state == ScheduleLaunchState::Pending) else {
        return Ok(());
    };
    let request_id = run.request_id.as_ref().context("sent run has no Request")?;
    let policy = match domain.schedule_request_tools(request_id)? {
        Some(patterns) => patterns,
        None => {
            fail_run(domain, run_id, "Scheduled Request has no saved launch policy".into())?;
            return Ok(());
        }
    };
    let intent = launch.intent.clone();
    let request_id = request_id.clone();
    let task_id = run.task_id.clone().context("sent run has no Task")?;
    match execute_system(domain, Command::ClaimScheduleLaunch {
        run_id: run_id.clone(), now: alondra_core::now_millis(),
    }) {
        Ok(_) => (),
        Err(error) if ordinary_conflict(&error) => return Ok(()),
        Err(error) => return Err(error.into()),
    }
    let state = domain.state()?;
    let producer = ProducerLaunch { task_id: &task_id, request_id: &request_id, state: &state };
    let (short, settings) = match invoke_claimed(&intent, &producer, &policy, home, &run_id.0, launcher).await {
        Ok(launched) => launched,
        Err((_, reason)) => {
            fail_run(domain, run_id, reason)?;
            return Ok(());
        }
    };
    // `claude --bg` picks the session id, so the Agent is the session Claude lists.
    let session = match &short {
        Some(short) => launcher.session_of(short).await,
        None => None,
    };
    let Some(session) = session else {
        crate::domain_sessions::remove(&settings);
        fail_run(domain, run_id, "Claude did not list the launched session".into())?;
        return Ok(());
    };
    let agent_id = AgentId(session.clone());
    match execute_system(domain, Command::RegisterAgent { agent_id: agent_id.clone(), name: intent.name.clone() }) {
        Ok(_) => (),
        Err(StoreError::Domain(error)) if error.code == "invalid_input" => (),
        Err(error) => return Err(error.into()),
    }
    let bound = domain.bind_agent(&session).map_err(anyhow::Error::from)
        .and_then(|capability| crate::domain_sessions::bind(&settings, &agent_id, capability.expose_for_child_environment()));
    if let Err(error) = bound {
        domain.revoke_agent(&session)?;
        crate::domain_sessions::remove(&settings);
        fail_run(domain, run_id, format!("Could not issue the session's domain capability: {error}"))?;
        return Ok(());
    }
    let handed = execute_system(domain, Command::RecordScheduleLaunchHanded {
        run_id: run_id.clone(), session_id: short, agent_id: Some(agent_id),
        finished_at: alondra_core::now_millis(),
    });
    match handed {
        Ok(_) => (),
        Err(error) if ordinary_conflict(&error) => {
            domain.revoke_agent(&session)?;
            crate::domain_sessions::remove(&settings);
            fail_run(domain, run_id, match error {
                StoreError::Domain(error) => error.message,
                error => error.to_string(),
            })?;
        }
        Err(error) => return Err(error.into()),
    }
    Ok(())
}

pub(crate) async fn run_cycle(
    domain: &DomainRuntime, home: &Path, due_enabled: bool,
    launcher: &impl crate::orchestrate::Launcher,
) -> Result<()> {
    let state = domain.state()?;
    let claimed: Vec<_> = state.schedule.runs.values()
        .filter(|run| run.state == ScheduleRunState::Launching)
        .map(|run| run.id.clone()).collect();
    for run_id in claimed { finish_claim(domain, home, &run_id)?; }
    if due_enabled {
        let now = alondra_core::now_millis();
        let state = domain.state()?;
        let due: Vec<_> = state.schedule.schedules.values()
            .filter(|schedule| schedule.enabled && schedule.deleted_at.is_none()
                && schedule.next_run.is_some_and(|at| at <= now))
            .cloned().collect();
        for schedule in due {
            let run_id: ScheduleRunId = format!("run-{}", crate::auth::random_hex(16)).into();
            let claim = execute_system(domain, Command::ClaimScheduleRun {
                schedule_id: schedule.id.clone(), run_id: run_id.clone(),
                origin: ScheduleRunOrigin::Due, now,
                next_after_now: next_after(&schedule, now),
                expected_schedule_revision: None,
            });
            match claim {
                Ok(_) => finish_claim(domain, home, &run_id)?,
                Err(error) if ordinary_conflict(&error) => (),
                Err(error) => return Err(error.into()),
            }
        }
    }
    let state: DomainState = domain.state()?;
    let pending: Vec<_> = state.schedule.runs.values()
        .filter(|run| run.launch.as_ref().is_some_and(|launch| launch.state == ScheduleLaunchState::Pending))
        .map(|run| run.id.clone()).collect();
    for run_id in pending { dispatch_pending(domain, home, &run_id, launcher).await?; }
    Ok(())
}

pub(crate) async fn run(domain: Arc<DomainRuntime>, home: PathBuf, due_enabled: bool) -> Result<()> {
    let launcher = crate::orchestrate::ClaudeCli;
    loop {
        run_cycle(&domain, &home, due_enabled, &launcher).await?;
        tokio::time::sleep(Duration::from_secs(15)).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alondra_core::domain_v2::{Schedule, ScheduleFrequency};
    use std::collections::BTreeMap;
    use std::sync::Mutex;

    const SESSION: &str = "ab1234cd-0000-4000-8000-000000000000";

    struct FakeLauncher(Mutex<Vec<(String, Vec<String>)>>);
    impl crate::orchestrate::Launcher for FakeLauncher {
        async fn run(&self, cwd: &str, args: &[String]) -> std::result::Result<String, crate::orchestrate::Refusal> {
            self.0.lock().unwrap().push((cwd.into(), args.to_vec()));
            Ok("backgrounded · ab1234 · scheduled/run-1".into())
        }
        async fn run_with_env(&self, _: &str, _: &[String], _: &[(String, String)]) -> std::result::Result<String, crate::orchestrate::Refusal> {
            panic!("a background session never sees the launcher's environment");
        }
        fn usage(&self) -> Option<alondra_core::usage::Usage> { None }
        fn plugin(&self) -> Option<std::path::PathBuf> { Some("/synthetic/claude-plugin".into()) }
    }

    #[test]
    fn calendar_matches_legacy_for_manual_disabled_and_due_slots() {
        let mut schedule = Schedule {
            id: "schedule-1".into(), title: "Review".into(), text: "Review project".into(),
            project: None, options: BTreeMap::new(), disallowed_tools: vec![],
            frequency: ScheduleFrequency::EveryHours, hour: 9, minute: 0,
            weekday: None, interval: Some(2), start_at: Some(1_000),
            enabled: true, next_run: Some(1_000), revision: 1, deleted_at: None,
        };
        assert_eq!(next_after(&schedule, 1_001), Some(7_201_000));
        schedule.enabled = false;
        schedule.next_run = None;
        assert_eq!(next_after(&schedule, 1_001), None);
        schedule.enabled = true;
        schedule.next_run = Some(1_000);
        schedule.frequency = ScheduleFrequency::Once;
        schedule.interval = None;
        schedule.start_at = Some(1_000);
        assert_eq!(next_after(&schedule, 1_000), None);
    }

    #[tokio::test]
    async fn captured_launch_uses_request_policy_and_explicit_new_node_command() {
        let root = std::env::temp_dir().join(format!("alondra-schedule-prep-{}", crate::auth::random_hex(8)));
        let project = root.join("App");
        std::fs::create_dir_all(project.join(".git")).unwrap();
        let definition = ScheduleDefinitionSnapshot {
            title: "Review".into(), text: "Review project".into(), project: Some("App".into()),
            options: BTreeMap::new(), disallowed_tools: vec!["Bash".into()],
        };
        let intent = prepare_intent(
            &ScheduleId::from("schedule-1"), &ScheduleRunId::from("run-1"),
            &definition, &OptionsFile::default(), &root,
        ).unwrap();
        let fake = FakeLauncher(Mutex::new(vec![]));
        let state = DomainState::new("epoch");
        let producer = ProducerLaunch { task_id: &"task-1".into(), request_id: &"request-1".into(), state: &state };
        let home = root.join("home");
        let (short, settings) = invoke_claimed(&intent, &producer, &definition.disallowed_tools, &home, "run-1", &fake).await.unwrap();
        assert_eq!(short.as_deref(), Some("ab1234"));
        let calls = fake.0.lock().unwrap();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].0, project.to_str().unwrap());
        let args = &calls[0].1;
        assert!(args.windows(2).any(|pair| pair == ["--name", "scheduled/schedule-run-1"]));
        assert!(!args.iter().any(|arg| arg == "--session-id"), "claude --bg ignores it");
        assert!(args.windows(2).any(|pair| pair == ["--disallowedTools", "Bash"]));
        assert!(args.windows(2).any(|pair| pair == ["--settings", settings.to_str().unwrap()]));
        let written: serde_json::Value = serde_json::from_slice(&std::fs::read(&settings).unwrap()).unwrap();
        assert_eq!(written["env"]["ALONDRA_DOMAIN_PROTOCOL"], "2");
        let prompt = args.last().unwrap();
        assert!(prompt.starts_with("Review project\n") && prompt.contains("request-1") && prompt.contains("task-1")
            && prompt.contains("data_epoch=epoch"), "{prompt}");
        assert!(!prompt.contains("auto-merge"), "{prompt}");
        drop(calls);
        std::fs::remove_dir_all(root).unwrap();
    }

    struct Fixture { root: PathBuf, home: PathBuf, domain: DomainRuntime }
    impl Fixture {
        fn new() -> Self {
            let root = std::env::temp_dir().join(format!("alondra-schedule-runner-{}", crate::auth::random_hex(8)));
            let home = root.join("home");
            let projects = root.join("projects");
            std::fs::create_dir_all(&home).unwrap();
            std::fs::create_dir(&projects).unwrap();
            std::fs::write(options::path_in(&home),
                format!("projects_root = {:?}\n", projects.to_str().unwrap())).unwrap();
            let domain = DomainRuntime::open(&home).unwrap();
            execute_system(&domain, Command::CreateSchedule { schedule: Schedule {
                id: "schedule-1".into(), title: "Review".into(), text: "Review project".into(),
                project: None, options: BTreeMap::new(), disallowed_tools: vec!["Bash".into()],
                frequency: ScheduleFrequency::Daily, hour: 9, minute: 0,
                weekday: None, interval: None, start_at: None,
                enabled: false, next_run: None, revision: 1, deleted_at: None,
            }}).unwrap();
            Self { root, home, domain }
        }
        fn claim(&self, id: &str) {
            execute_system(&self.domain, Command::ClaimScheduleRun {
                schedule_id: "schedule-1".into(), run_id: id.into(),
                origin: ScheduleRunOrigin::Manual, now: alondra_core::now_millis(),
                next_after_now: None,
                expected_schedule_revision: None,
            }).unwrap();
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) { std::fs::remove_dir_all(&self.root).unwrap(); }
    }

    struct CountingLauncher { calls: Mutex<usize>, fail: bool }
    impl crate::orchestrate::Launcher for CountingLauncher {
        async fn run(&self, _: &str, args: &[String]) -> std::result::Result<String, crate::orchestrate::Refusal> {
            *self.calls.lock().unwrap() += 1;
            assert!(args.iter().any(|arg| arg == "--settings"));
            tokio::task::yield_now().await;
            if self.fail { Err((StatusCode::BAD_GATEWAY, "synthetic Claude failure".into())) }
            else { Ok("backgrounded · ab1234 · scheduled/run".into()) }
        }
        async fn session_of(&self, short: &str) -> Option<String> {
            (short == "ab1234").then(|| SESSION.to_owned())
        }
        fn usage(&self) -> Option<alondra_core::usage::Usage> { None }
        fn plugin(&self) -> Option<PathBuf> { Some("/synthetic/plugin".into()) }
    }

    fn settings_files(home: &Path) -> Vec<serde_json::Value> {
        std::fs::read_dir(home.join("runtime/sessions")).map(|entries| entries.map(|entry| {
            serde_json::from_slice(&std::fs::read(entry.unwrap().path()).unwrap()).unwrap()
        }).collect()).unwrap_or_default()
    }

    #[tokio::test]
    async fn competing_runners_claim_one_external_dispatch() {
        let fixture = Fixture::new();
        fixture.claim("manual-1");
        let launcher = CountingLauncher { calls: Mutex::new(0), fail: false };
        let (left, right) = tokio::join!(
            run_cycle(&fixture.domain, &fixture.home, false, &launcher),
            run_cycle(&fixture.domain, &fixture.home, false, &launcher),
        );
        left.unwrap(); right.unwrap();
        assert_eq!(*launcher.calls.lock().unwrap(), 1);
        let state = fixture.domain.state().unwrap();
        let run = &state.schedule.runs[&ScheduleRunId::from("manual-1")];
        assert_eq!(run.state, ScheduleRunState::Sent);
        assert_eq!(run.launch.as_ref().unwrap().state, ScheduleLaunchState::Handed);
        assert_eq!(state.tasks.len(), 1);
        assert_eq!(state.requests.len(), 1);
        let session = SESSION.to_owned();
        let task = state.tasks.values().next().unwrap();
        assert_eq!(task.agent_id.as_ref().map(|id| id.0.as_str()), Some(session.as_str()));
        assert_eq!(run.launch.as_ref().unwrap().agent_id.as_ref().map(|id| id.0.as_str()), Some(session.as_str()));
        assert_eq!(state.agents[&AgentId(session.clone())].name, run.launch.as_ref().unwrap().intent.name);
        let registry = std::fs::read_to_string(fixture.home.join("domain-capabilities.json")).unwrap();
        assert!(registry.contains(&session), "the session Claude listed holds a durable bearer");
        let settings = settings_files(&fixture.home);
        assert_eq!(settings.len(), 1);
        assert_eq!(settings[0]["env"]["ALONDRA_DOMAIN_AGENT"], SESSION);
        assert_eq!(settings[0]["env"]["ALONDRA_DOMAIN_CAPABILITY"].as_str().unwrap().len(), 64);
        run_cycle(&fixture.domain, &fixture.home, false, &launcher).await.unwrap();
        assert_eq!(*launcher.calls.lock().unwrap(), 1);
    }

    #[tokio::test]
    async fn pending_resumes_but_dispatching_becomes_uncertain_after_restart() {
        let fixture = Fixture::new();
        fixture.claim("pending");
        finish_claim(&fixture.domain, &fixture.home, &"pending".into()).unwrap();
        let reopened = DomainRuntime::open(&fixture.home).unwrap();
        let launcher = CountingLauncher { calls: Mutex::new(0), fail: false };
        run_cycle(&reopened, &fixture.home, false, &launcher).await.unwrap();
        assert_eq!(*launcher.calls.lock().unwrap(), 1);

        fixture.claim("uncertain");
        finish_claim(&fixture.domain, &fixture.home, &"uncertain".into()).unwrap();
        execute_system(&fixture.domain, Command::ClaimScheduleLaunch {
            run_id: "uncertain".into(), now: alondra_core::now_millis(),
        }).unwrap();
        let restarted = DomainRuntime::open(&fixture.home).unwrap();
        let run = &restarted.state().unwrap().schedule.runs[&ScheduleRunId::from("uncertain")];
        assert_eq!(run.state, ScheduleRunState::Sent);
        assert_eq!(run.launch.as_ref().unwrap().state, ScheduleLaunchState::Uncertain);
        run_cycle(&restarted, &fixture.home, false, &launcher).await.unwrap();
        assert_eq!(*launcher.calls.lock().unwrap(), 1);
    }

    #[tokio::test]
    async fn returned_failure_commits_run_launch_and_marker_together() {
        let fixture = Fixture::new();
        fixture.claim("failed");
        let launcher = CountingLauncher { calls: Mutex::new(0), fail: true };
        run_cycle(&fixture.domain, &fixture.home, false, &launcher).await.unwrap();
        assert_eq!(*launcher.calls.lock().unwrap(), 1);
        let state = fixture.domain.state().unwrap();
        let run = &state.schedule.runs[&ScheduleRunId::from("failed")];
        assert_eq!(run.state, ScheduleRunState::Failed);
        assert_eq!(run.launch.as_ref().unwrap().state, ScheduleLaunchState::Failed);
        let request_id = run.request_id.as_ref().unwrap();
        assert!(fixture.domain.was_schedule_launch_failed(request_id).unwrap());
        assert!(state.tasks[run.task_id.as_ref().unwrap()].agent_id.is_none(), "a failed launch assigns nothing");
        assert!(state.agents.is_empty(), "no Agent before Claude reports a session");
        assert!(settings_files(&fixture.home).is_empty(), "a failed launch leaves no settings file");
    }

    struct Unlisted;
    impl crate::orchestrate::Launcher for Unlisted {
        async fn run(&self, _: &str, _: &[String]) -> std::result::Result<String, crate::orchestrate::Refusal> {
            Ok("backgrounded · ab1234 · scheduled/run".into())
        }
        async fn session_of(&self, _: &str) -> Option<String> { None }
        fn usage(&self) -> Option<alondra_core::usage::Usage> { None }
        fn plugin(&self) -> Option<PathBuf> { Some("/synthetic/plugin".into()) }
    }

    #[tokio::test]
    async fn a_session_claude_does_not_list_fails_the_run_without_an_agent() {
        let fixture = Fixture::new();
        fixture.claim("unlisted");
        run_cycle(&fixture.domain, &fixture.home, false, &Unlisted).await.unwrap();
        let state = fixture.domain.state().unwrap();
        let run = &state.schedule.runs[&ScheduleRunId::from("unlisted")];
        assert_eq!(run.state, ScheduleRunState::Failed);
        assert!(state.agents.is_empty());
        assert!(settings_files(&fixture.home).is_empty());
    }

    #[tokio::test]
    async fn failed_marker_write_rolls_back_run_and_launch_result_without_retrying_claude() {
        let fixture = Fixture::new();
        fixture.claim("rollback");
        let conn = rusqlite::Connection::open(fixture.home.join("domain-v2.db")).unwrap();
        conn.execute_batch("CREATE TRIGGER reject_failed_marker BEFORE INSERT ON schedule_failed_launches
            BEGIN SELECT RAISE(ABORT,'synthetic marker failure'); END;").unwrap();
        let launcher = CountingLauncher { calls: Mutex::new(0), fail: true };
        assert!(run_cycle(&fixture.domain, &fixture.home, false, &launcher).await.is_err());
        assert_eq!(*launcher.calls.lock().unwrap(), 1);
        let state = fixture.domain.state().unwrap();
        let run = &state.schedule.runs[&ScheduleRunId::from("rollback")];
        assert_eq!(run.state, ScheduleRunState::Sent);
        assert_eq!(run.launch.as_ref().unwrap().state, ScheduleLaunchState::Dispatching);
        assert!(!fixture.domain.was_schedule_launch_failed(run.request_id.as_ref().unwrap()).unwrap());
        drop(conn);
        let restarted = DomainRuntime::open(&fixture.home).unwrap();
        assert_eq!(restarted.state().unwrap().schedule.runs[&ScheduleRunId::from("rollback")]
            .launch.as_ref().unwrap().state, ScheduleLaunchState::Uncertain);
        run_cycle(&restarted, &fixture.home, false, &launcher).await.unwrap();
        assert_eq!(*launcher.calls.lock().unwrap(), 1);
    }
}
