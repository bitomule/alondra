//! Schedule v2's local-calendar and launch preparation adapter. The command
//! carrying this intent must be committed with its Run, Request, and Task before
//! a technical launch claim can call Claude.
use crate::domain_runtime::DomainRuntime;
use crate::domain_store::StoreError;
use crate::launch_worktree::Workplace;
use alondra_core::domain::{
    Actor, AgentId, Command, CommandEnvelope, CommandOutcome, DomainState, PROTOCOL_VERSION,
    Request, RequestId, ScheduleLaunchState, ScheduleRunOrigin, ScheduleRunState, Task,
    TaskId, TaskState,
};
use alondra_core::domain::{
    ScheduleDefinitionSnapshot, ScheduleFrequency, ScheduleId, ScheduleLaunchIntent, ScheduleRunId,
};
use alondra_core::options::Resolved;
use alondra_core::options::{self, OptionsFile};
use anyhow::{Context, Result};
use axum::http::StatusCode;
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;

pub(crate) fn next_after(schedule: &alondra_core::domain::Schedule, now: i64) -> Option<i64> {
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
    launch
        .validate(definition)
        .map_err(|error| error.to_string())?;
    Ok(launch)
}

/// The mission a scheduled node starts with, followed by the choices made for it.
pub(crate) fn producer_prompt(
    text: &str,
    resolved: &Resolved,
    task: &TaskId,
    approach: alondra_core::options::Approach,
) -> String {
    let user = crate::domain_messages::user_name();
    let mut lines = vec![
        text.trim_end().to_owned(),
        String::new(),
        "## Your Alondra task".into(),
        String::new(),
        format!("This work is Alondra task {task}; pass it as task_id to the Alondra tools. Your terminal does not reach {user}: only the Alondra tools do, and {user}'s answers are typed into this session. Use `ask` for a decision and end your turn after it; a plan goes through plan mode (`EnterPlanMode`, then `ExitPlanMode`, which {user} approves or sends back in Alondra), never a document or an `ask`. Use `request_action` for something {user} does outside Alondra, and `publish_information` for a finding {user} should read now. When you are done, deliver it with `present_result` and is_final true (the answer in body, files in attachments, a PR you registered or a document revision in targets) and end your turn: {user} accepts it in Alondra. A file or a final message left in this session is not a delivery."),
        String::new(),
        "## Choices for this work".into(),
        String::new(),
        "The user chose these options for this work:".into(),
    ];
    if resolved.auto_merge {
        lines.push("- PR handling: auto-merge. Register the PR and deliver it with present_result as your final result: once its checks pass, Alondra merges it and tells you. Do not merge it yourself.".into());
    }
    for picked in resolved
        .picked
        .iter()
        .filter(|p| p.id != alondra_core::options::PR && p.id != alondra_core::options::EFFORT)
    {
        lines.push(match &picked.mission {
            Some(mission) => format!("- {}: {}. {mission}", picked.label, picked.value_label),
            None => format!("- {}: {}.", picked.label, picked.value_label),
        });
    }
    if let Some(issue) = &resolved.issue {
        lines.push(format!("- This work comes from issue {issue}: write `Fixes {issue}` in the body of the PR that solves it."));
    }
    lines.push(String::new());
    lines.extend(approach.how_to_work());
    lines.join("\n")
}

/// Invoke only after the technical intent has been claimed durably. The caller
/// records the result against that claim; this function never retries Claude.
async fn invoke_claimed(
    launch: &ScheduleLaunchIntent,
    task: &TaskId,
    disallowed_tools: &[String],
    home: &Path,
    key: &str,
    launcher: &impl crate::orchestrate::Launcher,
) -> Result<(String, PathBuf, Workplace), crate::orchestrate::Refusal> {
    if !Path::new(&launch.cwd).is_dir() {
        return Err((
            StatusCode::BAD_REQUEST,
            "schedule project directory does not exist".into(),
        ));
    }
    if let Some(repository) = crate::claude_trust::check(Path::new(&launch.cwd)) {
        return Err((
            StatusCode::CONFLICT,
            crate::claude_trust::instructions(&repository),
        ));
    }
    let workplace = crate::launch_worktree::isolate(Path::new(&launch.cwd), &crate::launch_worktree::slug(&task.0), None, None)
        .await
        .map_err(|why| (StatusCode::CONFLICT, why))?;
    let Some(plugin) = launcher.plugin() else {
        return Err((
            StatusCode::SERVICE_UNAVAILABLE,
            "Alondra's Claude plugin is unavailable".into(),
        ));
    };
    let resolved: Resolved = serde_json::from_str(&launch.options_json)
        .map_err(|error| (StatusCode::INTERNAL_SERVER_ERROR, error.to_string()))?;
    let settings =
        crate::domain_sessions::prepare(home, key).map_err(|error| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("Could not write the session's settings: {error}"),
            )
        })?;
    let mut args: Vec<String> = ["--bg", "--name", &launch.name, "--permission-mode", "auto"]
        .map(String::from)
        .into();
    args.extend(resolved.launch_flags(None));
    args.extend(crate::domain_sessions::flags(
        &[plugin.as_path()],
        &settings,
    ));
    if !disallowed_tools.is_empty() {
        args.extend(["--disallowedTools".into(), disallowed_tools.join(" ")]);
    }
    args.push("--".into());
    args.push(producer_prompt(&launch.prompt, &resolved, task, alondra_core::options::Approach::Solo));
    match launcher.run(&workplace.dir().to_string_lossy(), &args).await {
        Ok(output) => Ok((output, settings, workplace)),
        Err(refusal) => {
            crate::domain_sessions::release(&settings);
            Err(refusal)
        }
    }
}

fn execute_system(domain: &DomainRuntime, command: Command) -> Result<CommandOutcome, StoreError> {
    let command_id: alondra_core::domain::CommandId =
        format!("schedule-worker-{}", crate::auth::random_hex(16)).into();
    for _ in 0..5 {
        let state = domain.state().map_err(StoreError::Storage)?;
        let envelope = CommandEnvelope {
            protocol_version: PROTOCOL_VERSION,
            data_epoch: state.data_epoch.clone(),
            command_id: command_id.clone(),
            expected_revision: state.revision,
            command: command.clone(),
        };
        match domain.execute(&Actor::System, &envelope, None, || Ok(())) {
            Err(StoreError::Domain(error)) if error.code == "stale_revision" => continue,
            result => return result,
        }
    }
    Err(alondra_core::domain::DomainError::new(
        "stale_revision",
        "Schedule writer remained busy",
    )
    .into())
}

fn ordinary_conflict(error: &StoreError) -> bool {
    matches!(error, StoreError::Domain(domain) if matches!(domain.code.as_str(), "conflict" | "not_found" | "stale_revision"))
}

pub(crate) fn options_at(home: &Path) -> Result<OptionsFile> {
    match std::fs::read_to_string(options::path_in(home)) {
        Ok(contents) => OptionsFile::parse(&contents).map_err(anyhow::Error::msg),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(OptionsFile::default()),
        Err(error) => Err(error.into()),
    }
}

fn request_task(snapshot: &ScheduleDefinitionSnapshot) -> (Request, Task) {
    let request_id: RequestId = format!("r-{}", crate::auth::random_hex(16)).into();
    let request = Request::new(request_id.clone(), snapshot.text.clone());
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
        model: None,
        delivered_dependencies: Default::default(), blocks: Default::default(), verdict: None, review_rounds: 0, wrap_up: Default::default(), priority: Default::default(),
    };
    (request, task)
}

