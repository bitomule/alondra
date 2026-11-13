//! Schedule v2's local-calendar and launch preparation adapter. The command
//! carrying this intent must be committed with its Run, Request, and Task before
//! a technical launch claim can call Claude.
use crate::locks::Poisonable as _;
use crate::domain_runtime::DomainRuntime;
use crate::domain_store::StoreError;
use crate::launch_worktree::Workplace;
use alondra_core::domain::{
    Actor, Command, CommandEnvelope, CommandOutcome, DomainState, PROTOCOL_VERSION,
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
    let mut resolved = file.resolve(definition.project.as_deref(), &definition.options)?;
    let pinned = resolved.account_key();
    if resolved.selection.is_none() && pinned.is_some() {
        let mut intent = alondra_core::model_selection::TaskSelection::automatic(alondra_core::model_selection::ModelProfile::Implementation);
        intent.account = pinned;
        if let Some(model) = &resolved.flags.model {
            intent.request_model(Some(alondra_core::model_selection::ModelKey { provider: alondra_core::accounts::Provider::Claude, model: model.clone() }));
            intent.requested_reasoning_effort = resolved.flags.effort.clone();
        }
        resolved.selection = Some(intent);
    }
    if let Some(intent) = &mut resolved.selection {
        intent.paid_account_grant = definition.paid_account_grant.clone().filter(|grant| Some(grant) == intent.account.as_ref());
    }
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

/// Who wrote the text a node starts with: a decision the orchestrator attributes to the user in a
/// mission is its account of it, while a schedule's text is the user's own.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum MissionAuthor {
    Orchestrator,
    Schedule,
}

/// The mission a node starts with, followed by the choices made for it.
pub(crate) fn producer_prompt(
    text: &str,
    author: MissionAuthor,
    resolved: &Resolved,
    task: &TaskId,
    approach: alondra_core::options::Approach,
) -> String {
    let user = crate::domain_messages::user_name();
    let planning = if resolved.selected_provider() == Some(alondra_core::accounts::Provider::Claude) {
        format!("a plan goes through plan mode (`EnterPlanMode`, then `ExitPlanMode`, which {user} approves or sends back in Alondra), never a document or an `ask`")
    } else {
        "submit plans through the Alondra planning tools advertised for this execution and wait for the user approval; use only native planning tools available in this session".to_owned()
    };
    let mut lines = vec![
        text.trim_end().to_owned(),
        String::new(),
        "## Your Alondra task".into(),
        String::new(),
        format!("This work is Alondra task {task}; pass it as task_id to the Alondra tools. Your terminal does not reach {user}: only the Alondra tools do, and {user}'s answers are typed into this session. Use `ask` for a decision and end your turn after it; {planning}. Use `request_action` for something {user} does outside Alondra, and `publish_information` for a finding {user} should read now. When you are done, deliver it with `present_result` and is_final true (the answer in body, files in attachments, a PR you registered or a document revision in targets) and end your turn: {user} accepts it in Alondra. A file or a final message left in this session is not a delivery."),
        String::new(),
    ];
    if author == MissionAuthor::Orchestrator {
        lines.push(format!("The orchestrator wrote this mission: a decision it attributes to {user} is its account of it, not {user}'s answer. Present a decision as {user}'s only from {user}'s own answer or message; ask {user} for one you do not have."));
        lines.push(String::new());
    }
    lines.extend([
        "## Choices for this work".into(),
        String::new(),
        "The user chose these options for this work:".into(),
    ]);
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
    lines.extend(approach.how_to_work_for(if resolved.selection.is_some() { None } else { resolved.selected_provider() }));
    lines.join("\n")
}

/// Invoke only after the technical intent has been claimed durably: the scheduled Request's task
/// starts through the daemon's Agent SDK host, with the Request's denied tools. The node the host
/// runs is the task's own from its admission.
async fn launch_through_sdk(
    domain: &DomainRuntime,
    launch: &ScheduleLaunchIntent,
    task: &TaskId,
    disallowed_tools: &[String],
    home: &Path,
) -> Result<String, String> {
    let mut resolved: Resolved = serde_json::from_str(&launch.options_json).map_err(|why| why.to_string())?;
    let account = if let Some(intent) = resolved.selection.clone() {
        let decision = crate::accounts::select_for_launch_with_tools(domain, home, &intent, disallowed_tools, alondra_core::now_millis()).map_err(|why| why.message)?;
        let mut intent = intent;
        intent.effective = Some(decision.selection.clone());
        resolved.flags.model = (decision.account.provider == alondra_core::accounts::Provider::Claude).then(|| decision.selection.model.model.clone());
        resolved.flags.effort = decision.selection.reasoning_effort;
        resolved.selection = Some(intent);
        decision.account
    } else {
        let id = crate::accounts::choose_for_launch(domain, home, alondra_core::now_millis())
            .ok_or_else(|| if domain.live.usage_pause.waits_for_session() { "no Claude account on this Mac has a session" } else { "no Claude account on this Mac has usage left" }.to_owned())?;
        alondra_core::accounts::AccountKey::new(alondra_core::accounts::Provider::Claude, id)
    };
    crate::accounts::log_choice(domain, "scheduled run", &account, alondra_core::now_millis());
    let profile = alondra_core::accounts::provider_folder(home, &account);
    let mut selected_launch = launch.clone();
    selected_launch.options_json = serde_json::to_string(&resolved).map_err(|why| why.to_string())?;
    let (workplace, plugin, resolved) = prepare_claimed(&selected_launch, task, home, profile.as_deref()).await.map_err(|(_, reason)| reason)?;
    crate::domain_orchestrator_tools::own_worktree(domain, task, &workplace).await.map_err(|error| error.message)?;
    let selected = resolved.selection.as_ref().and_then(|selection| selection.effective.as_ref());
    let options = alondra_daemon_engine_model::LaunchOptions {
        provider: account.provider,
        selection: resolved.selection.clone(),
        model: selected.map(|selection| selection.model.model.clone()).or_else(|| resolved.flags.model.clone()),
        effort: selected.and_then(|selection| selection.reasoning_effort.clone()).or_else(|| resolved.flags.effort.clone()),
        disallowed_tools: disallowed_tools.to_vec(),
    };
    let launched = crate::sdk_owner::launch(domain, crate::sdk_owner::Launch {
        task: task.clone(),
        name: launch.name.clone(),
        directory: workplace.dir().to_string_lossy().into_owned(),
        prompt: producer_prompt(&launch.prompt, MissionAuthor::Schedule, &resolved, task, alondra_core::options::Approach::Solo),
        plugin,
        options,
        model: resolved.launch_model(),
        account: account.account,
    })
    .await
    .map_err(|error| error.message)?;
    let agent = launched["agent_id"].as_str().map(str::to_owned).ok_or_else(|| "the SDK launch named no node".to_owned())?;
    if let Some(dir) = workplace.chosen().map(|dir| dir.to_string_lossy().into_owned()) {
        if let Err(error) = domain.set_inherited_worktree(&agent, &dir, "alondra") {
            log!("could not record that {agent} works in {dir}: {error:#}");
        }
    }
    Ok(agent)
}

async fn prepare_claimed(
    launch: &ScheduleLaunchIntent,
    task: &TaskId,
    home: &Path,
    profile: Option<&Path>,
) -> Result<(Workplace, PathBuf, Resolved), (StatusCode, String)> {
    if !Path::new(&launch.cwd).is_dir() {
        return Err((
            StatusCode::BAD_REQUEST,
            "schedule project directory does not exist".into(),
        ));
    }
    let resolved: Resolved = serde_json::from_str(&launch.options_json).map_err(|error| (StatusCode::INTERNAL_SERVER_ERROR, error.to_string()))?;
    let projects_root = options_at(home).ok().map(|file| file.projects_root(&crate::options::user_home()));
    if let Some(repository) = (resolved.selected_provider() == Some(alondra_core::accounts::Provider::Claude)).then(|| crate::claude_trust::ensure_for(profile, Path::new(&launch.cwd), projects_root.as_deref())).flatten() {
        return Err((
            StatusCode::CONFLICT,
            crate::claude_trust::instructions(&repository),
        ));
    }
    let workplace = crate::launch_worktree::isolate(Path::new(&launch.cwd), &crate::launch_worktree::slug(&task.0), None, None)
        .await
        .map_err(|why| (StatusCode::CONFLICT, why))?;
    let Some(plugin) = crate::plugins::node_in(home) else {
        return Err((
            StatusCode::SERVICE_UNAVAILABLE,
            "Alondra's Claude plugin is unavailable".into(),
        ));
    };
    Ok((workplace, plugin, resolved))
}

fn execute_system(domain: &DomainRuntime, command: Command) -> Result<CommandOutcome, StoreError> {
    let state = domain.state().map_err(StoreError::Storage)?;
    domain.execute_as_system(CommandEnvelope {
        protocol_version: PROTOCOL_VERSION,
        data_epoch: state.data_epoch.clone(),
        command_id: format!("schedule-worker-{}", crate::auth::random_hex(16)).into(),
        expected_revision: 0,
        command,
    })
}

fn ordinary_conflict(error: &StoreError) -> bool {
    matches!(error, StoreError::Domain(domain) if matches!(domain.code.as_str(), "conflict" | "not_found" | "stale_revision"))
}

pub fn options_at(home: &Path) -> Result<OptionsFile> {
    match crate::domain_store::provider_settings::read_options(home) {
        Ok(Some(contents)) => OptionsFile::parse(&contents).map_err(anyhow::Error::msg),
        Ok(None) => Ok(OptionsFile::fresh()),
        Err(error) => Err(anyhow::anyhow!(error)),
    }
}

fn scheduled_task_name(title: &str, at: i64) -> Result<String> {
    let seconds = at.div_euclid(1000) as libc::time_t;
    let mut date = unsafe { std::mem::zeroed::<libc::tm>() };
    if unsafe { libc::gmtime_r(&seconds, &mut date) }.is_null() {
        anyhow::bail!("scheduled task date is outside the supported calendar");
    }
    let date = format!("{:04}-{:02}-{:02}", date.tm_year + 1900, date.tm_mon + 1, date.tm_mday);
    let suffix = crate::auth::random_hex(16);
    let title = crate::launch_worktree::slug(title);
    // Reserve the complete identity suffix before the worktree's 64-byte limit.
    let budget = 64_usize.saturating_sub(4 + date.len() + suffix.len());
    let title = title.chars().take(budget).collect::<String>();
    Ok(format!("t-{}-{date}-{suffix}", title.trim_end_matches('-')))
}

fn request_task(snapshot: &ScheduleDefinitionSnapshot, at: i64) -> Result<(Request, Task)> {
    let request_id: RequestId = format!("r-{}", crate::auth::random_hex(16)).into();
    let request = Request::new(request_id.clone(), snapshot.text.clone());
    let task = Task {
        provider: Default::default(), selection: None, pending_selection: None, move_refused: None,
        id: scheduled_task_name(&snapshot.title, at)?.into(),
        request_id,
        agent_id: None,
        title: snapshot.title.clone(),
        state: TaskState::NotStarted,
        revision: 0,
        started: false,
        review_prs: false,
        created_revision: 0, final_presentation: None,
        pr_id: None,
        later_prs: vec![],
        opened_prs: Default::default(),
        dependencies: BTreeSet::new(),
        stack: None,
        documents: BTreeMap::new(),
        archived: false,
        model: None,
        waiting_for_tasks: Default::default(), ended_task_waits: Default::default(), delivered_dependencies: Default::default(), blocks: Default::default(), verdict: None, review_rounds: 0, wrap_up: Default::default(), priority: Default::default(), ending: None, stalls: 0, stall_closing: None, open_pr: None, continues: None, worktrees: Default::default(), account: None, allowed: Default::default(), project: None,
    };
    Ok((request, task))
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
    let mut launch = match prepared {
        Ok(launch) => launch,
        Err(error) => {
            fail_run(domain, run_id, error.to_string())?;
            return Ok(());
        }
    };
    let (mut request, task) = request_task(snapshot, run.claimed_at.context("claimed Run has no timestamp")?)?;
    launch.name = format!("scheduled/{}", task.id.0);
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
    let now = alondra_core::now_millis();
    let resolved: Resolved = serde_json::from_str(&launch.intent.options_json)?;
    let ready = if let Some(intent) = &resolved.selection {
        match crate::accounts::select_for_launch_with_tools(domain, home, intent, &policy, now) {
            Ok(_) => true,
            Err(error) if error.code == "paid_authorization_required" || error.code == "unsupported_capability" => { fail_run(domain, run_id, error.message)?; return Ok(()); }
            Err(_) => false,
        }
    } else { crate::accounts::choose_for_launch(domain, home, now).is_some() };
    let paused = domain.live.usage_pause.current().is_some_and(|pause| pause.by_user || resolved.selection.is_none());
    if !ready || paused || domain.live.machine_watch.disk_low() {
        return Ok(());
    }
    let key = crate::admission::Key::Run(run_id.0.clone());
    if state.scheduled_launch_guard(run_id).is_err() {
        domain.live.gate.forget(&key);
        return Ok(());
    }
    let waiting = crate::admission::Waiting {
        key: key.clone(),
        priority: crate::admission::run_priority(&state, run),
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
        Err(error) if ordinary_conflict(&error) => {
            domain.live.gate.not_started(&key);
            return Ok(());
        }
        Err(error) => {
            domain.live.gate.not_started(&key);
            return Err(error.into());
        }
    }
    let agent = match launch_through_sdk(domain, &intent, &task, &policy, home).await {
        Ok(agent) => agent,
        Err(reason) => return fail_run(domain, run_id, reason),
    };
    let handed = execute_system(domain, Command::RecordScheduleLaunchHanded {
        run_id: run_id.clone(),
        session_id: Some(agent),
        agent_id: None,
        finished_at: alondra_core::now_millis(),
    });
    match handed {
        Ok(_) => Ok(()),
        Err(error) if ordinary_conflict(&error) => Ok(()),
        Err(error) => Err(error.into()),
    }
}