fn fail_run(domain: &DomainRuntime, run_id: &ScheduleRunId, reason: String) -> Result<()> {
    log!("scheduled run {run_id} could not start: {}", reason.trim());
    match execute_system(
        domain,
        Command::RecordScheduleRunFailed {
            run_id: run_id.clone(),
            reason,
            finished_at: alondra_core::now_millis(),
        },
    ) {
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
    let Some(run) = state
        .schedule
        .runs
        .get(run_id)
        .filter(|run| run.state == ScheduleRunState::Launching)
    else {
        return Ok(());
    };
    let snapshot = run
        .definition_snapshot
        .as_ref()
        .context("claimed Run has no definition")?;
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
    let (mut request, task) = request_task(snapshot);
    request.auto_merge = serde_json::from_str::<Resolved>(&launch.options_json)
        .is_ok_and(|resolved| resolved.auto_merge);
    let sent = execute_system(
        domain,
        Command::RecordScheduleRunSent {
            run_id: run_id.clone(),
            request,
            task,
            launch,
            finished_at: alondra_core::now_millis(),
        },
    );
    if let Err(error) = sent {
        if ordinary_conflict(&error) {
            return Ok(());
        }
        let reason = format!("Could not record scheduled Request and Task: {error}");
        fail_run(domain, run_id, reason)?;
    }
    Ok(())
}

/// The successful ClaimScheduleLaunch commit is the irreversible boundary:
/// exactly one caller may invoke Claude, even if the process dies afterward.
async fn dispatch_pending(
    domain: &DomainRuntime,
    home: &Path,
    run_id: &ScheduleRunId,
    launcher: &impl crate::orchestrate::Launcher,
) -> Result<()> {
    let state = domain.state()?;
    let Some(run) = state
        .schedule
        .runs
        .get(run_id)
        .filter(|run| run.state == ScheduleRunState::Sent)
    else {
        return Ok(());
    };
    let Some(launch) = run
        .launch
        .as_ref()
        .filter(|launch| launch.state == ScheduleLaunchState::Pending)
    else {
        return Ok(());
    };
    let request_id = run.request_id.as_ref().context("sent run has no Request")?;
    let policy = match domain.schedule_request_tools(request_id)? {
        Some(patterns) => patterns,
        None => {
            fail_run(
                domain,
                run_id,
                "Scheduled Request has no saved launch policy".into(),
            )?;
            return Ok(());
        }
    };
    if domain.live.usage_pause.is_active() || domain.live.machine_watch.disk_low() {
        return Ok(());
    }
    let now = alondra_core::now_millis();
    let waiting = crate::fleet_gate::Waiting {
        key: crate::fleet_gate::Key::Run(run_id.0.clone()),
        priority: crate::fleet_gate::run_priority(&state, run),
        resuming: false,
        since: run.claimed_at.unwrap_or(now),
    };
    if domain.live.gate.ask(domain, waiting, now).is_err() {
        return Ok(());
    }
    let intent = launch.intent.clone();
    let task = run.task_id.clone().context("sent run has no Task")?;
    match execute_system(
        domain,
        Command::ClaimScheduleLaunch {
            run_id: run_id.clone(),
            now: alondra_core::now_millis(),
        },
    ) {
        Ok(_) => (),
        Err(error) if ordinary_conflict(&error) => return Ok(()),
        Err(error) => return Err(error.into()),
    }
    let (output, settings, workplace) =
        match invoke_claimed(&intent, &task, &policy, home, &run_id.0, launcher).await {
            Ok(launched) => launched,
            Err((_, reason)) => {
                fail_run(domain, run_id, reason)?;
                return Ok(());
            }
        };
    let session = match crate::mods_wait::wait(&domain.live.mods, &run_id.0, launcher).await {
        Ok(session) => session,
        Err(waited) => {
            if let Some(short) = alondra_core::sessions::launched_short(&output) {
                launcher.stop(&short).await;
            }
            crate::domain_sessions::release(&settings);
            let reason = crate::mods_wait::not_loaded(Path::new(&intent.cwd), &waited, launcher).await;
            fail_run(domain, run_id, reason)?;
            return Ok(());
        }
    };
    if let Some(dir) = workplace.chosen().map(|dir| dir.to_string_lossy().into_owned()) {
        if let Err(error) = domain.set_inherited_worktree(&session, &dir, "alondra") {
            log!("could not record that {session} works in {dir}: {error:#}");
        }
        if let Err(error) = domain.set_task_worktree(&task, &dir) {
            log!("could not record that {task} works in {dir}: {error:#}");
        }
    }
    hand_over(domain, run_id, &intent.name, &session, &settings)
}

fn task_is_closed(domain: &DomainRuntime, run_id: &ScheduleRunId) -> Result<bool> {
    let state = domain.state()?;
    Ok(state
        .schedule
        .runs
        .get(run_id)
        .and_then(|run| run.task_id.as_ref())
        .and_then(|task| state.tasks.get(task))
        .is_some_and(|task| task.state.terminal()))
}

/// The launched session is known: it becomes the run's Agent, its bearer is written into the
/// settings file it reads, and the run is handed.
fn hand_over(
    domain: &DomainRuntime,
    run_id: &ScheduleRunId,
    name: &str,
    session: &str,
    settings: &Path,
) -> Result<()> {
    if task_is_closed(domain, run_id)? {
        crate::domain_sessions::release(settings);
        return fail_run(domain, run_id, "its Task was closed".into());
    }
    let agent_id = AgentId(session.to_owned());
    match execute_system(
        domain,
        Command::RegisterAgent {
            agent_id: agent_id.clone(),
            name: name.to_owned(),
        },
    ) {
        Ok(_) => (),
        Err(StoreError::Domain(error)) if error.code == "invalid_input" => (),
        Err(error) => return Err(error.into()),
    }
    let bound = domain
        .bind_agent(session)
        .map_err(anyhow::Error::from)
        .and_then(|capability| {
            crate::domain_sessions::bind(
                settings,
                &agent_id,
                capability.expose_for_child_environment(),
            )
        });
    if let Err(error) = bound {
        domain.revoke_agent(session)?;
        crate::domain_sessions::release(settings);
        fail_run(
            domain,
            run_id,
            format!("Could not issue the session's domain capability: {error}"),
        )?;
        return Ok(());
    }
    let handed = execute_system(
        domain,
        Command::RecordScheduleLaunchHanded {
            run_id: run_id.clone(),
            session_id: Some(session.to_owned()),
            agent_id: Some(agent_id),
            finished_at: alondra_core::now_millis(),
        },
    );
    match handed {
        Ok(_) => (),
        Err(error) if ordinary_conflict(&error) => {
            domain.revoke_agent(session)?;
            crate::domain_sessions::release(settings);
            fail_run(
                domain,
                run_id,
                match error {
                    StoreError::Domain(error) => error.message,
                    error => error.to_string(),
                },
            )?;
        }
        Err(error) => return Err(error.into()),
    }
    Ok(())
}

fn settings_path(home: &Path, key: &str) -> Result<PathBuf> {
    let path = home.join("runtime/sessions").join(format!("{key}.json"));
    if !path.exists() {
        return crate::domain_sessions::prepare(home, key);
    }
    Ok(path)
}

/// A launch Alondra died in the middle of: Claude may be running with no bearer. Its mod
/// reports the launch key again when Alondra is back, so the session is looked for; found, it
/// gets its Agent and bearer, and when it never shows up the run fails and says so.
async fn recover_uncertain(
    domain: &DomainRuntime,
    home: &Path,
    run_id: &ScheduleRunId,
    launcher: &impl crate::orchestrate::Launcher,
) -> Result<()> {
    let state = domain.state()?;
    let Some((run, launch)) = state
        .schedule
        .runs
        .get(run_id)
        .filter(|run| run.state == ScheduleRunState::Sent)
        .and_then(|run| Some((run, run.launch.as_ref()?)))
        .filter(|(_, launch)| launch.state == ScheduleLaunchState::Uncertain)
    else {
        return Ok(());
    };
    let name = launch.intent.name.clone();
    let settings = settings_path(home, &run_id.0)?;
    match crate::mods_wait::wait(&domain.live.mods, &run_id.0, launcher).await {
        Ok(session) => hand_over(domain, run_id, &name, &session, &settings),
        Err(_) => {
            launcher.stop(&name).await;
            crate::domain_sessions::release(&settings);
            let title = run.definition_snapshot.as_ref().map_or(name.as_str(), |snapshot| snapshot.title.as_str());
            fail_run(
                domain,
                run_id,
                format!("The scheduled run \"{title}\" did not start properly: Alondra stopped while launching it and the session never reported back. Try it again."),
            )
        }
    }
}

/// The user's answer to a scheduled run that did not start. "Try again" launches its schedule
/// once more (the retry's run id derives from the failed one, so answering twice launches once);
/// either way the failed run's Task never started and is cancelled, so it does not wait forever.
fn answer_failed_launches(domain: &DomainRuntime, home: &Path) -> Result<()> {
    use alondra_core::domain::{InteractionPayload, LAUNCH_PROBLEM_PREFIX, LAUNCH_RETRY_OPTION};
    let state = domain.state()?;
    for (id, interaction) in &state.interactions {
        let Some(failed) = id.0.strip_prefix(LAUNCH_PROBLEM_PREFIX) else { continue };
        let InteractionPayload::Question { answer: Some(answer), .. } = &interaction.payload else { continue };
        if interaction.withdrawn {
            continue;
        }
        let Some(run) = state.schedule.runs.get(&ScheduleRunId::from(failed)) else { continue };
        let retried = if answer.option_id == LAUNCH_RETRY_OPTION {
            retry_launch(domain, home, &state, failed, run)
        } else {
            true
        };
        let Some(task) = run.task_id.as_ref().filter(|_| retried) else { continue };
        match execute_system(domain, Command::CancelTask { task_id: task.clone() }) {
            Ok(_) => (),
            Err(StoreError::Domain(error)) if error.code == "task_not_active" || ordinary_conflict_code(&error.code) => (),
            Err(error) => log!("could not cancel the unstarted Task of scheduled run {failed}: {error}"),
        }
    }
    Ok(())
}

fn ordinary_conflict_code(code: &str) -> bool {
    matches!(code, "conflict" | "not_found" | "stale_revision")
}

/// Whether the schedule is launching again: the retry run exists or was just claimed.
fn retry_launch(
    domain: &DomainRuntime,
    home: &Path,
    state: &DomainState,
    failed: &str,
    run: &alondra_core::domain::ScheduleRun,
) -> bool {
    let retry: ScheduleRunId = format!("retry-{failed}").into();
    if state.schedule.runs.contains_key(&retry) {
        return true;
    }
    let Some(schedule) = state
        .schedule
        .schedules
        .get(&run.schedule_id)
        .filter(|schedule| schedule.deleted_at.is_none())
    else {
        return false;
    };
    let now = alondra_core::now_millis();
    let claimed = execute_system(
        domain,
        Command::ClaimScheduleRun {
            schedule_id: schedule.id.clone(),
            run_id: retry.clone(),
            origin: ScheduleRunOrigin::Manual,
            now,
            next_after_now: next_after(schedule, now),
            expected_schedule_revision: None,
        },
    );
    match claimed {
        Ok(_) => {
            if let Err(error) = finish_claim(domain, home, &retry) {
                log!("retry {retry} of scheduled run {failed} could not be prepared: {error:#}");
            }
            true
        }
        Err(error) => {
            log!("retry of scheduled run {failed} refused: {error}");
            false
        }
    }
}

pub(crate) async fn run_cycle(
    domain: &DomainRuntime,
    home: &Path,
    due_enabled: bool,
    launcher: &impl crate::orchestrate::Launcher,
) -> Result<()> {
    let state = domain.state()?;
    let claimed: Vec<_> = state
        .schedule
        .runs
        .values()
        .filter(|run| run.state == ScheduleRunState::Launching)
        .map(|run| run.id.clone())
        .collect();
    for run_id in claimed {
        finish_claim(domain, home, &run_id)?;
    }
    let uncertain: Vec<_> = domain
        .state()?
        .schedule
        .runs
        .values()
        .filter(|run| {
            run.launch
                .as_ref()
                .is_some_and(|launch| launch.state == ScheduleLaunchState::Uncertain)
        })
        .map(|run| run.id.clone())
        .collect();
    for run_id in uncertain {
        if let Err(error) = recover_uncertain(domain, home, &run_id, launcher).await {
            log!("could not recover scheduled run {run_id}: {error:#}");
        }
    }
    answer_failed_launches(domain, home)?;
    let paused = domain.live.usage_pause.is_active() || domain.live.machine_watch.disk_low();
    if due_enabled && !paused {
        let now = alondra_core::now_millis();
        let state = domain.state()?;
        let due: Vec<_> = state
            .schedule
            .schedules
            .values()
            .filter(|schedule| {
                schedule.enabled
                    && schedule.deleted_at.is_none()
                    && schedule.next_run.is_some_and(|at| at <= now)
            })
            .cloned()
            .collect();
        for schedule in due {
            let run_id: ScheduleRunId = format!("run-{}", crate::auth::random_hex(16)).into();
            let claim = execute_system(
                domain,
                Command::ClaimScheduleRun {
                    schedule_id: schedule.id.clone(),
                    run_id: run_id.clone(),
                    origin: ScheduleRunOrigin::Due,
                    now,
                    next_after_now: next_after(&schedule, now),
                    expected_schedule_revision: None,
                },
            );
            match claim {
                Ok(_) => finish_claim(domain, home, &run_id)?,
                Err(error) if ordinary_conflict(&error) => (),
                Err(error) => return Err(error.into()),
            }
        }
    }
    let state = domain.state()?;
    let pending: Vec<_> = state
        .schedule
        .runs
        .values()
        .filter(|run| {
            run.launch
                .as_ref()
                .is_some_and(|launch| launch.state == ScheduleLaunchState::Pending)
        })
        .map(|run| run.id.clone())
        .collect();
    if paused {
        return Ok(());
    }
    for run_id in pending {
        dispatch_pending(domain, home, &run_id, launcher).await?;
    }
    Ok(())
}

fn nearest_run(state: &DomainState, due_enabled: bool) -> Option<i64> {
    if !due_enabled {
        return None;
    }
    state
        .schedule
        .schedules
        .values()
        .filter(|schedule| schedule.enabled && schedule.deleted_at.is_none())
        .filter_map(|schedule| schedule.next_run)
        .min()
}

fn rebase_runs(domain: &DomainRuntime, include_due: bool) -> Result<()> {
    let now = alondra_core::now_millis();
    let state = domain.state()?;
    let launching: BTreeSet<_> = state
        .schedule
        .runs
        .values()
        .filter(|run| run.state == ScheduleRunState::Launching)
        .map(|run| run.schedule_id.clone())
        .collect();
    for rebased in rebased_schedules(&state, &launching, now, include_due) {
        match execute_system(domain, Command::UpdateSchedule { schedule: rebased }) {
            Ok(_) => (),
            Err(error) if ordinary_conflict(&error) => (),
            Err(error) => return Err(error.into()),
        }
    }
    Ok(())
}

fn rebased_schedules(
    state: &DomainState,
    launching: &BTreeSet<ScheduleId>,
    now: i64,
    include_due: bool,
) -> Vec<alondra_core::domain::Schedule> {
    state
        .schedule
        .schedules
        .values()
        .filter_map(|schedule| {
            if !schedule.enabled
                || schedule.deleted_at.is_some()
                || (!include_due && !schedule.next_run.is_some_and(|at| at > now))
                || launching.contains(&schedule.id)
            {
                return None;
            }
            let next = next_after(schedule, now);
            if next == schedule.next_run {
                return None;
            }
            let mut rebased = schedule.clone();
            rebased.revision += 1;
            rebased.next_run = next;
            Some(rebased)
        })
        .collect()
}

enum WakeReason {
    Schedule,
    Clock(crate::schedule_clock::ClockChange),
    Deadline,
    Closed,
}

async fn wait_for_next_run(
    deadline: Option<i64>,
    schedules: &mut tokio::sync::watch::Receiver<alondra_core::domain::Revision>,
    usage: &mut tokio::sync::watch::Receiver<Option<crate::usage_pause::Exhausted>>,
    clock_changes: &mut tokio::sync::watch::Receiver<crate::schedule_clock::ClockChange>,
    turn: &mut tokio::sync::watch::Receiver<u64>,
) -> WakeReason {
    if let Some(deadline) = deadline {
        let (_timer, mut timer_fired) = crate::schedule_clock::WallTimer::at(deadline);
        tokio::select! {
            result = schedules.changed() => if result.is_ok() { WakeReason::Schedule } else { WakeReason::Closed },
            result = usage.changed() => if result.is_ok() { WakeReason::Schedule } else { WakeReason::Closed },
            result = turn.changed() => if result.is_ok() { WakeReason::Schedule } else { WakeReason::Closed },
            result = clock_changes.changed() => if result.is_ok() { WakeReason::Clock(*clock_changes.borrow_and_update()) } else { WakeReason::Closed },
            result = timer_fired.changed() => if result.is_ok() { WakeReason::Deadline } else { WakeReason::Closed },
        }
    } else {
        tokio::select! {
            result = schedules.changed() => if result.is_ok() { WakeReason::Schedule } else { WakeReason::Closed },
            result = usage.changed() => if result.is_ok() { WakeReason::Schedule } else { WakeReason::Closed },
            result = turn.changed() => if result.is_ok() { WakeReason::Schedule } else { WakeReason::Closed },
            result = clock_changes.changed() => if result.is_ok() { WakeReason::Clock(*clock_changes.borrow_and_update()) } else { WakeReason::Closed },
        }
    }
}

pub(crate) async fn run(
    domain: Arc<DomainRuntime>,
    home: PathBuf,
    due_enabled: bool,
) -> Result<()> {
    let launcher = crate::orchestrate::ClaudeCli;
    let mut schedule_changes = domain.subscribe_schedules();
    let mut usage_changes = domain.live.usage_pause.subscribe();
    let mut turn = domain.live.gate.subscribe();
    let mut system_clock = crate::schedule_clock::ClockChanges::new();
    loop {
        run_cycle(&domain, &home, due_enabled, &launcher).await?;
        let deadline = if domain.live.usage_pause.is_active() { None } else { nearest_run(&*domain.state()?, due_enabled) };
        match wait_for_next_run(
            deadline,
            &mut schedule_changes,
            &mut usage_changes,
            system_clock.receiver(),
            &mut turn,
        )
        .await
        {
            WakeReason::Clock(crate::schedule_clock::ClockChange::SystemClock) => {
                run_cycle(&domain, &home, due_enabled, &launcher).await?;
                rebase_runs(&domain, false)?;
            }
            WakeReason::Clock(crate::schedule_clock::ClockChange::TimeZone) => {
                rebase_runs(&domain, true)?
            }
            WakeReason::Closed => return Ok(()),
            WakeReason::Schedule | WakeReason::Deadline => (),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alondra_core::domain::{InteractionId, InteractionPayload, Schedule, ScheduleFrequency};
    use std::collections::BTreeMap;
    use std::sync::Mutex;

    const SESSION: &str = "ab1234cd-0000-4000-8000-000000000000";
    const FOUND_SESSION: &str = "cd5678ef-0000-4000-8000-000000000000";

    struct FakeLauncher(Mutex<Vec<(String, Vec<String>)>>);
    impl crate::orchestrate::Launcher for FakeLauncher {
        async fn run(
            &self,
            cwd: &str,
            args: &[String],
        ) -> std::result::Result<String, crate::orchestrate::Refusal> {
            self.0.lock().unwrap().push((cwd.into(), args.to_vec()));
            Ok("backgrounded · ab1234 · scheduled/run-1".into())
        }
        async fn run_with_env(
            &self,
            _: &str,
            _: &[String],
            _: &[(String, String)],
        ) -> std::result::Result<String, crate::orchestrate::Refusal> {
            panic!("a background session never sees the launcher's environment");
        }
        async fn stop(&self, _: &str) {}
        fn plugin(&self) -> Option<std::path::PathBuf> {
            Some("/synthetic/claude-plugin".into())
        }
    }

    #[test]
    fn an_auto_merge_request_keeps_the_merge_with_alondra() {
        let file = OptionsFile::default();
        let chosen = |pr: &str| {
            file.resolve(
                None,
                &BTreeMap::from([(
                    alondra_core::options::PR.to_owned(),
                    alondra_core::options::Choice::Value(pr.into()),
                )]),
            )
            .unwrap()
        };
        let auto = producer_prompt("Fix it", &chosen("auto_merge"), &TaskId::from("t-1"), alondra_core::options::Approach::Solo);
        assert!(auto.contains("once its checks pass, Alondra merges it") && auto.contains("Do not merge it yourself"), "{auto}");
        assert!(!auto.contains("gh pr merge") && !auto.contains("authorization"), "{auto}");
        let review = producer_prompt("Fix it", &chosen("review"), &TaskId::from("t-1"), alondra_core::options::Approach::Solo);
        assert!(!review.contains("auto-merge"), "{review}");
    }

    #[test]
    fn the_mission_names_the_task_and_how_to_deliver_through_alondra() {
        let resolved = OptionsFile::default().resolve(None, &BTreeMap::new()).unwrap();
        let prompt = producer_prompt("Analyse the mods", &resolved, &TaskId::from("t-mods"), alondra_core::options::Approach::Solo);
        assert!(prompt.starts_with("Analyse the mods\n"), "{prompt}");
        assert!(prompt.contains("This work is Alondra task t-mods; pass it as task_id"), "{prompt}");
        assert!(prompt.contains("`present_result` and is_final true") && prompt.contains("`ask`")
            && prompt.contains("is not a delivery"), "{prompt}");
        assert!(!prompt.contains("`report`"), "the old tool does not exist for this session: {prompt}");
    }

    #[test]
    fn calendar_matches_legacy_for_manual_disabled_and_due_slots() {
        let mut schedule = Schedule {
            id: "schedule-1".into(),
            title: "Review".into(),
            text: "Review project".into(),
            project: None,
            options: BTreeMap::new(),
            disallowed_tools: vec![],
            frequency: ScheduleFrequency::EveryHours,
            hour: 9,
            minute: 0,
            weekday: None,
            interval: Some(2),
            start_at: Some(1_000),
            enabled: true,
            next_run: Some(1_000),
            revision: 1,
            deleted_at: None,
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

    #[test]
    fn runner_waits_for_the_nearest_live_schedule_and_rebases_only_future_unclaimed_runs() {
        let mut state = DomainState::new("epoch");
        let mut daily = Schedule {
            id: "daily".into(),
            title: "Daily".into(),
            text: "Review".into(),
            project: None,
            options: BTreeMap::new(),
            disallowed_tools: vec![],
            frequency: ScheduleFrequency::Daily,
            hour: 9,
            minute: 0,
            weekday: None,
            interval: None,
            start_at: None,
            enabled: true,
            next_run: Some(20),
            revision: 4,
            deleted_at: None,
        };
        state
            .schedule
            .schedules
            .insert(daily.id.clone(), daily.clone());
        daily.id = "disabled".into();
        daily.enabled = false;
        daily.next_run = None;
        state
            .schedule
            .schedules
            .insert(daily.id.clone(), daily.clone());
        daily.id = "deleted".into();
        daily.enabled = false;
        daily.next_run = Some(5);
        daily.deleted_at = Some(1);
        state.schedule.schedules.insert(daily.id.clone(), daily);
        assert_eq!(nearest_run(&state, true), Some(20));
        assert_eq!(nearest_run(&state, false), None);

        let mut daily = state.schedule.schedules[&ScheduleId::from("daily")].clone();
        daily.frequency = ScheduleFrequency::EveryDays;
        daily.interval = Some(1);
        daily.start_at = Some(0);
        daily.next_run = Some(1_000_000_000_001);
        let mut rebase_state = DomainState::new("epoch");
        rebase_state
            .schedule
            .schedules
            .insert(daily.id.clone(), daily.clone());
        let excluded = BTreeSet::from([ScheduleId::from("daily")]);
        assert!(rebased_schedules(&rebase_state, &excluded, 1_000_000_000_000, true).is_empty());

        let rebased = rebased_schedules(&rebase_state, &BTreeSet::new(), 1_000_000_000_000, false);
        assert_eq!(rebased.len(), 1);
        assert_eq!(rebased[0].revision, 5);
        assert_ne!(rebased[0].next_run, Some(1_000_000_000_001));
        daily.next_run = Some(999_999_999_999);
        rebase_state
            .schedule
            .schedules
            .insert(daily.id.clone(), daily);
        let after_time_zone_change =
            rebased_schedules(&rebase_state, &BTreeSet::new(), 1_000_000_000_000, true);
        assert_eq!(after_time_zone_change.len(), 1);
        assert!(after_time_zone_change[0].next_run.unwrap() > 1_000_000_000_000);
    }

    #[tokio::test]
    async fn captured_launch_uses_request_policy_and_explicit_new_node_command() {
        let scratch_directory = crate::test_support::Directory::new();
        let root = scratch_directory.parent().unwrap().join(format!(
            "alondra-schedule-prep-{}",
            crate::auth::random_hex(8)
        ));
        let project = root.join("App");
        std::fs::create_dir_all(&project).unwrap();
        for args in [&["init", "-q", "-b", "main"][..], &["-c", "user.name=t", "-c", "user.email=t@t", "commit", "-q", "--allow-empty", "-m", "start"]] {
            assert!(std::process::Command::new("git").arg("-C").arg(&project).args(args).status().unwrap().success());
        }
        let definition = ScheduleDefinitionSnapshot {
            title: "Review".into(),
            text: "Review project".into(),
            project: Some("App".into()),
            options: BTreeMap::new(),
            disallowed_tools: vec!["Bash".into()],
        };
        let intent = prepare_intent(
            &ScheduleId::from("schedule-1"),
            &ScheduleRunId::from("run-1"),
            &definition,
            &OptionsFile::default(),
            &root,
        )
        .unwrap();
        let fake = FakeLauncher(Mutex::new(vec![]));
        let home = root.join("home");
        let (_, settings, _) = invoke_claimed(
            &intent,
            &TaskId::from("task-1"),
            &definition.disallowed_tools,
            &home,
            "run-1",
            &fake,
        )
        .await
        .unwrap();
        let calls = fake.0.lock().unwrap();
        assert_eq!(calls.len(), 1);
        assert_eq!(
            Path::new(&calls[0].0).canonicalize().unwrap(),
            project.join(".claude/worktrees/task-1").canonicalize().unwrap(),
            "a scheduled node works in a worktree of its own, never the main checkout"
        );
        let args = &calls[0].1;
        assert!(
            args.windows(2)
                .any(|pair| pair == ["--name", "scheduled/schedule-run-1"])
        );
        assert!(
            !args.iter().any(|arg| arg == "--session-id"),
            "claude --bg ignores it"
        );
        assert!(
            args.windows(2)
                .any(|pair| pair == ["--disallowedTools", "Bash"])
        );
        assert!(
            args.windows(2)
                .any(|pair| pair == ["--settings", settings.to_str().unwrap()])
        );
        let written: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&settings).unwrap()).unwrap();
        assert_eq!(written["env"]["ALONDRA_DOMAIN_PROTOCOL"], "2");
        let prompt = args.last().unwrap();
        assert!(
            prompt.starts_with("Review project\n")
                && prompt.contains("Choices for this work")
                && !prompt.contains("request-1")
                && prompt.contains("Alondra task task-1")
                && !prompt.contains("data_epoch=")
                && !prompt.contains("revision="),
            "{prompt}"
        );
        assert!(!prompt.contains("auto-merge"), "{prompt}");
        drop(calls);
        std::fs::remove_dir_all(root).unwrap();
    }

    struct Fixture {
        root: crate::test_support::Directory,
        home: PathBuf,
        domain: DomainRuntime,
    }
    impl Fixture {
        fn new() -> Self {
            let root = crate::test_support::Directory::new();
            let home = root.join("home");
            let projects = root.join("projects");
            std::fs::create_dir_all(&home).unwrap();
            std::fs::create_dir(&projects).unwrap();
            std::fs::write(
                options::path_in(&home),
                format!("projects_root = {:?}\n", projects.to_str().unwrap()),
            )
            .unwrap();
            let domain = DomainRuntime::open(&home).unwrap();
            execute_system(
                &domain,
                Command::CreateSchedule {
                    schedule: Schedule {
                        id: "schedule-1".into(),
                        title: "Review".into(),
                        text: "Review project".into(),
                        project: None,
                        options: BTreeMap::new(),
                        disallowed_tools: vec!["Bash".into()],
                        frequency: ScheduleFrequency::Daily,
                        hour: 9,
                        minute: 0,
                        weekday: None,
                        interval: None,
                        start_at: None,
                        enabled: false,
                        next_run: None,
                        revision: 1,
                        deleted_at: None,
                    },
                },
            )
            .unwrap();
            Self { root, home, domain }
        }
        fn claim(&self, id: &str) {
            execute_system(
                &self.domain,
                Command::ClaimScheduleRun {
                    schedule_id: "schedule-1".into(),
                    run_id: id.into(),
                    origin: ScheduleRunOrigin::Manual,
                    now: alondra_core::now_millis(),
                    next_after_now: None,
                    expected_schedule_revision: None,
                },
            )
            .unwrap();
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            std::fs::remove_dir_all(&self.root).unwrap();
        }
    }

    #[tokio::test]
    async fn schedule_create_update_delete_and_claim_wake_the_runner() {
        let fixture = Fixture::new();
        let mut changes = fixture.domain.subscribe_schedules();
        let schedule = Schedule {
            id: "schedule-2".into(),
            title: "Another review".into(),
            text: "Review".into(),
            project: None,
            options: BTreeMap::new(),
            disallowed_tools: vec![],
            frequency: ScheduleFrequency::Daily,
            hour: 9,
            minute: 0,
            weekday: None,
            interval: None,
            start_at: None,
            enabled: false,
            next_run: None,
            revision: 1,
            deleted_at: None,
        };
        execute_system(
            &fixture.domain,
            Command::CreateSchedule {
                schedule: schedule.clone(),
            },
        )
        .unwrap();
        changes.changed().await.unwrap();
        assert_eq!(
            *changes.borrow_and_update(),
            fixture.domain.state().unwrap().revision
        );

        let mut updated = schedule;
        updated.title = "Updated review".into();
        updated.revision = 2;
        execute_system(
            &fixture.domain,
            Command::UpdateSchedule { schedule: updated },
        )
        .unwrap();
        changes.changed().await.unwrap();
        assert_eq!(
            *changes.borrow_and_update(),
            fixture.domain.state().unwrap().revision
        );

        execute_system(
            &fixture.domain,
            Command::DeleteSchedule {
                schedule_id: "schedule-2".into(),
                deleted_at: alondra_core::now_millis(),
                expected_schedule_revision: Some(2),
            },
        )
        .unwrap();
        changes.changed().await.unwrap();
        assert_eq!(
            *changes.borrow_and_update(),
            fixture.domain.state().unwrap().revision
        );

        fixture.claim("run-wakes-scheduler");
        changes.changed().await.unwrap();
        assert_eq!(
            *changes.borrow_and_update(),
            fixture.domain.state().unwrap().revision
        );
    }

    struct CountingLauncher {
        calls: Mutex<usize>,
        fail: bool,
    }
    impl crate::orchestrate::Launcher for CountingLauncher {
        async fn run(
            &self,
            _: &str,
            args: &[String],
        ) -> std::result::Result<String, crate::orchestrate::Refusal> {
            *self.calls.lock().unwrap() += 1;
            assert!(args.iter().any(|arg| arg == "--settings"));
            tokio::task::yield_now().await;
            if self.fail {
                Err((StatusCode::BAD_GATEWAY, "synthetic Claude failure".into()))
            } else {
                Ok("backgrounded · ab1234 · scheduled/run".into())
            }
        }
        async fn stop(&self, _: &str) {}
        fn plugin(&self) -> Option<PathBuf> {
            Some("/synthetic/plugin".into())
        }
    }

    fn settings_files(home: &Path) -> Vec<serde_json::Value> {
        std::fs::read_dir(home.join("runtime/sessions"))
            .map(|entries| {
                entries
                    .map(|entry| {
                        serde_json::from_slice(&std::fs::read(entry.unwrap().path()).unwrap())
                            .unwrap()
                    })
                    .filter(|settings: &serde_json::Value| settings["env"].as_object().is_some_and(|env| !env.is_empty()))
                    .collect()
            })
            .unwrap_or_default()
    }

    #[tokio::test]
    async fn competing_runners_claim_one_external_dispatch() {
        let fixture = Fixture::new();
        fixture.claim("manual-1");
        let launcher = CountingLauncher {
            calls: Mutex::new(0),
            fail: false,
        };
        fixture.domain.live.mods.loaded_for(SESSION, Some("manual-1"));
        let (left, right) = tokio::join!(
            run_cycle(&fixture.domain, &fixture.home, false, &launcher),
            run_cycle(&fixture.domain, &fixture.home, false, &launcher),
        );
        left.unwrap();
        right.unwrap();
        assert_eq!(*launcher.calls.lock().unwrap(), 1);
        let state = fixture.domain.state().unwrap();
        let run = &state.schedule.runs[&ScheduleRunId::from("manual-1")];
        assert_eq!(run.state, ScheduleRunState::Sent);
        assert_eq!(
            run.launch.as_ref().unwrap().state,
            ScheduleLaunchState::Handed
        );
        assert_eq!(state.tasks.len(), 1);
        assert_eq!(state.requests.len(), 1);
        let session = SESSION.to_owned();
        let task = state.tasks.values().next().unwrap();
        assert_eq!(
            task.agent_id.as_ref().map(|id| id.0.as_str()),
            Some(session.as_str())
        );
        assert_eq!(
            run.launch
                .as_ref()
                .unwrap()
                .agent_id
                .as_ref()
                .map(|id| id.0.as_str()),
            Some(session.as_str())
        );
        assert_eq!(
            state.agents[&AgentId(session.clone())].name,
            run.launch.as_ref().unwrap().intent.name
        );
        let registry =
            std::fs::read_to_string(fixture.home.join("domain-capabilities.json")).unwrap();
        assert!(
            registry.contains(&session),
            "the session Claude listed holds a durable bearer"
        );
        let settings = settings_files(&fixture.home);
        assert_eq!(settings.len(), 1);
        assert_eq!(settings[0]["env"]["ALONDRA_DOMAIN_AGENT"], SESSION);
        assert_eq!(
            settings[0]["env"]["ALONDRA_DOMAIN_CAPABILITY"]
                .as_str()
                .unwrap()
                .len(),
            64
        );
        run_cycle(&fixture.domain, &fixture.home, false, &launcher)
            .await
            .unwrap();
        assert_eq!(*launcher.calls.lock().unwrap(), 1);
    }

    #[tokio::test]
    async fn pending_resumes_but_dispatching_becomes_uncertain_after_restart() {
        let fixture = Fixture::new();
        fixture.claim("pending");
        finish_claim(&fixture.domain, &fixture.home, &"pending".into()).unwrap();
        let reopened = DomainRuntime::open(&fixture.home).unwrap();
        let launcher = CountingLauncher {
            calls: Mutex::new(0),
            fail: false,
        };
        reopened.live.mods.loaded_for(SESSION, Some("pending"));
        run_cycle(&reopened, &fixture.home, false, &launcher)
            .await
            .unwrap();
        assert_eq!(*launcher.calls.lock().unwrap(), 1);
        assert_eq!(reopened.state().unwrap().schedule.runs[&ScheduleRunId::from("pending")].state, ScheduleRunState::Sent);

        fixture.domain.reload();
        fixture.claim("uncertain");
        finish_claim(&fixture.domain, &fixture.home, &"uncertain".into()).unwrap();
        execute_system(
            &fixture.domain,
            Command::ClaimScheduleLaunch {
                run_id: "uncertain".into(),
                now: alondra_core::now_millis(),
            },
        )
        .unwrap();
        let restarted = DomainRuntime::open(&fixture.home).unwrap();
        let run = &restarted.state().unwrap().schedule.runs[&ScheduleRunId::from("uncertain")];
        assert_eq!(run.state, ScheduleRunState::Sent);
        assert_eq!(
            run.launch.as_ref().unwrap().state,
            ScheduleLaunchState::Uncertain
        );
        crate::domain_sessions::prepare(&fixture.home, "uncertain").unwrap();
        restarted.live.mods.loaded_for(FOUND_SESSION, Some("uncertain"));
        run_cycle(&restarted, &fixture.home, false, &launcher)
            .await
            .unwrap();
        assert_eq!(*launcher.calls.lock().unwrap(), 1, "recovery never launches Claude again");
        let state = restarted.state().unwrap();
        let run = &state.schedule.runs[&ScheduleRunId::from("uncertain")];
        let launch = run.launch.as_ref().unwrap();
        assert_eq!(launch.state, ScheduleLaunchState::Handed);
        assert_eq!(launch.session_id.as_deref(), Some(FOUND_SESSION));
        let task = &state.tasks[run.task_id.as_ref().unwrap()];
        assert_eq!(task.agent_id.as_ref().map(|id| id.0.as_str()), Some(FOUND_SESSION));
        let bound = std::fs::read(fixture.home.join("runtime/sessions/uncertain.json")).unwrap();
        let bound: serde_json::Value = serde_json::from_slice(&bound).unwrap();
        assert_eq!(bound["env"]["ALONDRA_DOMAIN_AGENT"], FOUND_SESSION);
        assert_eq!(bound["env"]["ALONDRA_DOMAIN_CAPABILITY"].as_str().unwrap().len(), 64);
    }

    #[tokio::test]
    async fn an_uncertain_launch_whose_task_was_closed_fails_quietly_and_never_stops_the_runner() {
        let fixture = Fixture::new();
        fixture.claim("closed");
        finish_claim(&fixture.domain, &fixture.home, &"closed".into()).unwrap();
        execute_system(
            &fixture.domain,
            Command::ClaimScheduleLaunch { run_id: "closed".into(), now: alondra_core::now_millis() },
        )
        .unwrap();
        let task = fixture.domain.state().unwrap().schedule.runs[&ScheduleRunId::from("closed")].task_id.clone().unwrap();
        execute_system(&fixture.domain, Command::CancelTask { task_id: task }).unwrap();
        let restarted = DomainRuntime::open(&fixture.home).unwrap();
        let launcher = WithoutMods(Mutex::new(vec![]));
        run_cycle(&restarted, &fixture.home, false, &launcher).await.unwrap();

        let state = restarted.state().unwrap();
        assert_eq!(state.schedule.runs[&ScheduleRunId::from("closed")].state, ScheduleRunState::Failed);
        assert!(!state.interactions.contains_key(&InteractionId::from("problem-launch-closed")), "no Problem on a closed Task");
    }

    #[tokio::test]
    async fn a_found_session_whose_task_was_closed_is_not_bound_and_the_run_stops_retrying() {
        let fixture = Fixture::new();
        fixture.claim("closed-found");
        finish_claim(&fixture.domain, &fixture.home, &"closed-found".into()).unwrap();
        execute_system(
            &fixture.domain,
            Command::ClaimScheduleLaunch { run_id: "closed-found".into(), now: alondra_core::now_millis() },
        )
        .unwrap();
        let task = fixture.domain.state().unwrap().schedule.runs[&ScheduleRunId::from("closed-found")].task_id.clone().unwrap();
        execute_system(&fixture.domain, Command::CancelTask { task_id: task }).unwrap();
        let restarted = DomainRuntime::open(&fixture.home).unwrap();
        restarted.live.mods.loaded_for(FOUND_SESSION, Some("closed-found"));
        let launcher = WithoutMods(Mutex::new(vec![]));
        run_cycle(&restarted, &fixture.home, false, &launcher).await.unwrap();

        let state = restarted.state().unwrap();
        let run = &state.schedule.runs[&ScheduleRunId::from("closed-found")];
        assert_eq!(run.state, ScheduleRunState::Failed);
        assert_eq!(run.error.as_deref(), Some("its Task was closed"));
        assert!(!state.agents.contains_key(&AgentId(FOUND_SESSION.into())), "a closed Task's session is never bound");
        assert!(state.tasks[run.task_id.as_ref().unwrap()].agent_id.is_none());
        assert!(!state.interactions.contains_key(&InteractionId::from("problem-launch-closed-found")), "no card on a closed Task");
        let revision = state.revision;
        run_cycle(&restarted, &fixture.home, false, &launcher).await.unwrap();
        assert_eq!(restarted.state().unwrap().revision, revision, "the run is not retried every cycle");
    }

    #[tokio::test]
    async fn an_uncertain_launch_that_never_reports_is_stopped_failed_and_retried_once_on_request() {
        let fixture = Fixture::new();
        fixture.claim("lost");
        finish_claim(&fixture.domain, &fixture.home, &"lost".into()).unwrap();
        execute_system(
            &fixture.domain,
            Command::ClaimScheduleLaunch { run_id: "lost".into(), now: alondra_core::now_millis() },
        )
        .unwrap();
        crate::domain_sessions::prepare(&fixture.home, "lost").unwrap();
        let restarted = DomainRuntime::open(&fixture.home).unwrap();
        let launcher = WithoutMods(Mutex::new(vec![]));
        run_cycle(&restarted, &fixture.home, false, &launcher).await.unwrap();

        let state = restarted.state().unwrap();
        let run = &state.schedule.runs[&ScheduleRunId::from("lost")];
        assert_eq!(run.state, ScheduleRunState::Failed);
        assert_eq!(run.launch.as_ref().unwrap().state, ScheduleLaunchState::Failed);
        assert!(run.error.as_deref().unwrap().contains("did not start properly"));
        assert_eq!(launcher.0.lock().unwrap().len(), 1, "the unseen session is stopped");
        assert!(settings_files(&fixture.home).is_empty(), "nothing stays bound to Alondra");
        assert!(
            fixture.home.join("runtime/sessions/lost.json").exists(),
            "Claude Code resumes the session with the same --settings path, so the file stays"
        );
        let problem = &state.interactions[&InteractionId::from("problem-launch-lost")];
        let InteractionPayload::Question { options, .. } = &problem.payload else { panic!("a Problem asks") };
        assert_eq!(
            options.iter().map(|option| option.id.as_str()).collect::<Vec<_>>(),
            ["retry", "seen"]
        );

        let answer = |option: &str, id: &str| {
            let state = restarted.state().unwrap();
            restarted
                .execute(
                    &Actor::User { user_id: "u".into() },
                    &CommandEnvelope {
                        protocol_version: PROTOCOL_VERSION,
                        data_epoch: state.data_epoch.clone(),
                        command_id: id.to_owned().into(),
                        expected_revision: state.revision,
                        command: Command::AnswerQuestion {
                            attachments: vec![],
                            interaction_id: "problem-launch-lost".into(),
                            option_id: option.into(),
                            inputs: BTreeMap::new(),
                        },
                    },
                    None,
                    || Ok(()),
                )
                .unwrap();
        };
        answer("retry", "answer-retry");
        let relaunch = CountingLauncher { calls: Mutex::new(0), fail: false };
        restarted.live.mods.loaded_for(SESSION, Some("retry-lost"));
        run_cycle(&restarted, &fixture.home, false, &relaunch).await.unwrap();
        run_cycle(&restarted, &fixture.home, false, &relaunch).await.unwrap();
        assert_eq!(*relaunch.calls.lock().unwrap(), 1, "the retry launches the schedule once");
        let state = restarted.state().unwrap();
        let retried = &state.schedule.runs[&ScheduleRunId::from("retry-lost")];
        assert_eq!(retried.launch.as_ref().unwrap().state, ScheduleLaunchState::Handed);
        let original = state.schedule.runs[&ScheduleRunId::from("lost")].task_id.as_ref().unwrap();
        assert_eq!(
            state.tasks[original].state,
            alondra_core::domain::TaskState::Cancelled,
            "the failed run's never-started Task does not wait forever"
        );
        assert_ne!(retried.task_id.as_ref(), Some(original));
        assert_ne!(
            state.tasks[retried.task_id.as_ref().unwrap()].state,
            alondra_core::domain::TaskState::Cancelled
        );
    }

    #[tokio::test]
    async fn a_repository_claude_does_not_trust_is_never_launched_and_the_orchestrator_hears_why() {
        let fixture = Fixture::new();
        let config = fixture.root.join(".claude.json");
        std::fs::write(&config, r#"{"projects":{}}"#).unwrap();
        crate::claude_trust::use_config_for_test(&config);
        fixture.claim("untrusted");
        let launcher = CountingLauncher {
            calls: Mutex::new(0),
            fail: false,
        };
        run_cycle(&fixture.domain, &fixture.home, false, &launcher)
            .await
            .unwrap();
        assert_eq!(*launcher.calls.lock().unwrap(), 0, "Claude is never asked to start there");
        let state = fixture.domain.state().unwrap();
        let run = &state.schedule.runs[&ScheduleRunId::from("untrusted")];
        assert_eq!(run.state, ScheduleRunState::Failed);
        assert!(run.error.as_deref().unwrap().starts_with("Claude Code does not trust"));
        let told = fixture.domain.store.lock().unwrap().pending_messages().unwrap();
        assert!(
            told.iter().any(|message| message.recipient_kind == "orchestrator"
                && message.message.contains("Claude Code does not trust")),
            "the orchestrator hears the scheduled Task needs the user's trust: {told:?}"
        );
    }

    #[tokio::test]
    async fn returned_failure_commits_run_launch_and_marker_together() {
        let fixture = Fixture::new();
        fixture.claim("failed");
        let launcher = CountingLauncher {
            calls: Mutex::new(0),
            fail: true,
        };
        run_cycle(&fixture.domain, &fixture.home, false, &launcher)
            .await
            .unwrap();
        assert_eq!(*launcher.calls.lock().unwrap(), 1);
        let state = fixture.domain.state().unwrap();
        let run = &state.schedule.runs[&ScheduleRunId::from("failed")];
        assert_eq!(run.state, ScheduleRunState::Failed);
        assert_eq!(
            run.launch.as_ref().unwrap().state,
            ScheduleLaunchState::Failed
        );
        let request_id = run.request_id.as_ref().unwrap();
        assert!(
            fixture
                .domain
                .was_schedule_launch_failed(request_id)
                .unwrap()
        );
        assert!(
            state.tasks[run.task_id.as_ref().unwrap()]
                .agent_id
                .is_none(),
            "a failed launch assigns nothing"
        );
        assert!(
            state.agents.is_empty(),
            "no Agent before Claude reports a session"
        );
        assert!(
            settings_files(&fixture.home).is_empty(),
            "a failed launch leaves no settings file"
        );
        let task_id = run.task_id.as_ref().unwrap();
        let told = fixture.domain.store.lock().unwrap().pending_messages().unwrap();
        assert!(
            told.iter().any(|message| message.recipient_kind == "orchestrator"
                && message.task_id.as_deref() == Some(task_id.0.as_str())
                && message.message.contains("could not start: synthetic Claude failure")),
            "the orchestrator hears why the scheduled Task did not start: {told:?}"
        );
    }

    struct WithoutMods(Mutex<Vec<String>>);
    impl crate::orchestrate::Launcher for WithoutMods {
        async fn run(
            &self,
            _: &str,
            _: &[String],
        ) -> std::result::Result<String, crate::orchestrate::Refusal> {
            Ok("backgrounded · ab1234 · scheduled/run".into())
        }
        async fn stop(&self, short: &str) {
            self.0.lock().unwrap().push(short.into());
        }
        fn mods_deadline(&self) -> std::time::Duration {
            std::time::Duration::from_millis(20)
        }
        fn plugin(&self) -> Option<PathBuf> {
            Some("/synthetic/plugin".into())
        }
    }

    #[tokio::test]
    async fn a_session_whose_mod_never_loads_is_stopped_and_fails_the_run_without_an_agent() {
        let fixture = Fixture::new();
        fixture.claim("untrusted");
        let launcher = WithoutMods(Mutex::new(vec![]));
        run_cycle(&fixture.domain, &fixture.home, false, &launcher)
            .await
            .unwrap();
        let state = fixture.domain.state().unwrap();
        let run = &state.schedule.runs[&ScheduleRunId::from("untrusted")];
        assert_eq!(run.state, ScheduleRunState::Failed);
        assert!(run.error.as_deref().is_some_and(|error| error.starts_with("The session's mod did not report within")), "{:?}", run.error);
        assert_eq!(*launcher.0.lock().unwrap(), ["ab1234"]);
        assert!(state.agents.is_empty());
        assert!(settings_files(&fixture.home).is_empty());
    }

    #[tokio::test]
    async fn failed_marker_write_rolls_back_run_and_launch_result_without_retrying_claude() {
        let fixture = Fixture::new();
        fixture.claim("rollback");
        let conn = rusqlite::Connection::open(fixture.home.join("domain.db")).unwrap();
        conn.execute_batch(
            "CREATE TRIGGER reject_failed_marker BEFORE INSERT ON schedule_failed_launches
            BEGIN SELECT RAISE(ABORT,'synthetic marker failure'); END;",
        )
        .unwrap();
        let launcher = CountingLauncher {
            calls: Mutex::new(0),
            fail: true,
        };
        assert!(
            run_cycle(&fixture.domain, &fixture.home, false, &launcher)
                .await
                .is_err()
        );
        assert_eq!(*launcher.calls.lock().unwrap(), 1);
        let state = fixture.domain.state().unwrap();
        let run = &state.schedule.runs[&ScheduleRunId::from("rollback")];
        assert_eq!(run.state, ScheduleRunState::Sent);
        assert_eq!(
            run.launch.as_ref().unwrap().state,
            ScheduleLaunchState::Dispatching
        );
        assert!(
            !fixture
                .domain
                .was_schedule_launch_failed(run.request_id.as_ref().unwrap())
                .unwrap()
        );
        drop(conn);
        let restarted = DomainRuntime::open(&fixture.home).unwrap();
        assert_eq!(
            restarted.state().unwrap().schedule.runs[&ScheduleRunId::from("rollback")]
                .launch
                .as_ref()
                .unwrap()
                .state,
            ScheduleLaunchState::Uncertain
        );
        rusqlite::Connection::open(fixture.home.join("domain.db"))
            .unwrap()
            .execute_batch("DROP TRIGGER reject_failed_marker;")
            .unwrap();
        let recovery = WithoutMods(Mutex::new(vec![]));
        run_cycle(&restarted, &fixture.home, false, &recovery)
            .await
            .unwrap();
        assert_eq!(*launcher.calls.lock().unwrap(), 1, "recovery never launches Claude again");
        assert_eq!(
            restarted.state().unwrap().schedule.runs[&ScheduleRunId::from("rollback")].state,
            ScheduleRunState::Failed
        );
    }

    struct LoadingLauncher<'a>(&'a DomainRuntime, Mutex<usize>);
    impl crate::orchestrate::Launcher for LoadingLauncher<'_> {
        async fn run(
            &self,
            _: &str,
            args: &[String],
        ) -> std::result::Result<String, crate::orchestrate::Refusal> {
            *self.1.lock().unwrap() += 1;
            self.0.live.mods.loaded_by(SESSION, args);
            Ok("backgrounded · ab1234 · scheduled/run".into())
        }
        async fn stop(&self, _: &str) {}
        fn plugin(&self) -> Option<PathBuf> {
            Some("/synthetic/plugin".into())
        }
    }

    #[tokio::test]
    async fn a_due_schedule_and_a_pending_launch_wait_out_the_usage_limit_and_each_launch_once_after_it() {
        let fixture = Fixture::new();
        fixture.claim("manual-1");
        let now = alondra_core::now_millis();
        fixture.domain.live.usage_pause.exhaust(Some(now + 3_600_000), now);
        let launcher = LoadingLauncher(&fixture.domain, Mutex::new(0));
        let launches = || *launcher.1.lock().unwrap();
        let runs = || fixture.domain.state().unwrap().schedule.runs.len();
        run_cycle(&fixture.domain, &fixture.home, false, &launcher).await.unwrap();
        let mut due = fixture.domain.state().unwrap().schedule.schedules[&ScheduleId::from("schedule-1")].clone();
        due.enabled = true;
        due.next_run = Some(now + 200);
        due.revision += 1;
        execute_system(&fixture.domain, Command::UpdateSchedule { schedule: due }).unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(300)).await;

        run_cycle(&fixture.domain, &fixture.home, true, &launcher).await.unwrap();
        run_cycle(&fixture.domain, &fixture.home, true, &launcher).await.unwrap();
        assert_eq!(launches(), 0, "paused: Claude is not called");
        assert_eq!(runs(), 1, "the due schedule starts no run: it is deferred, not failed");
        let state = fixture.domain.state().unwrap();
        let manual = &state.schedule.runs[&ScheduleRunId::from("manual-1")];
        assert_eq!(manual.launch.as_ref().unwrap().state, ScheduleLaunchState::Pending, "the claimed launch waits too");
        assert!(state.schedule.runs.values().all(|run| run.state != ScheduleRunState::Failed), "nothing is recorded as failed");
        assert_eq!(state.schedule.schedules[&ScheduleId::from("schedule-1")].next_run, Some(now + 200), "still due");

        fixture.domain.live.usage_pause.resume(alondra_core::now_millis());
        run_cycle(&fixture.domain, &fixture.home, true, &launcher).await.unwrap();
        assert_eq!(launches(), 2, "back: the deferred launch and the due schedule go, one each");
        assert_eq!(runs(), 2);
        run_cycle(&fixture.domain, &fixture.home, true, &launcher).await.unwrap();
        assert_eq!(launches(), 2, "and never again");
        assert_eq!(runs(), 2);
        let state = fixture.domain.state().unwrap();
        assert!(state.schedule.runs.values().all(|run| run.launch.as_ref().is_some_and(|launch| launch.state == ScheduleLaunchState::Handed)));
    }

    #[tokio::test]
    async fn a_due_schedule_and_a_pending_launch_wait_while_the_disk_is_nearly_full() {
        let fixture = Fixture::new();
        fixture.claim("manual-1");
        let now = alondra_core::now_millis();
        fixture.domain.live.machine_watch.set_disk_low(true);
        let launcher = LoadingLauncher(&fixture.domain, Mutex::new(0));
        let launches = || *launcher.1.lock().unwrap();
        run_cycle(&fixture.domain, &fixture.home, false, &launcher).await.unwrap();
        let mut due = fixture.domain.state().unwrap().schedule.schedules[&ScheduleId::from("schedule-1")].clone();
        due.enabled = true;
        due.next_run = Some(now);
        due.revision += 1;
        execute_system(&fixture.domain, Command::UpdateSchedule { schedule: due }).unwrap();

        run_cycle(&fixture.domain, &fixture.home, true, &launcher).await.unwrap();
        run_cycle(&fixture.domain, &fixture.home, true, &launcher).await.unwrap();
        assert_eq!(launches(), 0, "no node starts on a nearly full disk");
        let state = fixture.domain.state().unwrap();
        assert_eq!(state.schedule.runs[&ScheduleRunId::from("manual-1")].launch.as_ref().unwrap().state, ScheduleLaunchState::Pending);
        assert!(state.schedule.runs.values().all(|run| run.state != ScheduleRunState::Failed), "nothing is recorded as failed");

        fixture.domain.live.machine_watch.set_disk_low(false);
        run_cycle(&fixture.domain, &fixture.home, true, &launcher).await.unwrap();
        assert_eq!(launches(), 2, "space again: the deferred launch and the due schedule go");
    }
}