/// A launch Alondra died in the middle of, by this daemon's SDK host or an earlier release's
/// `claude --bg`: whether its node started is not known, so the run fails and says so, and its
/// task waits for the user's answer like any other failed start.
fn recover_uncertain(domain: &DomainRuntime, run_id: &ScheduleRunId) -> Result<()> {
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
    let name = launch.intent.name.as_str();
    let title = run.definition_snapshot.as_ref().map_or(name, |snapshot| snapshot.title.as_str());
    fail_run(
        domain,
        run_id,
        format!("The scheduled run \"{title}\" did not start properly: Alondra stopped while launching it. Try it again."),
    )
}

/// The user's answer to a scheduled run that did not start. "Try again" launches its schedule
/// once more (the retry's run id derives from the failed one, so answering twice launches once);
/// either way the failed run's Task never started and is cancelled, so it does not wait forever.
fn answer_failed_launches(domain: &DomainRuntime, home: &Path) -> Result<()> {
    use alondra_core::domain::{InteractionPayload, LAUNCH_RETRY_OPTION, SystemCard};
    let state = domain.state()?;
    for (id, interaction) in &state.interactions {
        let Some(SystemCard::LaunchProblem { run: failed }) = &interaction.system_card else { continue };
        let failed = failed.as_str();
        let InteractionPayload::Question { answer: Some(answer), .. } = &interaction.payload else { continue };
        if interaction.withdrawn {
            continue;
        }
        let Some(run) = state.schedule.runs.get(&ScheduleRunId::from(failed)) else { continue };
        let (retried, gone) = if answer.option_id == LAUNCH_RETRY_OPTION {
            match retry_launch(domain, home, &state, failed, run) {
                Some(retried) => (retried, false),
                None => (true, true),
            }
        } else {
            (true, false)
        };
        let Some(task) = run.task_id.as_ref().filter(|_| retried) else { continue };
        match execute_system(domain, Command::CancelTask { task_id: task.clone(), reason: None }) {
            Ok(_) if gone => {
                if let Err(error) = domain.store.lock_or_exit("the domain store").set_cancel_reason(task, "Its schedule no longer exists, so it cannot be launched again") {
                    log!("could not say why {task} was cancelled: {error:#}");
                }
            }
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

/// Whether the schedule is launching again: the retry run exists or was just claimed. `None` when
/// its schedule is gone, so nothing can launch it again.
fn retry_launch(
    domain: &DomainRuntime,
    home: &Path,
    state: &DomainState,
    failed: &str,
    run: &alondra_core::domain::ScheduleRun,
) -> Option<bool> {
    let retry: ScheduleRunId = format!("retry-{failed}").into();
    if state.schedule.runs.contains_key(&retry) {
        return Some(true);
    }
    let schedule = state
        .schedule
        .schedules
        .get(&run.schedule_id)
        .filter(|schedule| schedule.deleted_at.is_none())?;
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
            Some(true)
        }
        Err(error) => {
            log!("retry of scheduled run {failed} refused: {error}");
            Some(false)
        }
    }
}

pub(crate) async fn run_cycle(
    domain: &DomainRuntime,
    home: &Path,
    due_enabled: bool,
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
        if let Err(error) = recover_uncertain(domain, &run_id) {
            log!("could not recover scheduled run {run_id}: {error:#}");
        }
    }
    for (run_id, reason) in domain.state()?.scheduled_launches_of_ended_tasks() {
        fail_run(domain, &run_id, reason.into())?;
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
        dispatch_pending(domain, home, &run_id).await?;
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

pub async fn run(
    domain: Arc<DomainRuntime>,
    home: PathBuf,
    due_enabled: bool,
) -> Result<()> {
    let mut schedule_changes = domain.subscribe_schedules();
    let mut usage_changes = domain.live.usage_pause.subscribe();
    let mut turn = domain.live.gate.subscribe();
    let mut system_clock = crate::schedule_clock::ClockChanges::new();
    loop {
        run_cycle(&domain, &home, due_enabled).await?;
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
                run_cycle(&domain, &home, due_enabled).await?;
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
    use crate::sdk_owner::tests::SdkLaunches;
    use alondra_core::domain::{InteractionId, InteractionPayload, Schedule, ScheduleFrequency};
    use std::collections::BTreeMap;

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
        let auto = producer_prompt("Fix it", MissionAuthor::Orchestrator, &chosen("auto_merge"), &TaskId::from("t-1"), alondra_core::options::Approach::Solo);
        assert!(auto.contains("once its checks pass, Alondra merges it") && auto.contains("Do not merge it yourself"), "{auto}");
        assert!(!auto.contains("gh pr merge") && !auto.contains("authorization"), "{auto}");
        let review = producer_prompt("Fix it", MissionAuthor::Orchestrator, &chosen("review"), &TaskId::from("t-1"), alondra_core::options::Approach::Solo);
        assert!(!review.contains("auto-merge"), "{review}");
    }

    #[test]
    fn the_mission_names_the_task_and_how_to_deliver_through_alondra() {
        let resolved = OptionsFile::default().resolve(None, &BTreeMap::new()).unwrap();
        let prompt = producer_prompt("Analyse the mods", MissionAuthor::Orchestrator, &resolved, &TaskId::from("t-mods"), alondra_core::options::Approach::Solo);
        assert!(prompt.starts_with("Analyse the mods\n"), "{prompt}");
        assert!(prompt.contains("This work is Alondra task t-mods; pass it as task_id"), "{prompt}");
        assert!(prompt.contains("`present_result` and is_final true") && prompt.contains("`ask`")
            && prompt.contains("is not a delivery"), "{prompt}");
        assert!(!prompt.contains("`report`"), "the old tool does not exist for this session: {prompt}");
    }

    /// Task t-web-single-source-plan: the orchestrator's mission said «Decisión previa de David»
    /// and the node took it as David's own words.
    #[test]
    fn only_a_mission_the_orchestrator_wrote_says_that_it_did() {
        let resolved = OptionsFile::default().resolve(None, &BTreeMap::new()).unwrap();
        let prompt = |author| producer_prompt("Analyse the mods", author, &resolved, &TaskId::from("t-mods"), alondra_core::options::Approach::Solo);
        assert!(prompt(MissionAuthor::Orchestrator).contains("The orchestrator wrote this mission"));
        assert!(!prompt(MissionAuthor::Schedule).contains("The orchestrator wrote this mission"));
    }

    #[test]
    fn calendar_matches_legacy_for_manual_disabled_and_due_slots() {
        let mut schedule = Schedule {
            paid_account_grant: None,
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
            paid_account_grant: None,
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

    #[test]
    fn scheduled_names_are_readable_unique_and_persisted() {
        let fixture = Fixture::new();
        fixture.claim("readable");
        finish_claim(&fixture.domain, &fixture.home, &"readable".into()).unwrap();
        let state = fixture.domain.state().unwrap();
        let run = &state.schedule.runs[&ScheduleRunId::from("readable")];
        let task = run.task_id.as_ref().unwrap();
        assert!(task.0.starts_with("t-review-"), "{}", task.0);
        assert_eq!(run.launch.as_ref().unwrap().intent.name, format!("scheduled/{}", task.0));
        assert_eq!(crate::launch_worktree::slug(&task.0), task.0);
        finish_claim(&fixture.domain, &fixture.home, &"readable".into()).unwrap();
        assert_eq!(fixture.domain.state().unwrap().schedule.runs[&run.id], *run);
        fixture.claim("another");
        finish_claim(&fixture.domain, &fixture.home, &"another".into()).unwrap();
        assert_ne!(fixture.domain.state().unwrap().schedule.runs[&ScheduleRunId::from("another")].task_id.as_ref(), Some(task));
    }

    #[test]
    fn scheduled_names_keep_their_suffix_for_long_and_non_ascii_titles() {
        for title in ["Weekly review ".repeat(8), "Revisión semanal".into(), "🎉 !!!".into()] {
            let fixture = Fixture::new();
            let mut schedule = fixture.domain.state().unwrap().schedule.schedules[&ScheduleId::from("schedule-1")].clone();
            schedule.title = title;
            schedule.revision += 1;
            execute_system(&fixture.domain, Command::UpdateSchedule { schedule }).unwrap();
            fixture.claim("title");
            finish_claim(&fixture.domain, &fixture.home, &"title".into()).unwrap();
            let state = fixture.domain.state().unwrap();
            let task = state.schedule.runs[&ScheduleRunId::from("title")].task_id.as_ref().unwrap();
            assert!(task.0.len() <= 64);
            assert_eq!(crate::launch_worktree::slug(&task.0), task.0);
            assert!(task.0.split('-').count() >= 6, "title, UTC date and unique suffix: {}", task.0);
            assert_eq!(task.0.rsplit('-').next().unwrap().len(), 32);
        }
    }

    #[test]
    fn scheduled_name_uses_the_claims_utc_date() {
        let name = scheduled_task_name("Weekly review", 1_783_036_800_000).unwrap();
        assert!(name.starts_with("t-weekly-review-2026-07-03-"), "{name}");
        assert_eq!(scheduled_task_name("🎉", 0).unwrap().split('-').take(5).collect::<Vec<_>>(), ["t", "node", "1970", "01", "01"]);
    }

    #[tokio::test]
    async fn existing_scheduled_identity_survives_reopening_and_schedule_edits() {
        let fixture = Fixture::new();
        let project = fixture.in_project("LegacyApp");
        let old_slug = "t-a245e78abcdef";
        let workplace = crate::launch_worktree::isolate(&project, old_slug, None, None).await.unwrap();
        let old_path = project.join(".claude/worktrees").join(old_slug).canonicalize().unwrap();
        assert_eq!(workplace.dir().canonicalize().unwrap(), old_path);
        fixture.claim("legacy");
        let state = fixture.domain.state().unwrap();
        let run = &state.schedule.runs[&ScheduleRunId::from("legacy")];
        let snapshot = run.definition_snapshot.as_ref().unwrap();
        let (request, mut task) = request_task(snapshot, 0).unwrap();
        task.id = "t-a245e78abcdef".into();
        let file = options_at(&fixture.home).unwrap();
        let launch = prepare_intent(&run.schedule_id, &run.id, snapshot, &file, &fixture.root.join("projects")).unwrap();
        execute_system(&fixture.domain, Command::RecordScheduleRunSent {
            run_id: run.id.clone(), request, task, launch, finished_at: alondra_core::now_millis(),
        }).unwrap();
        let saved = fixture.domain.state().unwrap().schedule.runs[&run.id].clone();
        let mut schedule = fixture.domain.state().unwrap().schedule.schedules[&run.schedule_id].clone();
        schedule.title = "Changed title".into();
        schedule.revision += 1;
        execute_system(&fixture.domain, Command::UpdateSchedule { schedule }).unwrap();
        let reopened = DomainRuntime::open(&fixture.home).unwrap();
        finish_claim(&reopened, &fixture.home, &run.id).unwrap();
        assert_eq!(reopened.state().unwrap().schedule.runs[&run.id], saved);
        assert_eq!(old_path.canonicalize().unwrap(), old_path);
        let branch = std::process::Command::new("git").arg("-C").arg(&old_path).args(["branch", "--show-current"]).output().unwrap();
        assert!(branch.status.success());
        assert_eq!(String::from_utf8(branch.stdout).unwrap().trim(), format!("worktree-{old_slug}"));
    }

    pub struct Fixture {
        root: crate::test_support::Directory,
        home: PathBuf,
        domain: std::sync::Arc<DomainRuntime>,
    }
    impl Fixture {
        fn open(home: &Path) -> DomainRuntime {
            let domain = DomainRuntime::open(home).unwrap();
            crate::sdk_owner::tests::measure_healthy_machine(&domain);
            domain
        }

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
            let domain = std::sync::Arc::new(Self::open(&home));
            execute_system(
                &domain,
                Command::CreateSchedule {
                    schedule: Schedule {
                        paid_account_grant: None,
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

    fn git_repository(dir: &Path) {
        std::fs::create_dir_all(dir).unwrap();
        for args in [&["init", "-q", "-b", "main"][..], &["-c", "user.name=t", "-c", "user.email=t@t", "commit", "-q", "--allow-empty", "-m", "start"]] {
            assert!(std::process::Command::new("git").arg("-C").arg(dir).args(args).status().unwrap().success());
        }
    }

    impl Fixture {
        /// Points the schedule at `name`, a git repository in the projects folder.
        fn in_project(&self, name: &str) -> PathBuf {
            let project = self.root.join("projects").join(name);
            git_repository(&project);
            let mut schedule = self.domain.state().unwrap().schedule.schedules[&ScheduleId::from("schedule-1")].clone();
            schedule.project = Some(name.into());
            schedule.revision += 1;
            execute_system(&self.domain, Command::UpdateSchedule { schedule }).unwrap();
            project
        }

        /// How many nodes were started: each SDK launch registers its own.
        fn launches(&self) -> usize {
            self.domain.state().unwrap().agents.len()
        }
    }

    fn answer_problem(domain: &DomainRuntime, problem: &str, option: &str, id: &str) {
        let state = domain.state().unwrap();
        domain
            .execute(
                &Actor::User { user_id: "u".into() },
                &CommandEnvelope {
                    protocol_version: PROTOCOL_VERSION,
                    data_epoch: state.data_epoch.clone(),
                    command_id: id.to_owned().into(),
                    expected_revision: state.revision,
                    command: Command::AnswerQuestion {
                        attachments: vec![],
                        interaction_id: problem.into(),
                        option_id: option.into(),
                        inputs: BTreeMap::new(),
                    },
                },
                None,
                || Ok(()),
            )
            .unwrap();
    }

    // claude --bg got the run's name, the Request's denied tools and the mission as flags. Through
    // the SDK the host's Claude is started with them, in a worktree of the task's own.
    #[tokio::test(flavor = "multi_thread")]
    async fn captured_launch_uses_request_policy_and_explicit_new_node_command() {
        let fixture = Fixture::new();
        let sdk = SdkLaunches::enable(&fixture.domain, &fixture.home);
        let project = fixture.in_project("App");
        fixture.claim("run-1");
        run_cycle(&fixture.domain, &fixture.home, false).await.unwrap();
        let state = fixture.domain.state().unwrap();
        let run = &state.schedule.runs[&ScheduleRunId::from("run-1")];
        let task = run.task_id.clone().unwrap();
        let agent = state.tasks[&task].agent_id.clone().expect("the task is its SDK node's");
        assert_eq!(state.agents[&agent].name, format!("scheduled/{}", task.0));
        assert_eq!(sdk.options(&agent.0).disallowed_tools, ["Bash"], "the Request's policy is kept for a resumed session");
        sdk.until_answered(&task.0).await;
        let starts = sdk.starts();
        assert_eq!(starts.len(), 1, "{starts:?}");
        assert_eq!(
            PathBuf::from(starts[0]["cwd"].as_str().unwrap()),
            project.join(".claude/worktrees").join(crate::launch_worktree::slug(&task.0)).canonicalize().unwrap(),
            "a scheduled node works in a worktree of its own, never the main checkout"
        );
        let branch = std::process::Command::new("git").arg("-C").arg(starts[0]["cwd"].as_str().unwrap()).args(["branch", "--show-current"]).output().unwrap();
        assert!(branch.status.success());
        assert_eq!(String::from_utf8(branch.stdout).unwrap().trim(), format!("worktree-{}", task.0));
        assert!(sdk.flag("--disallowedTools").is_some_and(|tools| tools.contains("Bash")), "{:?}", sdk.args());
        let prompts = sdk.prompts();
        assert_eq!(prompts.len(), 1, "{prompts:?}");
        let prompt = &prompts[0];
        let request = run.request_id.as_ref().unwrap();
        assert!(
            prompt.starts_with("Review project\n")
                && prompt.contains("Choices for this work")
                && !prompt.contains(&request.0)
                && prompt.contains(&format!("Alondra task {task}"))
                && !prompt.contains("data_epoch=")
                && !prompt.contains("revision="),
            "{prompt}"
        );
        assert!(!prompt.contains("auto-merge"), "{prompt}");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_scheduled_runs_sdk_launch_records_the_worktree_alondra_made_as_its_tasks_worktree() {
        let fixture = Fixture::new();
        let _sdk = SdkLaunches::enable(&fixture.domain, &fixture.home);
        let project = fixture.in_project("App");
        fixture.claim("run-1");
        run_cycle(&fixture.domain, &fixture.home, false).await.unwrap();
        let state = fixture.domain.state().unwrap();
        let task = state.schedule.runs[&ScheduleRunId::from("run-1")].task_id.clone().unwrap();
        let agent = state.tasks[&task].agent_id.clone().expect("the task is its SDK node's");
        let made = project.join(".claude/worktrees").join(crate::launch_worktree::slug(&task.0)).canonicalize().unwrap();
        let recorded = fixture.domain.task_worktree(&task).unwrap().expect("the task's worktree is recorded");
        assert_eq!(Path::new(&recorded).canonicalize().unwrap(), made, "it is cleaned with the task");
        let owned = fixture.domain.inherited_worktree(&agent.0).unwrap().expect("its node owns it");
        assert_eq!(Path::new(&owned).canonicalize().unwrap(), made);
    }

    #[tokio::test]
    async fn schedule_create_update_delete_and_claim_wake_the_runner() {
        let fixture = Fixture::new();
        let mut changes = fixture.domain.subscribe_schedules();
        let schedule = Schedule {
            paid_account_grant: None,
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

    #[tokio::test(flavor = "multi_thread")]
    async fn competing_runners_claim_one_external_dispatch() {
        let fixture = Fixture::new();
        let sdk = SdkLaunches::enable(&fixture.domain, &fixture.home);
        fixture.claim("manual-1");
        let (left, right) = tokio::join!(
            run_cycle(&fixture.domain, &fixture.home, false),
            run_cycle(&fixture.domain, &fixture.home, false),
        );
        left.unwrap();
        right.unwrap();
        assert_eq!(fixture.launches(), 1);
        let state = fixture.domain.state().unwrap();
        let run = &state.schedule.runs[&ScheduleRunId::from("manual-1")];
        assert_eq!(run.state, ScheduleRunState::Sent);
        let launch = run.launch.as_ref().unwrap();
        assert_eq!(launch.state, ScheduleLaunchState::Handed);
        assert_eq!(state.tasks.len(), 1);
        assert_eq!(state.requests.len(), 1);
        let task = state.tasks.values().next().unwrap();
        let agent = task.agent_id.clone().expect("the task is its SDK node's");
        assert_eq!(launch.session_id.as_deref(), Some(agent.0.as_str()), "the run is handed to the node that runs it");
        assert_eq!(state.agents[&agent].name, launch.intent.name);
        let registry = std::fs::read_to_string(fixture.home.join("domain-capabilities.json")).unwrap();
        assert!(registry.contains(&agent.0), "the node holds a durable bearer");
        sdk.until_answered(&task.id.0).await;
        let capture: serde_json::Value = serde_json::from_slice(&std::fs::read(&sdk.capture).unwrap()).unwrap();
        let environment: Vec<&str> = capture["env_keys"].as_array().unwrap().iter().filter_map(serde_json::Value::as_str).collect();
        assert!(environment.contains(&"ALONDRA_DOMAIN_CAPABILITY"), "its Claude reaches Alondra with that bearer: {environment:?}");
        run_cycle(&fixture.domain, &fixture.home, false).await.unwrap();
        assert_eq!(fixture.launches(), 1);
        assert_eq!(sdk.starts().len(), 1, "one Claude is started");
    }

    // A scheduled run of a daemon with the Agent SDK host starts its task through that host, with
    // the same checks, and is handed to the node the host runs.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_scheduled_task_runs_through_the_sdk_host() {
        let fixture = Fixture::new();
        let _sdk = SdkLaunches::enable(&fixture.domain, &fixture.home);
        fixture.claim("sdk-run");
        run_cycle(&fixture.domain, &fixture.home, false).await.unwrap();
        let state = fixture.domain.state().unwrap();
        let run = &state.schedule.runs[&ScheduleRunId::from("sdk-run")];
        assert_eq!(run.launch.as_ref().unwrap().state, ScheduleLaunchState::Handed);
        let task = state.tasks.values().next().unwrap();
        let lifecycle = alondra_core::domain::replay(&task.id, &state.lifecycle_journal[&task.id]).unwrap();
        assert_eq!(lifecycle.execution().map(|stamp| stamp.host().as_str().to_owned()).as_deref(), Some("alondra-sdk"));
        assert!(task.agent_id.as_ref().is_some_and(|agent| fixture.domain.owns_sdk_host(agent)), "the host's owner runs its node");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn pending_resumes_but_dispatching_becomes_uncertain_after_restart() {
        let fixture = Fixture::new();
        fixture.claim("pending");
        finish_claim(&fixture.domain, &fixture.home, &"pending".into()).unwrap();
        {
            let reopened = Arc::new(Fixture::open(&fixture.home));
            let sdk = SdkLaunches::enable(&reopened, &fixture.home);
            run_cycle(&reopened, &fixture.home, false).await.unwrap();
            let state = reopened.state().unwrap();
            let run = &state.schedule.runs[&ScheduleRunId::from("pending")];
            assert_eq!(run.state, ScheduleRunState::Sent);
            assert_eq!(run.launch.as_ref().unwrap().state, ScheduleLaunchState::Handed);
            assert_eq!(state.agents.len(), 1, "the pending launch goes once");
            sdk.until_answered(&run.task_id.as_ref().unwrap().0).await;
        }

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
        let restarted = Fixture::open(&fixture.home);
        let run = &restarted.state().unwrap().schedule.runs[&ScheduleRunId::from("uncertain")];
        assert_eq!(run.state, ScheduleRunState::Sent);
        assert_eq!(
            run.launch.as_ref().unwrap().state,
            ScheduleLaunchState::Uncertain
        );
        run_cycle(&restarted, &fixture.home, false).await.unwrap();
        let state = restarted.state().unwrap();
        assert_eq!(state.agents.len(), 1, "recovery never launches Claude again");
        let run = &state.schedule.runs[&ScheduleRunId::from("uncertain")];
        assert_eq!(run.state, ScheduleRunState::Failed);
        assert_eq!(run.launch.as_ref().unwrap().state, ScheduleLaunchState::Failed);
        assert!(run.error.as_deref().unwrap().contains("did not start properly"), "{:?}", run.error);
        assert!(state.tasks[run.task_id.as_ref().unwrap()].agent_id.is_none(), "nothing is assigned the uncertain run's task");
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
        execute_system(&fixture.domain, Command::CancelTask { task_id: task, reason: None }).unwrap();
        let restarted = Fixture::open(&fixture.home);
        run_cycle(&restarted, &fixture.home, false).await.unwrap();

        let state = restarted.state().unwrap();
        assert_eq!(state.schedule.runs[&ScheduleRunId::from("closed")].state, ScheduleRunState::Failed);
        assert!(!state.interactions.contains_key(&InteractionId::from("problem-launch-closed")), "no Problem on a closed Task");
        let revision = state.revision;
        run_cycle(&restarted, &fixture.home, false).await.unwrap();
        assert_eq!(restarted.state().unwrap().revision, revision, "the run is not retried every cycle");
    }

    /// Puts a run back as a daemon before the fix left it: still waiting to launch after its Task
    /// was cancelled.
    fn leave_launch_waiting(domain: &DomainRuntime, run: &str) {
        let store = domain.store.lock().unwrap();
        let conn = store.conn_for_test();
        let triggers: Vec<(String, String)> = conn
            .prepare("SELECT name, sql FROM sqlite_master WHERE type='trigger' AND name IN ('schedule_run_result_transition','schedule_launch_transition')")
            .unwrap()
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        assert_eq!(triggers.len(), 2);
        for (name, _) in &triggers {
            conn.execute_batch(&format!("DROP TRIGGER {name}")).unwrap();
        }
        conn.execute(
            "UPDATE schedule_runs SET state='sent', error=NULL, launch=json_set(launch,'$.state','pending','$.finished_at',NULL,'$.error',NULL) WHERE id=?1",
            [run],
        )
        .unwrap();
        for (_, sql) in &triggers {
            conn.execute_batch(sql).unwrap();
        }
    }

    // On 0.3.14 four cancelled High-priority scheduled tasks kept their launch waiting: the gate
    // let them through every minute, their claim was refused, and nothing else ever started.
    #[tokio::test]
    async fn a_launch_left_waiting_by_a_cancelled_task_never_passes_the_gate_and_ends_on_the_next_start() {
        let fixture = Fixture::new();
        fixture.claim("stuck");
        finish_claim(&fixture.domain, &fixture.home, &"stuck".into()).unwrap();
        let task = fixture.domain.state().unwrap().schedule.runs[&ScheduleRunId::from("stuck")].task_id.clone().unwrap();
        execute_system(&fixture.domain, Command::CancelTask { task_id: task.clone(), reason: None }).unwrap();
        leave_launch_waiting(&fixture.domain, "stuck");

        let restarted = Fixture::open(&fixture.home);
        let state = restarted.state().unwrap();
        let run = &state.schedule.runs[&ScheduleRunId::from("stuck")];
        assert_eq!((run.state, run.launch.as_ref().unwrap().state), (ScheduleRunState::Sent, ScheduleLaunchState::Pending));
        assert_eq!(state.tasks[&task].state, TaskState::Cancelled);
        let waiting = crate::admission::Waiting {
            key: crate::admission::Key::Run("stuck".into()),
            priority: alondra_core::domain::TaskPriority::High,
            resuming: false,
            since: 0,
        };
        assert!(restarted.live.gate.ask(&restarted, waiting, alondra_core::now_millis()).is_err(), "the gate never lets a launch of a cancelled task through");

        run_cycle(&restarted, &fixture.home, false).await.unwrap();
        let state = restarted.state().unwrap();
        let run = &state.schedule.runs[&ScheduleRunId::from("stuck")];
        assert_eq!(run.state, ScheduleRunState::Failed);
        assert_eq!(run.launch.as_ref().unwrap().state, ScheduleLaunchState::Failed);
        assert_eq!(run.error.as_deref(), Some(alondra_core::domain::SCHEDULED_TASK_CANCELLED));
        assert!(!state.interactions.contains_key(&InteractionId::from("problem-launch-stuck")), "no Problem on a cancelled Task");
        let revision = state.revision;
        run_cycle(&restarted, &fixture.home, false).await.unwrap();
        assert_eq!(restarted.state().unwrap().revision, revision, "it is settled once");
    }

    #[tokio::test]
    async fn cancelling_a_scheduled_task_before_it_launches_ends_its_run_for_good() {
        let fixture = Fixture::new();
        fixture.claim("cancelled");
        finish_claim(&fixture.domain, &fixture.home, &"cancelled".into()).unwrap();
        let task = fixture.domain.state().unwrap().schedule.runs[&ScheduleRunId::from("cancelled")].task_id.clone().unwrap();
        execute_system(&fixture.domain, Command::CancelTask { task_id: task, reason: None }).unwrap();

        let restarted = Fixture::open(&fixture.home);
        let run = &restarted.state().unwrap().schedule.runs[&ScheduleRunId::from("cancelled")];
        assert_eq!(run.state, ScheduleRunState::Failed);
        assert_eq!(run.launch.as_ref().unwrap().state, ScheduleLaunchState::Failed);
        assert_eq!(run.error.as_deref(), Some(alondra_core::domain::SCHEDULED_TASK_CANCELLED));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn an_uncertain_launch_fails_its_run_at_once_and_is_retried_once_on_request() {
        let fixture = Fixture::new();
        fixture.claim("lost");
        finish_claim(&fixture.domain, &fixture.home, &"lost".into()).unwrap();
        execute_system(
            &fixture.domain,
            Command::ClaimScheduleLaunch { run_id: "lost".into(), now: alondra_core::now_millis() },
        )
        .unwrap();
        let restarted = Arc::new(Fixture::open(&fixture.home));
        let _sdk = SdkLaunches::enable(&restarted, &fixture.home);
        run_cycle(&restarted, &fixture.home, false).await.unwrap();

        let state = restarted.state().unwrap();
        let run = &state.schedule.runs[&ScheduleRunId::from("lost")];
        assert_eq!(run.state, ScheduleRunState::Failed);
        assert_eq!(run.launch.as_ref().unwrap().state, ScheduleLaunchState::Failed);
        assert!(run.error.as_deref().unwrap().contains("did not start properly"));
        assert!(state.agents.is_empty(), "nothing is started for it");
        let problem = &state.interactions[&InteractionId::from("problem-launch-lost")];
        let InteractionPayload::Question { options, .. } = &problem.payload else { panic!("a Problem asks") };
        assert_eq!(
            options.iter().map(|option| option.id.as_str()).collect::<Vec<_>>(),
            ["retry", "seen"]
        );

        answer_problem(&restarted, "problem-launch-lost", "retry", "answer-retry");
        run_cycle(&restarted, &fixture.home, false).await.unwrap();
        run_cycle(&restarted, &fixture.home, false).await.unwrap();
        let state = restarted.state().unwrap();
        assert_eq!(state.agents.len(), 1, "the retry launches the schedule once");
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
    async fn a_repository_alondra_cannot_trust_is_never_launched_and_the_orchestrator_hears_why() {
        let fixture = Fixture::new();
        let sdk = SdkLaunches::enable(&fixture.domain, &fixture.home);
        let config = fixture.root.join(".claude.json");
        std::fs::write(&config, r#"{"projects":{}}"#).unwrap();
        crate::claude_trust::use_config_for_test(&config);
        std::fs::create_dir(fixture.root.join(".claude.json.lock")).unwrap();
        fixture.claim("untrusted");
        run_cycle(&fixture.domain, &fixture.home, false).await.unwrap();
        assert_eq!(fixture.launches(), 0, "Claude is never asked to start there");
        assert!(sdk.starts().is_empty());
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
        run_cycle(&fixture.domain, &fixture.home, false).await.unwrap();
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
        assert!(state.agents.is_empty(), "a launch that did not start registers no node");
        let task_id = run.task_id.as_ref().unwrap();
        let told = fixture.domain.store.lock().unwrap().pending_messages().unwrap();
        assert!(
            told.iter().any(|message| message.recipient_kind == "orchestrator"
                && message.task_id.as_deref() == Some(task_id.0.as_str())
                && message.message.contains("could not start: this Alondra has no Agent SDK host")),
            "the orchestrator hears why the scheduled Task did not start: {told:?}"
        );
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
        assert!(
            run_cycle(&fixture.domain, &fixture.home, false)
                .await
                .is_err()
        );
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
        let restarted = Arc::new(Fixture::open(&fixture.home));
        let sdk = SdkLaunches::enable(&restarted, &fixture.home);
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
        run_cycle(&restarted, &fixture.home, false)
            .await
            .unwrap();
        assert!(restarted.state().unwrap().agents.is_empty() && sdk.starts().is_empty(), "recovery never launches Claude again");
        assert_eq!(
            restarted.state().unwrap().schedule.runs[&ScheduleRunId::from("rollback")].state,
            ScheduleRunState::Failed
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_due_schedule_and_a_pending_launch_wait_out_the_usage_limit_and_each_launch_once_after_it() {
        let fixture = Fixture::new();
        let _sdk = SdkLaunches::enable(&fixture.domain, &fixture.home);
        fixture.claim("manual-1");
        let now = alondra_core::now_millis();
        fixture.domain.live.usage_pause.exhaust(Some(now + 3_600_000), now);
        let runs = || fixture.domain.state().unwrap().schedule.runs.len();
        run_cycle(&fixture.domain, &fixture.home, false).await.unwrap();
        let mut due = fixture.domain.state().unwrap().schedule.schedules[&ScheduleId::from("schedule-1")].clone();
        due.enabled = true;
        due.next_run = Some(now);
        due.revision += 1;
        execute_system(&fixture.domain, Command::UpdateSchedule { schedule: due }).unwrap();

        run_cycle(&fixture.domain, &fixture.home, true).await.unwrap();
        run_cycle(&fixture.domain, &fixture.home, true).await.unwrap();
        assert_eq!(fixture.launches(), 0, "paused: Claude is not called");
        assert_eq!(runs(), 1, "the due schedule starts no run: it is deferred, not failed");
        let state = fixture.domain.state().unwrap();
        let manual = &state.schedule.runs[&ScheduleRunId::from("manual-1")];
        assert_eq!(manual.launch.as_ref().unwrap().state, ScheduleLaunchState::Pending, "the claimed launch waits too");
        assert!(state.schedule.runs.values().all(|run| run.state != ScheduleRunState::Failed), "nothing is recorded as failed");
        assert_eq!(state.schedule.schedules[&ScheduleId::from("schedule-1")].next_run, Some(now), "still due");

        fixture.domain.live.usage_pause.resume(alondra_core::now_millis());
        run_cycle(&fixture.domain, &fixture.home, true).await.unwrap();
        assert_eq!(fixture.launches(), 2, "back: the deferred launch and the due schedule go, one each");
        assert_eq!(runs(), 2);
        run_cycle(&fixture.domain, &fixture.home, true).await.unwrap();
        assert_eq!(fixture.launches(), 2, "and never again");
        assert_eq!(runs(), 2);
        let state = fixture.domain.state().unwrap();
        assert!(state.schedule.runs.values().all(|run| run.launch.as_ref().is_some_and(|launch| launch.state == ScheduleLaunchState::Handed)));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_due_schedule_and_a_pending_launch_wait_while_the_disk_is_nearly_full() {
        let fixture = Fixture::new();
        let _sdk = SdkLaunches::enable(&fixture.domain, &fixture.home);
        fixture.claim("manual-1");
        let now = alondra_core::now_millis();
        fixture.domain.live.machine_watch.set_disk_low(true);
        run_cycle(&fixture.domain, &fixture.home, false).await.unwrap();
        let mut due = fixture.domain.state().unwrap().schedule.schedules[&ScheduleId::from("schedule-1")].clone();
        due.enabled = true;
        due.next_run = Some(now);
        due.revision += 1;
        execute_system(&fixture.domain, Command::UpdateSchedule { schedule: due }).unwrap();

        run_cycle(&fixture.domain, &fixture.home, true).await.unwrap();
        run_cycle(&fixture.domain, &fixture.home, true).await.unwrap();
        assert_eq!(fixture.launches(), 0, "no node starts on a nearly full disk");
        let state = fixture.domain.state().unwrap();
        assert_eq!(state.schedule.runs[&ScheduleRunId::from("manual-1")].launch.as_ref().unwrap().state, ScheduleLaunchState::Pending);
        assert!(state.schedule.runs.values().all(|run| run.state != ScheduleRunState::Failed), "nothing is recorded as failed");

        fixture.domain.live.machine_watch.set_disk_low(false);
        run_cycle(&fixture.domain, &fixture.home, true).await.unwrap();
        assert_eq!(fixture.launches(), 2, "space again: the deferred launch and the due schedule go");
    }
}
