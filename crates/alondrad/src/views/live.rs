use super::reset_time;
use alondra_core::Millis;
use alondra_core::domain::{DomainState, NodePhase, PrState, PullRequest, TaskId, TaskState};
use alondra_core::views::{
    Gauge, InboxTime, Lang, Level, LiveActivity, LiveBackground, LiveHistoryStep, LiveNode, LiveOrchestrator,
    LiveCheck, LivePullRequest, LiveRun, LiveSnapshot, LiveStep, Notice, StepState, Tone,
};
use crate::node_work::StepStatus;
use crate::node_reports::Activity;
use serde_json::Value;
use sha2::Digest;
use std::{collections::{BTreeMap, HashMap}, sync::Mutex};

#[derive(Default)]
pub(crate) struct LiveReadCache(Mutex<HashMap<String, (u64, String, Value)>>);

impl LiveReadCache {
    fn get(&self, path: &str, domain_revision: u64, source_revision: &str) -> Option<Value> {
        self.0.lock().expect("live read cache poisoned").get(path)
            .filter(|(domain, source, _)| *domain == domain_revision && source == source_revision)
            .map(|(_, _, value)| value.clone())
    }

    fn insert(&self, path: String, domain_revision: u64, source_revision: &str, value: Value) {
        let mut cache = self.0.lock().expect("live read cache poisoned");
        if cache.len() >= 128 && !cache.contains_key(&path) { cache.clear(); }
        cache.insert(path, (domain_revision, source_revision.to_owned(), value));
    }
}

/// The project a node was launched for, which a restart cannot lose: its task's request, or the
/// project its name starts with.
fn launched_project(
    state: &DomainState,
    agent: &alondra_core::domain::Agent,
    launches: &BTreeMap<alondra_core::domain::RequestId, (Option<String>, Option<String>)>,
) -> Option<String> {
    state.tasks.values()
        .filter(|task| task.agent_id.as_ref() == Some(&agent.id) && !task.archived)
        .find_map(|task| launches.get(&task.request_id).and_then(|(project, _)| project.clone()))
        .or_else(|| agent.name.split_once('/').map(|(project, _)| project.to_owned()).filter(|project| !project.is_empty()))
}

fn node_label(key: &str, lang: Lang) -> String {
    match (key, lang) {
        ("stalled", Lang::En) => "stalled",
        ("stalled", Lang::Es) => "parado",
        ("failed", Lang::En) => "failed",
        ("failed", Lang::Es) => "falló",
        ("busy", Lang::En) => "working",
        ("busy", Lang::Es) => "trabajando",
        ("waiting_machine", Lang::En) => "waiting for the machine",
        ("waiting_machine", Lang::Es) => "esperando máquina",
        (_, Lang::En) => "idle",
        (_, Lang::Es) => "inactivo",
    }.to_owned()
}

pub(crate) async fn gather(
    client: &crate::client_routes::Client,
    state: &DomainState,
    lang: Lang,
    keys: &[String],
    allow_issue_refresh: bool,
    clock_only: bool,
) -> LiveSnapshot {
    gather_at(client, state, lang, keys, allow_issue_refresh, clock_only, alondra_core::now_millis()).await
}

async fn gather_at(
    client: &crate::client_routes::Client,
    state: &DomainState,
    lang: Lang,
    keys: &[String],
    allow_issue_refresh: bool,
    clock_only: bool,
    now: Millis,
) -> LiveSnapshot {
    let exhausted = client.domain.live.usage_pause.current();
    let (launches, refusals, retries) = {
        let store = client.domain.store.lock().expect("domain store poisoned");
        (store.request_launch_summaries().unwrap_or_default(), store.launch_refusals().unwrap_or_default(), store.start_retries().unwrap_or_default())
    };
    let start_waits = {
        let store = client.domain.store.lock().expect("domain store poisoned");
        state.tasks.values()
            .filter(|task| task.state == TaskState::NotStarted && !task.archived && task.agent_id.is_none())
            .filter(|task| client.domain.live.closed_task(&task.id.0).is_none() && !store.task_was_launched(&task.id).unwrap_or(true))
            .map(|task| {
                let wait = match retries.get(&task.id) {
                    Some((_, at)) => alondra_core::views::LiveStartWait::Retry { at: crate::usage_pause::clock_text(*at, now, lang == Lang::Es) },
                    None => alondra_core::views::LiveStartWait::Room,
                };
                (task.id.0.clone(), wait)
            })
            .collect()
    };
    let launch_refusals = refusals.into_iter()
        .filter(|(task, _, _)| state.tasks.get(task).is_some_and(|task| task.state == TaskState::NotStarted))
        .map(|(task, reason, detail)| {
            let detail = if reason == "workspace_untrusted" { detail.map(|folder| home_relative(&folder)) } else { detail };
            (task.0, alondra_core::views::LiveLaunchRefusal { reason, detail })
        })
        .collect();
    let nodes = state
        .agents
        .values()
        .filter_map(|agent| {
            let session_id = agent.id.0.clone();
            let reported = client.domain.live.mods.session(&session_id).unwrap_or_default();
            let open_task = state.tasks.values().any(|task| task.agent_id.as_ref() == Some(&agent.id) && !task.archived
                && !matches!(task.state, TaskState::Done | TaskState::Closing | TaskState::Cancelled));
            let (node, state_since) = match client.domain.live.node(&session_id) {
                Some(node) => {
                    let state_since = Some(node.state_since);
                    (node, state_since)
                }
                None if reported.started_at.is_some() || agent.phase.live() || open_task => {
                    let mut node = alondra_core::Node::new(&session_id, 0);
                    node.cwd = reported.cwd.clone();
                    (node, None)
                }
                None => return None,
            };
            let project = if node.cwd.is_some() || node.is_orchestrator {
                node.project()
            } else {
                launched_project(state, agent, &launches).unwrap_or_else(|| node.project())
            };
            let activity = match reported.activity(now) {
                Activity::Stalled if crate::domain_holds::waits_on_user(state, &session_id) => Activity::Working,
                activity => activity,
            };
            let state_key = match activity {
                Activity::Stalled => "stalled",
                Activity::Failed => "failed",
                Activity::Working => "busy",
                Activity::Idle if node.status == alondra_core::NodeStatus::WaitingMachine => "waiting_machine",
                Activity::Idle => "free",
            };
            let state_label = node_label(state_key, lang);
            let tool = reported.tool.as_ref().map(|tool| match &tool.summary {
                Some(summary) => format!("{}: {summary}", tool.name),
                None => tool.name.clone(),
            });
            let answer = reported.answer.as_ref().filter(|answer| !answer.text.trim().is_empty());
            let failure = reported.job.as_ref().filter(|_| activity == Activity::Failed).map(|job| job.detail.clone()).filter(|detail| !detail.is_empty());
            let summary = failure.or_else(|| tool.clone())
                .or_else(|| node.daemon_needs.clone())
                .or_else(|| node.daemon_detail.clone())
                .or_else(|| answer.map(|answer| answer.text.clone()))
                .map(|summary| summary.lines().next().unwrap_or("").chars().take(141).collect());
            let background: Vec<(&str, alondra_core::BackgroundItem)> = reported.background.iter()
                .map(|(id, _)| id.as_str()).zip(reported.background_at(now)).collect();
            let suspect = |id: &str| background.iter().any(|(known, item)| *known == id && item.suspect);
            let shown_as_run = |id: &str| reported.work.agents.iter().any(|agent| agent.id == id && agent.finished_at.is_none())
                || reported.work.workflows.iter().any(|workflow| workflow.id == id && workflow.finished_at.is_none());
            let counts = [alondra_core::BackgroundKind::Agent, alondra_core::BackgroundKind::Shell,
                alondra_core::BackgroundKind::Other].into_iter().filter_map(|kind| {
                    let count = background.iter().filter(|(_, item)| item.kind == kind).count();
                    (count > 0).then(|| {
                        let noun = match (lang, kind) {
                            (Lang::En, alondra_core::BackgroundKind::Agent) => "subagents",
                            (Lang::En, alondra_core::BackgroundKind::Shell) => "shells",
                            (Lang::En, alondra_core::BackgroundKind::Other) => "tasks",
                            (Lang::Es, alondra_core::BackgroundKind::Agent) => "subagentes",
                            (Lang::Es, alondra_core::BackgroundKind::Shell) => "shells",
                            (Lang::Es, alondra_core::BackgroundKind::Other) => "tareas",
                        };
                        format!("{count} {noun}")
                    })
                }).collect::<Vec<_>>();
            let background_suspect = background.iter().any(|(_, item)| item.suspect);
            let background_badge = (!counts.is_empty()).then(|| {
                let mut text = counts.join(" · ");
                if background_suspect { text.push_str(match lang { Lang::En => " · hung?", Lang::Es => " · ¿colgado?" }); }
                text
            });
            Some(LiveNode {
                agent_id: node.session_id,
                short_id: Some(node.short_id),
                cwd: node.cwd.clone(),
                name: agent.name.clone(),
                project: Some(project),
                state_key: Some(state_key.to_owned()),
                state_label: Some(state_label),
                summary,
                needs: node.daemon_needs.clone(),
                subagents: reported.work.agents.iter().map(|agent| LiveRun {
                    label: agent.description.clone(),
                    kind: agent.kind.clone(),
                    model: agent.model.clone(),
                    agents: None,
                    since: timestamp(agent.started_at),
                    ran_secs: agent.finished_at.map(|end| end.saturating_sub(agent.started_at).max(0) as u64 / 1000),
                    suspect: agent.finished_at.is_none() && suspect(&agent.id),
                    workflow: None,
                }).collect(),
                workflows: reported.work.workflows.iter().map(|workflow| {
                    let state = workflow.transcript_dir.as_deref()
                        .and_then(|dir| crate::workflow_state::read(dir, workflow.finished_at.is_some()));
                    LiveRun {
                        label: workflow.name.clone(),
                        kind: None,
                        model: None,
                        agents: state.as_ref().map(|state| state.running + state.done)
                            .or_else(|| workflow.transcript_dir.as_deref().and_then(crate::node_work::workflow_agents)),
                        since: timestamp(workflow.started_at),
                        ran_secs: workflow.finished_at.map(|end| end.saturating_sub(workflow.started_at).max(0) as u64 / 1000),
                        suspect: workflow.finished_at.is_none() && suspect(&workflow.id),
                        workflow: state,
                    }
                }).collect(),
                steps: reported.work.steps.iter().map(|step| LiveStep {
                    subject: step.subject.clone(),
                    state: match step.status {
                        StepStatus::InProgress => StepState::InProgress,
                        StepStatus::Completed => StepState::Completed,
                        StepStatus::Pending | StepStatus::Deleted | StepStatus::Unknown => StepState::Pending,
                    },
                    active_form: step.active_form.clone(),
                }).collect(),
                background: background.iter().filter(|(id, _)| !shown_as_run(id)).map(|(_, item)| LiveBackground {
                    kind: serde_json::to_value(item.kind).ok().and_then(|value| value.as_str().map(str::to_owned)).unwrap_or_else(|| "other".into()),
                    label: item.label.clone(),
                    since: timestamp(item.started_at),
                    suspect: item.suspect,
                }).collect(),
                background_badge,
                background_suspect,
                working: agent.phase == NodePhase::Working,
                waiting: matches!(agent.phase, NodePhase::WaitingUser { .. } | NodePhase::Paused(_)),
                quiet: agent.phase == NodePhase::Stopped
                    && state_since.is_some_and(|since| now.saturating_sub(since.max(answer.map_or(0, |answer| answer.at)))
                        >= alondra_core::states::NODE_QUIET_MS),
                state_since: state_since.map(timestamp),
                last_message: answer.map(|answer| answer.text.clone()),
                last_message_at: answer.map(|answer| timestamp(answer.at)),
                last_turn_aborted: answer.is_some_and(|answer| answer.aborted),
                tool: tool.map(|tool| tool.chars().take(141).collect()),
                context_percent: reported.context.as_ref().and_then(|context| context.percent).map(|percent| percent.round().clamp(0.0, 999.0) as u32),
                context_window: reported.context.as_ref().map(|context| context.window),
                cost_cents: reported.cost_usd.map(|usd| (usd * 100.0).round().max(0.0) as u64),
                model: reported.model.clone(),
                reconnecting: agent.phase == NodePhase::Starting { reviving: true },
                disconnected: agent.phase == NodePhase::Closed && open_task,
                waiting_until: exhausted.as_ref().filter(|_| node.usage_limited).map(|exhausted| timestamp(exhausted.until)),
            })
        })
        .collect();
    let pull_requests = state
        .pull_requests
        .values()
        .map(|pr| {
            let details = client.details.get(&pr.id.0);
            LivePullRequest {
                id: pr.id.0.clone(),
                title: details.as_ref().and_then(|details| details.title.clone()),
                body: details.as_ref().and_then(|details| details.body.clone()),
                head_ref: details.as_ref().and_then(|details| details.head_ref.clone()),
                base_ref: details.as_ref().and_then(|details| details.base_ref.clone()),
                checks: details.as_ref().map(|details| {
                    details.checks.iter().map(|check| LiveCheck {
                        name: check.name.clone(),
                        state: check_state(check.state),
                    }).collect()
                }).unwrap_or_default(),
                additions: details.as_ref().map_or(0, |details| details.additions),
                deletions: details.as_ref().map_or(0, |details| details.deletions),
                changed_files: details.as_ref().map_or(0, |details| details.changed_files),
                checks_passed: details.as_ref().map(|details| {
                    details
                        .checks
                        .iter()
                        .all(|check| check.state == alondra_core::pr::CheckState::Passed)
                }),
                conflicts: details.as_ref().is_some_and(|details| details.conflicts),
                stacked_on: stacked_below(pr, state).map(|below| below.id.0.clone()),
                url: Some(alondra_core::pr::canonical(&pr.repository, pr.number)),
            }
        })
        .collect();
    let orchestrator = crate::domain_orchestrator::status(&crate::domain_orchestrator::executor(
        client.domain.clone(),
        client.home(),
    ))
    .ok()
    .map(|value| LiveOrchestrator {
        working: value["phase"] == "running",
        summary: value["last_error"].as_str().map(str::to_owned),
        failure: value["failure"].as_str().map(str::to_owned),
        queue_depth: value["queue_depth"].as_u64().unwrap_or(0).min(u32::MAX as u64) as u32,
        active_run_id: value["active_run_id"].as_str().map(str::to_owned),
        can_retry: value["can_retry"].as_bool().unwrap_or(false),
        can_cancel: value["can_cancel"].as_bool().unwrap_or(false),
    });
    let update = crate::updates::status(client.home());
    let update = update.available.then(|| Notice {
        text: match lang {
            Lang::En => format!("Alondra {} is available", update.latest.unwrap_or_default()),
            Lang::Es => format!(
                "Alondra {} está disponible",
                update.latest.unwrap_or_default()
            ),
        },
        tone: Tone::Info,
        action: None,
    });
    let inbox_times = if clock_only { vec![] } else {
        let orders = state.inbox_orders();
        let revisions = orders.iter().map(|(_, order)| *order).collect::<Vec<_>>();
        client
            .domain
            .store
            .lock()
            .expect("domain store poisoned")
            .revision_times(&revisions)
            .ok()
            .map(|times| {
                orders
                    .into_iter()
                    .filter_map(|(id, order)| {
                        times.get(&order).copied().map(|at| InboxTime {
                            id,
                            at: timestamp(at),
                        })
                    })
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default()
    };
    let sample = client
        .gauges
        .lock()
        .expect("machine gauges poisoned")
        .clone();
    let gauges = if let Some((machine, usage)) = sample {
        let mut published = client.views_gauges.lock().expect("published views gauges poisoned");
        published_gauges(&mut published, std::time::Instant::now(), lang, || (
            gauges(&machine, usage.as_ref(), Lang::En, machine.at),
            gauges(&machine, usage.as_ref(), Lang::Es, machine.at),
        ))
    } else { vec![] };
    let request_times = if clock_only { BTreeMap::new() } else { client
        .domain
        .store
        .lock()
        .expect("domain store poisoned")
        .request_times()
        .unwrap_or_default()
        .into_iter()
        .map(|(id, at)| (id.0, timestamp(at)))
        .collect() };
    let (task_since, cancel_reasons) = if clock_only { (BTreeMap::new(), BTreeMap::new()) } else {
        let (times, reasons) = {
            let store = client.domain.store.lock().expect("domain store poisoned");
            (store.task_state_times().unwrap_or_default(), store.cancel_reasons().unwrap_or_default())
        };
        let since = state.tasks.values().filter(|task| !task.archived)
            .filter_map(|task| times.get(&task.id).map(|at| (task.id.0.clone(), timestamp(*at))))
            .collect();
        (since, reasons.into_iter().map(|(task, reason)| (task.0, reason)).collect())
    };
    let task_options = state.tasks.values().map(|task| {
        let (project, effort) = launches.get(&task.request_id).cloned().unwrap_or_default();
        (task.id.0.clone(), alondra_core::views::live::LiveTaskOptions { project, effort })
    }).collect();
    let wants_new = keys
        .iter()
        .any(|key| key.as_str() == "new_request" || key.starts_with("new_request?"));
    let mut projects = Vec::new();
    let mut options = Vec::new();
    let mut project_options = BTreeMap::new();
    if wants_new {
        if let Some(data) = read_json(client, "/projects").await {
            projects = serde_json::from_value(data["projects"].clone()).unwrap_or_default();
        }
        if let Some(data) = read_json(client, "/options").await {
            options = serde_json::from_value(data["options"].clone()).unwrap_or_default();
        }
        let selected: std::collections::BTreeSet<_> = keys
            .iter()
            .filter_map(|key| key.split_once('?').map(|(_, query)| query))
            .flat_map(|query| url::form_urlencoded::parse(query.as_bytes()))
            .filter(|(name, _)| name == "project")
            .map(|(_, project)| project.into_owned())
            .filter(|project| !project.is_empty())
            .collect();
        for project in selected {
            let query = url::form_urlencoded::Serializer::new(String::new())
                .append_pair("project", &project)
                .finish();
            if let Some(data) = read_json(client, &format!("/options?{query}")).await {
                let catalog = serde_json::from_value(data["options"].clone()).unwrap_or_default();
                project_options.insert(project, catalog);
            }
        }
    }
    let mut issues = Vec::new();
    let mut issue_error = None;
    let issue_keys: Vec<_> = keys
        .iter()
        .filter(|key| key.as_str() == "issue_picker" || key.starts_with("issue_picker?"))
        .collect();
    if !issue_keys.is_empty() {
        if allow_issue_refresh {
            let refresh = issue_keys.iter().any(|key| key.contains("refresh=1"));
            let path = if refresh { "/issues?refresh=1" } else { "/issues" };
            if let Some(data) = read_json(client, path).await {
                issues = serde_json::from_value(data["issues"].clone()).unwrap_or_default();
                issue_error = data["error"].as_str().map(str::to_owned);
            }
        } else if let Some(cached) = client.cached_issues().await {
            issues = cached.issues;
            issue_error = cached.error;
        }
    }
    let mut activity = BTreeMap::new();
    for id in keys
        .iter()
        .filter_map(|key| key.strip_prefix("node:"))
        .filter(|id| !matches!(state.resolve_target(id), alondra_core::domain::Target::Orchestrator(_)))
    {
        let path = format!("/agents/{id}/activity");
        let activity_revision = activity_source_revision(client, id);
        if let Some(data) = read_live_json(client, &path, state.revision, &activity_revision).await {
            let entries: Vec<alondra_core::activity::Activity> =
                serde_json::from_value(data["activity"].clone()).unwrap_or_default();
            activity.insert(
                id.to_owned(),
                entries
                    .into_iter()
                    .map(|entry| LiveActivity {
                        agent_id: entry.agent_id,
                        agent_type: entry.agent_type,
                        task: entry.task,
                        tool: entry.tool,
                        summary: entry.summary,
                        since: timestamp(entry.since),
                    })
                    .collect(),
            );
        }
    }
    let mut task_history = BTreeMap::new();
    for id in keys.iter().filter_map(|key| key.strip_prefix("task:")) {
        let task = TaskId(id.to_owned());
        if let Ok(history) = client
            .domain
            .store
            .lock()
            .expect("domain store poisoned")
            .task_history(&task)
        {
            task_history.insert(
                id.to_owned(),
                history
                    .into_iter()
                    .map(|step| LiveHistoryStep {
                        at: timestamp(step.at),
                        state: step.state,
                        summary: step.summary,
                    })
                    .collect(),
            );
        }
    }
    let mut search_matches = None;
    if let Some(query) = keys.iter().filter_map(|key| key.strip_prefix("search?q=")).next() {
        let query = url::form_urlencoded::parse(format!("q={query}").as_bytes())
            .find(|(name, _)| name == "q").map(|(_, value)| value.into_owned()).unwrap_or_default();
        if query.chars().count() >= 3 {
            let path = format!("/agents/search?q={}", url::form_urlencoded::byte_serialize(query.as_bytes()).collect::<String>());
            let search_revision = search_source_revision(client, state);
            if let Some(data) = read_live_json(client, &path, state.revision, &search_revision).await {
                search_matches = serde_json::from_value(data["sessions"].clone()).ok();
            }
        }
    }
    let (allowed_rules, allow_always_missing) = allowed_rules(client, keys);
    LiveSnapshot {
        closed_nodes: client.domain.live.closed_sessions(),
        closed_tasks: client.domain.live.closed_task_ids(),
        nodes,
        pull_requests,
        orchestrator,
        update,
        gauges,
        inbox_times,
        request_times,
        projects,
        options,
        project_options,
        issues,
        issue_error,
        activity,
        task_history,
        task_options,
        task_since,
        cancel_reasons,
        search_matches,
        usage_exhausted: exhausted.map(|exhausted| alondra_core::views::LiveUsageExhausted {
            since: timestamp(exhausted.since),
            until: timestamp(exhausted.until),
            paused_by_user: exhausted.by_user,
        }),
        launch_refusals,
        start_waits,
        pacing: client.domain.live.gate.live(&client.domain, now),
        allowed_rules,
        allow_always_missing,
    }
}

/// This Mac's rules «allow always», checked against their files only when its settings are on
/// screen, and the cards whose rules Claude Code never wrote.
fn allowed_rules(client: &crate::client_routes::Client, keys: &[String]) -> (Vec<alondra_core::views::LiveAllowedRule>, Vec<String>) {
    use crate::allow_always::RuleState;
    let rows = if keys.iter().any(|key| key == "machine") {
        crate::allow_always::reconcile(&client.domain)
    } else {
        client.domain.store.lock().expect("domain store poisoned").allowed_rules().unwrap_or_default()
    };
    let mut missing: Vec<String> = rows.iter().filter(|row| row.state == RuleState::Missing).map(|row| row.interaction_id.clone()).collect();
    missing.sort();
    missing.dedup();
    let active = rows.into_iter().filter(|row| row.state == RuleState::Active).map(|row| alondra_core::views::LiveAllowedRule {
        id: row.id,
        repo: std::path::Path::new(&row.repo).file_name().map(|name| name.to_string_lossy().into_owned()).unwrap_or(row.repo.clone()),
        rule: row.rule,
        added: timestamp(row.added_at),
        task_id: row.task_id,
    }).collect();
    (active, missing)
}

fn home_relative(folder: &str) -> String {
    match std::env::var("HOME").ok().filter(|home| !home.is_empty()).and_then(|home| folder.strip_prefix(&home).map(str::to_owned)) {
        Some(rest) if rest.starts_with('/') => format!("~{rest}"),
        _ => folder.to_owned(),
    }
}

fn check_state(state: alondra_core::pr::CheckState) -> alondra_core::views::CheckState {
    use alondra_core::{pr::CheckState as Github, views::CheckState as View};
    match state {
        Github::Passed => View::Pass,
        Github::Failed => View::Fail,
        Github::Running | Github::Other => View::Pending,
    }
}

fn direct_stack_parent<'a>(pr: &PullRequest, state: &'a DomainState) -> Option<&'a PullRequest> {
    if pr.state != PrState::Open { return None; }
    state.pull_requests.values()
        .filter(|candidate| candidate.id != pr.id && candidate.state == PrState::Open
            && candidate.host == pr.host && candidate.repository == pr.repository
            && candidate.head == pr.base)
        .min_by_key(|candidate| candidate.number)
}

fn stacked_below<'a>(pr: &PullRequest, state: &'a DomainState) -> Option<&'a PullRequest> {
    let direct = direct_stack_parent(pr, state)?;
    let mut seen = std::collections::HashSet::new();
    let mut current = direct;
    loop {
        if current.id == pr.id || !seen.insert(current.id.clone()) { return None; }
        let Some(next) = direct_stack_parent(current, state) else { return Some(direct); };
        current = next;
    }
}

async fn read_json(client: &crate::client_routes::Client, path: &str) -> Option<Value> {
    let response = client.read(path).await?;
    if !response.status().is_success() {
        return None;
    }
    let bytes = axum::body::to_bytes(response.into_body(), 2 * 1024 * 1024)
        .await
        .ok()?;
    serde_json::from_slice(&bytes).ok()
}

async fn read_live_json(
    client: &crate::client_routes::Client,
    path: &str,
    domain_revision: u64,
    source_revision: &str,
) -> Option<Value> {
    if let Some(cached) = client.live_reads.get(path, domain_revision, source_revision) {
        return Some(cached);
    }
    let value = read_json(client, path).await?;
    client.live_reads.insert(path.to_owned(), domain_revision, source_revision, value.clone());
    Some(value)
}

fn search_source_revision(client: &crate::client_routes::Client, state: &DomainState) -> String {
    let nodes = client.domain.live.nodes();
    let candidates: Vec<_> = state.agents.values().map(|agent| {
        let node = nodes.iter().find(|node| node.session_id == agent.id.0);
        let tasks: Vec<&str> = state.tasks.values()
            .filter(|task| task.agent_id.as_ref() == Some(&agent.id)).map(|task| task.title.as_str()).collect();
        let text = format!("{} {} {} {} {}", agent.name, node.map_or(String::new(), |n| n.short_id.clone()),
            node.map_or(String::new(), |n| n.project()),
            node.and_then(|n| n.last_message.clone()).unwrap_or_default(), tasks.join(" "));
        (agent.id.0.clone(), text)
    }).collect();
    let bytes = serde_json::to_vec(&candidates).expect("search source serialization");
    digest_hex(&bytes)
}

fn activity_source_revision(client: &crate::client_routes::Client, id: &str) -> String {
    client.domain.live.threads(id)
        .and_then(|activity| serde_json::to_vec(&activity).ok())
        .map(|bytes| digest_hex(&bytes))
        .unwrap_or_default()
}

fn published_gauges(
    published: &mut Option<(std::time::Instant, Vec<Gauge>, Vec<Gauge>)>,
    now: std::time::Instant,
    lang: Lang,
    compute: impl FnOnce() -> (Vec<Gauge>, Vec<Gauge>),
) -> Vec<Gauge> {
    if published.as_ref().is_none_or(|(at, _, _)| now.saturating_duration_since(*at) >= std::time::Duration::from_secs(60)) {
        let (en, es) = compute();
        match published.as_mut() {
            Some((at, old_en, old_es)) => {
                *at = now;
                if !same_visible_gauges(old_en, &en) || !same_visible_gauges(old_es, &es) {
                    *old_en = en;
                    *old_es = es;
                }
            }
            None => *published = Some((now, en, es)),
        }
    }
    published.as_ref().map(|(_, en, es)| if lang == Lang::En { en.clone() } else { es.clone() }).unwrap_or_default()
}

fn digest_hex(bytes: &[u8]) -> String {
    sha2::Sha256::digest(bytes).iter().map(|byte| format!("{byte:02x}")).collect()
}

#[cfg(test)]
mod cache_tests {
    use super::*;
    use alondra_core::domain::{Actor, Command, CommandEnvelope};
    use std::{fs, sync::Arc};

    fn execute(domain: &crate::domain_runtime::DomainRuntime, actor: Actor, id: &str, command: Command) {
        let state = domain.state().unwrap();
        domain.execute(&actor, &CommandEnvelope {
            protocol_version: alondra_core::domain::PROTOCOL_VERSION, data_epoch: state.data_epoch.clone(), command_id: id.into(),
            expected_revision: state.revision, command,
        }, None, || Ok(())).unwrap();
    }

    fn report(domain: &crate::domain_runtime::DomainRuntime, value: Value, at: Millis) {
        domain.report(serde_json::from_value(value).unwrap(), at);
    }


    fn wait_fixture() -> (crate::test_support::Directory, Arc<crate::domain_runtime::DomainRuntime>, Arc<crate::client_routes::Client>) {
        let home = crate::test_support::Directory::new();
        fs::create_dir_all(&home).unwrap();
        let domain = Arc::new(crate::domain_runtime::DomainRuntime::open(&home).unwrap());
        let client = crate::client_routes::Client::open(domain.clone(), &home, Arc::new(Mutex::new(None)));
        execute(&domain, Actor::Orchestrator { generation: 0 }, "request", Command::CreateRequest {
            request_id: "request".into(), body: "Work".into(), auto_merge: false, maintenance: false, message: false,
        });
        execute(&domain, Actor::Orchestrator { generation: 0 }, "title", Command::TitleRequest { request_id: "request".into(), title: "Work".into() });
        for id in ["room", "launch", "relaunch", "send", "closed", "retry"] {
            execute(&domain, Actor::Orchestrator { generation: 0 }, id, Command::CreateTask {
                task_id: id.into(), request_id: "request".into(), title: id.into(), agent_id: None,
                review_prs: true, dependencies: Default::default(), stack: None,
                launch: Some(alondra_core::domain::LaunchPlan::new("node", home.to_str().unwrap())),
            });
        }
        (home, domain, client)
    }

    #[tokio::test]
    async fn live_store_reads_refresh_launch_retry_and_close_side_state_with_the_same_domain_snapshot() {
        use alondra_core::domain::ModelChooser;
        use alondra_core::views::LiveStartWait;
        use crate::domain_store::model_metrics::{Decision, DecisionKind};
        let (_home, domain, client) = wait_fixture();
        let state = domain.state().unwrap();
        let now = 1_700_000_000_000;
        let initial = gather_at(&client, &state, Lang::En, &[], false, false, now).await;
        assert_eq!(initial.start_waits.keys().map(String::as_str).collect::<Vec<_>>(), ["closed", "launch", "relaunch", "retry", "room", "send"]);
        for (id, kind) in [("launch", DecisionKind::Launch), ("relaunch", DecisionKind::Relaunch), ("send", DecisionKind::Send)] {
            domain.record_model_decision(&Decision { task_id: id.into(), request_id: "request".into(), agent_id: None,
                at: now, kind, model: None, effort: None, approach: None, chosen_by: ModelChooser::Orchestrator,
                reason: None, title: id.into() }).unwrap();
        }
        domain.live.remember_closed_task("closed", crate::client_routes::ClosedNode { name: "Closed node".into(), cwd: None });
        domain.store.lock().unwrap().set_start_retry(&"retry".into(), Some((2, now + 60_000))).unwrap();
        domain.store.lock().unwrap().begin_live_read_trace();
        let changed = gather_at(&client, &state, Lang::En, &[], false, false, now).await;
        let trace = domain.store.lock().unwrap().end_live_read_trace();
        assert_eq!(domain.state().unwrap().revision, state.revision);
        assert_eq!(changed.start_waits.keys().map(String::as_str).collect::<Vec<_>>(), ["retry", "room", "send"]);
        assert_eq!(changed.start_waits["room"], LiveStartWait::Room);
        assert_eq!(changed.start_waits["send"], LiveStartWait::Room);
        assert_eq!(changed.start_waits["retry"], LiveStartWait::Retry { at: crate::usage_pause::clock_text(now + 60_000, now, false) });
        assert!(trace.iter().all(|read| !read.failed));
        assert_eq!(trace.iter().filter(|read| read.getter == "task_was_launched").count(), 5, "live-closed task skips the SQL probe");
        let later = gather_at(&client, &state, Lang::Es, &[], false, false, now + 2 * 86_400_000).await;
        assert_eq!(later.start_waits["retry"], LiveStartWait::Retry { at: crate::usage_pause::clock_text(now + 60_000, now + 2 * 86_400_000, true) });
        assert_ne!(changed.start_waits["retry"], later.start_waits["retry"], "moving clock/language remain live inputs");
        domain.live.forget_closed_task("closed");
        domain.store.lock().unwrap().set_start_retry(&"retry".into(), None).unwrap();
        let restored = gather_at(&client, &state, Lang::En, &[], false, false, now).await;
        assert_eq!(restored.start_waits["closed"], LiveStartWait::Room);
        assert_eq!(restored.start_waits["retry"], LiveStartWait::Room);
    }

    #[tokio::test]
    async fn unreadable_launch_markers_fail_closed_and_trace_the_failed_reads() {
        let (home, domain, client) = wait_fixture();
        let state = domain.state().unwrap();
        let connection = rusqlite::Connection::open(crate::domain_database_migration::active_path(&home)).unwrap();
        connection.execute_batch("DROP TABLE model_decisions").unwrap();
        domain.store.lock().unwrap().begin_live_read_trace();
        let live = gather_at(&client, &state, Lang::En, &[], false, false, 1_700_000_000_000).await;
        let trace = domain.store.lock().unwrap().end_live_read_trace();
        assert!(live.start_waits.is_empty(), "unknown launch history must not advertise a fresh launch slot");
        let failed: Vec<_> = trace.iter().filter(|read| read.failed).collect();
        assert_eq!(failed.len(), 6);
        assert!(failed.iter().all(|read| read.getter == "task_was_launched" && read.statement_attempts == 1 && read.rows == 0));
    }

    #[tokio::test]
    async fn unobserved_nodes_do_not_fabricate_transition_times_or_miss_the_cache() {
        use alondra_core::views::{Client, Transport};
        let home = crate::test_support::Directory::new();
        fs::create_dir_all(&home).unwrap();
        let domain = Arc::new(crate::domain_runtime::DomainRuntime::open(&home).unwrap());
        let client = crate::client_routes::Client::open(domain.clone(), &home, Arc::new(Mutex::new(None)));
        for id in ["connected", "reported", "assigned", "hidden"] {
            execute(&domain, Actor::System, id, Command::RegisterAgent { agent_id: id.into(), name: id.into(), at: 0 });
        }
        execute(&domain, Actor::System, "observe", Command::MoveNode { agent_id: "connected".into(), event: alondra_core::domain::NodeEvent::TurnStarted, at: 0 });
        execute(&domain, Actor::System, "hide", Command::MoveNode { agent_id: "hidden".into(), event: alondra_core::domain::NodeEvent::SessionClosed, at: 0 });
        execute(&domain, Actor::Orchestrator { generation: 0 }, "request", Command::CreateRequest {
            request_id: "request".into(), body: "Work".into(), auto_merge: false, maintenance: false, message: false,
        });
        execute(&domain, Actor::Orchestrator { generation: 0 }, "task", Command::CreateTask {
            task_id: "task".into(), request_id: "request".into(), title: "Work".into(), agent_id: Some("assigned".into()),
            review_prs: true, dependencies: Default::default(), stack: None,
            launch: Some(alondra_core::domain::LaunchPlan::new("node", home.to_str().unwrap())),
        });
        report(&domain, serde_json::json!({"session_id": "reported", "event": "session.start", "version": "test"}), 1_000);
        let state = domain.state().unwrap();
        let first = gather_at(&client, &state, Lang::En, &[], false, false, 2_000).await;
        let later = gather_at(&client, &state, Lang::En, &[], false, false, 2_000 + alondra_core::states::NODE_QUIET_MS).await;
        let first_view = client.views.render("fleet", &state, &first, Client::Mac, Lang::En, Transport::Loopback).unwrap();
        let later_view = client.views.render("fleet", &state, &later, Client::Mac, Lang::En, Transport::Loopback).unwrap();
        assert_eq!(client.views.build_count(), 1, "reading an unobserved node again must reuse the view");
        assert_eq!(first.nodes.iter().map(|node| node.agent_id.as_str()).collect::<Vec<_>>(), ["assigned", "connected", "reported"]);
        for node in &first.nodes {
            assert_eq!(node.state_since, None, "a session or task is not an observed node transition");
            assert!(!node.quiet, "the time of the last node transition is unknown");
        }
        assert_eq!(serde_json::to_value(&later).unwrap(), serde_json::to_value(&first).unwrap());
        assert_eq!(later_view, first_view);
    }

    #[tokio::test]
    async fn observed_nodes_keep_transition_times_and_quiet_boundaries() {
        use alondra_core::views::{Client, Transport};
        let home = crate::test_support::Directory::new();
        fs::create_dir_all(&home).unwrap();
        let domain = Arc::new(crate::domain_runtime::DomainRuntime::open(&home).unwrap());
        let client = crate::client_routes::Client::open(domain.clone(), &home, Arc::new(Mutex::new(None)));
        execute(&domain, Actor::System, "agent", Command::RegisterAgent { agent_id: "known".into(), name: "Known".into(), at: 0 });
        domain.live.observe(&serde_json::json!({"session_id": "known", "hook_event_name": "Stop", "last_assistant_message": "Ready"}));
        execute(&domain, Actor::System, "stop", Command::MoveNode { agent_id: "known".into(), event: alondra_core::domain::NodeEvent::TurnEnded, at: 0 });
        let since = domain.live.node("known").unwrap().state_since;
        let state = domain.state().unwrap();
        let before = gather_at(&client, &state, Lang::En, &[], false, false, since + alondra_core::states::NODE_QUIET_MS - 1).await;
        let quiet = gather_at(&client, &state, Lang::En, &[], false, false, since + alondra_core::states::NODE_QUIET_MS).await;
        assert_eq!(before.nodes[0].state_since, Some(timestamp(since)));
        assert_eq!(quiet.nodes[0].state_since, before.nodes[0].state_since);
        assert!(!before.nodes[0].quiet);
        assert!(quiet.nodes[0].quiet);
        let before_view = client.views.render("fleet", &state, &before, Client::Mac, Lang::En, Transport::Loopback).unwrap();
        let quiet_view = client.views.render("fleet", &state, &quiet, Client::Mac, Lang::En, Transport::Loopback).unwrap();
        assert_ne!(quiet_view.cursor, before_view.cursor);
        assert_eq!(client.views.build_count(), 2);
        report(&domain, serde_json::json!({"session_id": "known", "event": "turn.complete", "answer": "New answer"}), since + 1_000);
        let recent_answer = gather_at(&client, &state, Lang::En, &[], false, false, since + alondra_core::states::NODE_QUIET_MS).await;
        assert!(!recent_answer.nodes[0].quiet, "a recent answer postpones quiet");
        assert_eq!(recent_answer.nodes[0].last_message.as_deref(), Some("New answer"));
        assert_eq!(recent_answer.nodes[0].state_since, Some(timestamp(since)));
    }

    #[tokio::test]
    async fn report_updates_and_elapsed_tool_limits_invalidate_unobserved_nodes() {
        use alondra_core::views::{Client, Transport};
        let home = crate::test_support::Directory::new();
        fs::create_dir_all(&home).unwrap();
        let domain = Arc::new(crate::domain_runtime::DomainRuntime::open(&home).unwrap());
        let client = crate::client_routes::Client::open(domain.clone(), &home, Arc::new(Mutex::new(None)));
        execute(&domain, Actor::System, "agent", Command::RegisterAgent { agent_id: "reported".into(), name: "Reported".into(), at: 0 });
        report(&domain, serde_json::json!({"session_id": "reported", "event": "session.start", "version": "test"}), 1_000);
        report(&domain, serde_json::json!({"session_id": "reported", "event": "tool.start", "tool": "Bash", "input": {"command": "build"}}), 2_000);
        let state = domain.state().unwrap();
        let limit = 2_000 + alondra_core::states::call_limit("Bash");
        let working = gather_at(&client, &state, Lang::En, &[], false, false, limit).await;
        let stalled = gather_at(&client, &state, Lang::En, &[], false, false, limit + 1).await;
        assert_eq!(working.nodes[0].state_key.as_deref(), Some("busy"));
        assert_eq!(stalled.nodes[0].state_key.as_deref(), Some("stalled"));
        let working_view = client.views.render("fleet", &state, &working, Client::Mac, Lang::En, Transport::Loopback).unwrap();
        let stalled_view = client.views.render("fleet", &state, &stalled, Client::Mac, Lang::En, Transport::Loopback).unwrap();
        assert_ne!(stalled_view.cursor, working_view.cursor);
        report(&domain, serde_json::json!({"session_id": "reported", "event": "turn.complete", "answer": "Done", "aborted": true}), limit + 2);
        let answer = gather_at(&client, &state, Lang::En, &[], false, false, limit + 2).await;
        assert_eq!(answer.nodes[0].state_key.as_deref(), Some("free"));
        assert_eq!(answer.nodes[0].last_message.as_deref(), Some("Done"));
        assert_eq!(answer.nodes[0].last_message_at, Some(timestamp(limit + 2)));
        assert!(answer.nodes[0].last_turn_aborted);
        assert!(!answer.nodes[0].quiet);
        assert_eq!(answer.nodes[0].state_since, None);
        let answer_view = client.views.render("fleet", &state, &answer, Client::Mac, Lang::En, Transport::Loopback).unwrap();
        assert_ne!(answer_view.cursor, stalled_view.cursor);
        assert_eq!(client.views.build_count(), 3);
    }

    /// The rows of a view whose badge says `state`, by id, wherever they are nested.
    fn rows_with(value: &Value, state: &str, found: &mut Vec<String>) {
        match value {
            Value::Object(row) => {
                let badged = row.get("badges").and_then(Value::as_array)
                    .is_some_and(|badges| badges.iter().any(|badge| badge["text"] == state));
                if badged && let Some(id) = row.get("id").and_then(Value::as_str) {
                    found.push(id.to_owned());
                }
                row.values().for_each(|child| rows_with(child, state, found));
            }
            Value::Array(items) => items.iter().for_each(|child| rows_with(child, state, found)),
            _ => {}
        }
    }

    #[tokio::test]
    async fn the_gate_fleet_and_tasks_list_count_the_same_nodes_working() {
        use alondra_core::views::{Client, Transport};
        use crate::node_reports::{Envelope, Report};
        let home = crate::test_support::Directory::new();
        fs::create_dir_all(&home).unwrap();
        let domain = Arc::new(crate::domain_runtime::DomainRuntime::open(&home).unwrap());
        domain.live.gate.tune(crate::fleet_gate::Tuning { spacing: 0, limit: Some(crate::fleet_gate::Limit::Fixed(2)) });
        let client = crate::client_routes::Client::open(domain.clone(), &home, Arc::new(Mutex::new(None)));
        execute(&domain, Actor::User { user_id: "u".into() }, "request", Command::CreateRequest {
            request_id: "request".into(), body: "Work".into(), auto_merge: false, maintenance: false, message: false,
        });
        execute(&domain, Actor::User { user_id: "u".into() }, "title", Command::TitleRequest { request_id: "request".into(), title: "Work".into() });
        let now = alondra_core::now_millis();
        for id in ["build", "docs", "harness"] {
            let agent = format!("agent-{id}");
            execute(&domain, Actor::Orchestrator { generation: 0 }, &format!("create-{id}"), Command::CreateTask {
                task_id: id.into(), request_id: "request".into(), title: format!("Task {id}"), agent_id: None, review_prs: false,
                dependencies: Default::default(), stack: None, launch: Some(alondra_core::domain::LaunchPlan::new("node", "/tmp")),
            });
            execute(&domain, Actor::System, &format!("register-{id}"), Command::RegisterAgent { agent_id: agent.as_str().into(), name: agent.clone(), at: 0 });
            execute(&domain, Actor::System, &format!("observe-{id}"), Command::MoveNode { agent_id: agent.as_str().into(), event: alondra_core::domain::NodeEvent::TurnStarted, at: 0 });
            execute(&domain, Actor::User { user_id: "u".into() }, &format!("assign-{id}"), Command::AssignTask {
                task_id: id.into(), agent_id: agent.as_str().into(), model: None,
            });
            execute(&domain, Actor::Agent { agent_id: agent.as_str().into() }, &format!("start-{id}"), Command::StartTask { task_id: id.into() });
            domain.report(Envelope { session_id: agent, report: Report::TurnStart }, now);
        }
        assert!(domain.live.gate.hold_back(&domain, "agent-harness", crate::fleet_pause::Reason::Declared {
            reason: "The build ran out of memory".into(),
        }, now));

        let state = domain.state().unwrap();
        let gate = domain.live.gate.snapshot(&domain, now).unwrap();
        let live = gather_at(&client, &state, Lang::En, &[], false, false, now).await;
        let tasks = client.views.render("tasks", &state, &live, Client::Mac, Lang::En, Transport::Loopback).unwrap();
        let mut listed = vec![];
        rows_with(&serde_json::to_value(&tasks).unwrap(), "Working", &mut listed);
        listed.sort();
        let fleet: Vec<&str> = live.nodes.iter().filter(|node| node.working).map(|node| node.agent_id.as_str()).collect();

        assert_eq!(gate.working_tasks.iter().map(|task| task.0.as_str()).collect::<Vec<_>>(), ["build", "docs"]);
        assert_eq!(live.pacing.as_ref().map(|pacing| pacing.working), Some(2));
        assert_eq!(listed, ["build", "docs"], "the Tasks list says Working of the same tasks");
        assert_eq!(fleet, ["agent-build", "agent-docs"], "Fleet does not show the node waiting for the machine as working");
    }

    #[test]
    fn published_gauges_debounce_for_a_minute_and_ignore_invisible_values() {
        let now = std::time::Instant::now();
        let mut published = None;
        let gauge = |text: &str, value: f64| Gauge {
            id: "usage".into(), label: "Usage".into(), value, text: text.into(),
            detail: None, level: Level::Calm,
        };
        assert_eq!(published_gauges(&mut published, now, Lang::En, || (vec![gauge("50%", 0.5)], vec![]))[0].text, "50%");
        let mut computed = false;
        assert_eq!(published_gauges(&mut published, now + std::time::Duration::from_secs(59), Lang::En, || {
            computed = true;
            (vec![gauge("60%", 0.6)], vec![])
        })[0].text, "50%");
        assert!(!computed, "a fresh sample must not be recomputed before the minute boundary");
        assert_eq!(published_gauges(&mut published, now + std::time::Duration::from_secs(60), Lang::En, || {
            (vec![gauge("50%", 0.501)], vec![])
        })[0].value, 0.5, "a hidden fraction change must not change the published gauge");
        assert_eq!(published_gauges(&mut published, now + std::time::Duration::from_secs(120), Lang::En, || {
            (vec![gauge("60%", 0.6)], vec![])
        })[0].text, "60%");
    }

    #[tokio::test]
    async fn inbox_times_follow_server_order_and_committed_revision_times_in_both_languages() {
        use alondra_core::domain::{Recipient, Scope};
        let home = crate::test_support::Directory::new();
        fs::create_dir_all(&home).unwrap();
        let domain = Arc::new(crate::domain_runtime::DomainRuntime::open(&home).unwrap());
        let client = crate::client_routes::Client::open(domain.clone(), &home, Arc::new(std::sync::Mutex::new(None)));
        let mut orders = vec![];
        for id in ["z-first", "a-second"] {
            let state = domain.state().unwrap();
            domain.execute(&Actor::Orchestrator { generation: 0 }, &CommandEnvelope {
                protocol_version: 2, data_epoch: state.data_epoch.clone(),
                command_id: format!("notice-{id}").into(), expected_revision: state.revision,
                command: Command::PublishInformation {
                    attachments: vec![], interaction_id: id.into(),
                    scope: Scope::Orchestrator { request_id: None }, recipient: Recipient::User,
                    body: id.into(), card: alondra_core::domain::CardSpec::new(alondra_core::domain::CardKind::Know, "Read the update"),
                },
            }, None, || Ok(())).unwrap();
            orders.push(domain.state().unwrap().revision);
        }
        let state = domain.state().unwrap();
        let before = serde_json::to_vec(&*state).unwrap();
        let times = domain.store.lock().unwrap().revision_times(&orders).unwrap();
        assert_eq!(times.len(), 2, "both source commits must exist");
        for lang in [Lang::En, Lang::Es] {
            let snapshot = gather(&client, &state, lang, &[], false, false).await;
            assert_eq!(snapshot.inbox_times.iter().map(|i| (i.id.as_str(), i.at.clone())).collect::<Vec<_>>(), vec![
                ("interaction:z-first", timestamp(times[&orders[0]])),
                ("interaction:a-second", timestamp(times[&orders[1]])),
            ]);
            assert!(gather(&client, &state, lang, &[], false, true).await.inbox_times.is_empty());
        }
        assert_eq!(serde_json::to_vec(&*domain.state().unwrap()).unwrap(), before);
    }

    #[tokio::test]
    async fn activity_and_search_reads_are_reused_until_their_sources_change() {
        let scratch_directory = crate::test_support::Directory::new();
        let home = scratch_directory.parent().unwrap().join(format!("alondra-live-cache-{}", crate::auth::random_hex(8)));
        fs::create_dir(&home).unwrap();
        let domain = Arc::new(crate::domain_runtime::DomainRuntime::open(&home).unwrap());
        let client = crate::client_routes::Client::open(domain.clone(), &home, Arc::new(std::sync::Mutex::new(None)));
        for id in ["a", "b"] {
            let state = domain.state().unwrap();
            domain.execute(&Actor::System, &CommandEnvelope {
                protocol_version: 2, data_epoch: state.data_epoch.clone(),
                command_id: format!("register-{id}").into(), expected_revision: state.revision,
                command: Command::RegisterAgent { agent_id: id.into(), name: format!("Agent {id}"), at: 0 },
            }, None, || Ok(())).unwrap();
        }
        let activity_hook = |id: &str, message: &str| serde_json::json!({
            "session_id": id, "hook_event_name": "PreToolUse", "tool_name": "Bash",
            "tool_use_id": format!("tool-{message}"), "tool_input": {"command": message},
        });
        let message_hook = |id: &str, message: &str| serde_json::json!({
            "session_id": id, "hook_event_name": "Stop", "last_assistant_message": message,
        });
        domain.live.observe(&activity_hook("a", "Parser ready"));
        domain.live.observe(&message_hook("b", "Other work"));
        let state = domain.state().unwrap();
        let activity = activity_source_revision(&client, "a");
        let search = search_source_revision(&client, &state);
        let cache = &client.live_reads;
        let activity_path = "/agents/a/activity";
        let search_path = "/agents/search?q=parser";
        cache.insert(activity_path.into(), state.revision, &activity, serde_json::json!({"activity": 1}));
        cache.insert(search_path.into(), state.revision, &search, serde_json::json!({"search": 1}));
        assert!(cache.get(activity_path, state.revision, &activity).is_some());
        assert!(cache.get(search_path, state.revision, &search).is_some());

        domain.live.observe(&message_hook("b", "Other work changed"));
        assert_eq!(activity_source_revision(&client, "a"), activity);
        assert!(cache.get(activity_path, state.revision, &activity_source_revision(&client, "a")).is_some());
        assert_ne!(search_source_revision(&client, &state), search);
        assert!(cache.get(search_path, state.revision, &search_source_revision(&client, &state)).is_none());

        domain.live.observe(&activity_hook("a", "Parser changed"));
        assert_ne!(activity_source_revision(&client, "a"), activity);
        assert!(cache.get(activity_path, state.revision, &activity_source_revision(&client, "a")).is_none());
        drop(client);
        drop(domain);
        fs::remove_dir_all(home).unwrap();
    }
}

/// RAM and CPU move a point or two every sample. Ten points is about two of the pill's 22 pt bar,
/// the smallest move a glance notices; below it, or without a new level, the published set stays.
const LOAD_STEP: f64 = 0.10;
/// Free disk drifts slowly with builds and caches; two points is tens of GB on any Mac disk.
const DISK_STEP: f64 = 0.02;

fn machine_step(id: &str) -> Option<f64> {
    match id {
        "memory" | "cpu" => Some(LOAD_STEP),
        "disk" => Some(DISK_STEP),
        _ => None,
    }
}

/// Whether publishing `right` over `left` would show nothing meaningful: a Claude reading counts
/// whenever its text, detail or level moves; a machine gauge only past its step or on a new level.
fn same_visible_gauges(left: &[Gauge], right: &[Gauge]) -> bool {
    left.len() == right.len()
        && left.iter().zip(right).all(|(a, b)| {
            let same_reading = match machine_step(&a.id) {
                Some(step) => (a.value - b.value).abs() < step,
                None => a.text == b.text && a.detail == b.detail,
            };
            a.id == b.id && same_reading && a.level == b.level
        })
}

/// A reading older than this is shown with its age: turns refresh it every few minutes while a
/// node works, so an hour without one means no node has run, and the 5-hour window can have moved
/// by tens of points.
const STALE_READING_MS: Millis = 60 * 60 * 1000;

fn reading_age(measured_at: Millis, now: Millis, lang: Lang) -> Option<String> {
    let age = now - measured_at;
    if age <= STALE_READING_MS {
        return None;
    }
    let hours = (age as f64 / STALE_READING_MS as f64).round() as i64;
    let (amount, unit) = if hours >= 48 { (hours / 24, "d") } else { (hours, "h") };
    Some(match lang {
        Lang::En => format!("measured {amount} {unit} ago"),
        Lang::Es => format!("medido hace {amount} {unit}"),
    })
}

fn gauges(
    machine: &alondra_core::Machine,
    usage: Option<&alondra_core::usage::Usage>,
    lang: Lang,
    now: Millis,
) -> Vec<Gauge> {
    let mut out = Vec::new();
    if let Some(usage) = usage {
        let week_reset = reset_time::reset_day_and_time(usage.week_resets_at, now, reset_time::local_offset_secs, lang);
        let aged = |detail: String| Some(match reading_age(usage.measured_at, now, lang) {
            Some(age) => format!("{detail} · {age}"),
            None => detail,
        });
        out.push(Gauge {
            id: "claude_week".into(),
            label: match lang {
                Lang::En => "Claude week",
                Lang::Es => "Claude semana",
            }
            .into(),
            value: usage.week_percent / 100.0,
            text: format!("{}%", usage.week_percent.round()),
            detail: aged(match lang {
                Lang::En => format!("Weekly limit resets {week_reset}"),
                Lang::Es => format!("El límite semanal se reinicia {week_reset}"),
            }),
            level: if usage.week_percent >= 85.0 {
                Level::Hot
            } else {
                Level::Calm
            },
        });
        if let Some((spent, share)) = usage.today_spent.zip(usage.today_share) {
            let level = if usage.over_pace {
                Level::Hot
            } else if spent >= share * 0.75 {
                Level::Warm
            } else {
                Level::Calm
            };
            out.push(Gauge {
                id: "claude_today".into(),
                label: match lang {
                    Lang::En if usage.over_pace => "Claude today !",
                    Lang::Es if usage.over_pace => "Claude hoy !",
                    Lang::En => "Claude today",
                    Lang::Es => "Claude hoy",
                }
                .into(),
                value: spent / share.max(0.1),
                text: format!("{}/{}%", spent.round(), share.round()),
                detail: aged(if usage.over_pace {
                    match lang {
                        Lang::En => "Spending faster than today's share; confirm new launches".into(),
                        Lang::Es => "Ritmo superior a la cuota diaria; confirma nuevos lanzamientos".into(),
                    }
                } else {
                    match lang {
                        Lang::En => format!("{}% spent of today's {}% share", spent.round(), share.round()),
                        Lang::Es => format!("Gastado {}% de la cuota diaria del {}%", spent.round(), share.round()),
                    }
                }),
                level,
            });
        }
        if let Some(percent) = usage.five_hour_percent {
            let five_hour_reset = |at| reset_time::five_hour_reset(at, now, reset_time::local_offset_secs, lang);
            out.push(Gauge {
                id: "claude_five_hours".into(),
                label: match lang {
                    Lang::En => "Claude 5 h",
                    Lang::Es => "Claude 5 h",
                }
                .into(),
                value: percent / 100.0,
                text: format!("{}%", percent.round()),
                detail: usage.five_hour_resets_at.and_then(|at| aged(match lang {
                    Lang::En => format!("5-hour limit resets {}", five_hour_reset(at)),
                    Lang::Es => format!("El límite de 5 horas se reinicia {}", five_hour_reset(at)),
                })),
                level: if percent >= 85.0 { Level::Hot } else { Level::Calm },
            });
        }
    }
    if let Some(free) = machine.memory_free_percent {
        let used = 100 - free.min(100);
        out.push(Gauge {
            id: "memory".into(),
            label: "RAM".into(),
            value: used as f64 / 100.0,
            text: format!("{used}%"),
            detail: machine.memory_pressure.filter(|pressure| *pressure != alondra_core::MemoryPressure::Normal).map(|_| match lang {
                Lang::En => "macOS reports memory pressure".into(),
                Lang::Es => "macOS avisa de presión de memoria".into(),
            }),
            level: if free < 15 || machine.memory_pressure == Some(alondra_core::MemoryPressure::Critical) {
                Level::Hot
            } else if free < 30 || machine.memory_pressure == Some(alondra_core::MemoryPressure::Warning) {
                Level::Warm
            } else {
                Level::Calm
            },
        });
    }
    let cpu = f64::from(machine.cpu_usage);
    out.push(Gauge {
        id: "cpu".into(),
        label: "CPU".into(),
        value: cpu / 100.0,
        text: format!("{}%", cpu.round()),
        detail: None,
        level: if machine.unreliable {
            Level::Hot
        } else if cpu >= 90.0 {
            Level::Hot
        } else if cpu >= 75.0 {
            Level::Warm
        } else {
            Level::Calm
        },
    });
    if machine.disk_total > 0 {
        let used = machine.disk_total.saturating_sub(machine.disk_available);
        let ratio = used as f64 / machine.disk_total as f64;
        let free = machine.disk_available as f64 / 1_000_000_000.0;
        out.push(Gauge {
            id: "disk".into(),
            label: match lang {
                Lang::En => "Disk",
                Lang::Es => "Disco",
            }
            .into(),
            value: ratio,
            text: match lang {
                Lang::En => format!("{free:.0} GB free"),
                Lang::Es => format!("{free:.0} GB libres"),
            },
            detail: None,
            level: if ratio >= 0.95 {
                Level::Hot
            } else if ratio >= 0.85 {
                Level::Warm
            } else {
                Level::Calm
            },
        });
    }
    out
}

fn timestamp(millis: i64) -> String {
    crate::log::local_iso(
        millis.div_euclid(1000) as libc::time_t,
        millis.rem_euclid(1000) as u32,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pr(id: &str, number: u64, head: &str, base: &str) -> PullRequest {
        PullRequest {
            id: alondra_core::domain::PrId(id.into()),
            task_id: TaskId("task".into()),
            host: "github.com".into(),
            repository: "bitomule/alondra".into(),
            number,
            head: head.into(),
            base: base.into(),
            state: PrState::Open,
            merged_head: None,
            allowed_methods: vec![],
            review: None,
        }
    }

    #[test]
    fn stacked_pr_uses_the_open_parent_and_ignores_cycles() {
        let mut state = DomainState::new("epoch");
        let below = pr("github.com/bitomule/alondra#154", 154, "base-branch", "main");
        let above = pr("github.com/bitomule/alondra#155", 155, "feature-branch", "base-branch");
        state.pull_requests.insert(below.id.clone(), below.clone());
        state.pull_requests.insert(above.id.clone(), above.clone());
        assert_eq!(stacked_below(&above, &state).map(|pr| pr.number), Some(154));
        let mut closed = below.clone();
        closed.state = PrState::Merged;
        state.pull_requests.insert(closed.id.clone(), closed);
        assert!(stacked_below(&above, &state).is_none());
        let mut foreign = pr("enterprise.example/bitomule/alondra#154", 154,
            "base-branch", "main");
        foreign.host = "enterprise.example".into();
        state.pull_requests.insert(foreign.id.clone(), foreign);
        assert!(stacked_below(&above, &state).is_none());
        let mut cycle = below;
        cycle.base = "feature-branch".into();
        state.pull_requests.insert(cycle.id.clone(), cycle);
        assert!(stacked_below(&above, &state).is_none());
    }

    #[test]
    fn ram_wakes_clients_only_past_its_step_or_on_a_new_level() {
        let memory = |value: f64, level: Level| Gauge { id: "memory".into(), label: "RAM".into(), value,
            text: format!("{}%", (value * 100.0).round()), detail: None, level };
        assert!(same_visible_gauges(&[memory(0.50, Level::Calm)], &[memory(0.51, Level::Calm)]));
        assert!(same_visible_gauges(&[memory(0.50, Level::Calm)], &[memory(0.59, Level::Calm)]));
        assert!(!same_visible_gauges(&[memory(0.50, Level::Calm)], &[memory(0.61, Level::Calm)]));
        assert!(!same_visible_gauges(&[memory(0.69, Level::Calm)], &[memory(0.71, Level::Warm)]));
    }

    #[test]
    fn disk_wakes_clients_only_past_its_step_or_on_a_new_level() {
        let disk = |value: f64, text: &str, level: Level| Gauge { id: "disk".into(), label: "Disk".into(), value,
            text: text.into(), detail: None, level };
        assert!(same_visible_gauges(&[disk(0.880, "58 GB free", Level::Warm)], &[disk(0.881, "57 GB free", Level::Warm)]));
        assert!(!same_visible_gauges(&[disk(0.880, "58 GB free", Level::Warm)], &[disk(0.901, "48 GB free", Level::Warm)]));
        assert!(!same_visible_gauges(&[disk(0.849, "70 GB free", Level::Calm)], &[disk(0.851, "69 GB free", Level::Warm)]));
    }

    #[test]
    fn every_claude_reading_change_wakes_clients() {
        let week = |text: &str, detail: &str| Gauge { id: "claude_week".into(), label: "Claude week".into(),
            value: 0.83, text: text.into(), detail: Some(detail.into()), level: Level::Calm };
        assert!(same_visible_gauges(&[week("83%", "resets Monday")], &[week("83%", "resets Monday")]));
        assert!(!same_visible_gauges(&[week("83%", "resets Monday")], &[week("84%", "resets Monday")]));
        assert!(!same_visible_gauges(&[week("83%", "resets Monday")], &[week("83%", "resets Monday · measured 2 h ago")]));
    }

    #[test]
    fn a_minute_of_ram_and_cpu_wiggle_keeps_the_published_set() {
        let machine = |memory_free: u32, cpu: f32| alondra_core::Machine { at: 0, memory_total: 100,
            memory_available: 50, memory_free_percent: Some(memory_free), memory_pressure: None, compressor: None, load: [0.0; 3], cpus: 8,
            cpu_usage: cpu, disk_total: 1_000, disk_available: 120, unreliable: false };
        let usage = |week: f64| alondra_core::usage::Usage { week_percent: week, week_resets_at: 0,
            today_spent: None, today_share: None, per_day_left: 0.0, over_pace: false, five_hour_percent: Some(10.0),
            five_hour_resets_at: Some(0), measured_at: 0 };
        let sample = |m: &alondra_core::Machine, u: &alondra_core::usage::Usage| {
            (gauges(m, Some(u), Lang::En, 0), gauges(m, Some(u), Lang::Es, 0))
        };
        let start = std::time::Instant::now();
        let minute = |n: u64| start + std::time::Duration::from_secs(60 * n);
        let mut published = None;
        let first = published_gauges(&mut published, start, Lang::En, || sample(&machine(61, 21.0), &usage(83.0)));
        let wiggled = published_gauges(&mut published, minute(1), Lang::En, || sample(&machine(58, 26.0), &usage(83.0)));
        assert_eq!(wiggled, first, "RAM 39→42 % and CPU 21→26 % are not worth waking every phone");
        let reading = published_gauges(&mut published, minute(2), Lang::En, || sample(&machine(58, 26.0), &usage(84.0)));
        assert_eq!(reading.iter().find(|gauge| gauge.id == "claude_week").unwrap().text, "84%");
        assert_eq!(reading.iter().find(|gauge| gauge.id == "memory").unwrap().text, "42%",
            "a published set carries the current machine readings along with the new Claude one");
    }

    #[test]
    fn machine_gauges_measure_cpu_after_memory_like_the_header_did() {
        let mut machine = alondra_core::Machine { at: 0, memory_total: 100, memory_available: 50,
            memory_free_percent: Some(50), memory_pressure: None, compressor: None, load: [0.0; 3], cpus: 8, cpu_usage: 34.4,
            disk_total: 100, disk_available: 50, unreliable: false };
        let english = gauges(&machine, None, Lang::En, 0);
        assert_eq!(english.iter().map(|gauge| gauge.id.as_str()).collect::<Vec<_>>(), ["memory", "cpu", "disk"]);
        let cpu = &english[1];
        assert_eq!((cpu.label.as_str(), cpu.text.as_str(), cpu.level), ("CPU", "34%", Level::Calm));
        let cpu_level = |machine: &alondra_core::Machine| gauges(machine, None, Lang::En, 0)[1].level;
        machine.cpu_usage = 74.0;
        assert_eq!(cpu_level(&machine), Level::Calm);
        machine.cpu_usage = 75.0;
        assert_eq!(cpu_level(&machine), Level::Warm);
        machine.cpu_usage = 89.0;
        assert_eq!(cpu_level(&machine), Level::Warm);
        machine.cpu_usage = 90.0;
        assert_eq!(cpu_level(&machine), Level::Hot);
        machine.cpu_usage = 10.0;
        machine.unreliable = true;
        assert_eq!(gauges(&machine, None, Lang::En, 0)[1].level, Level::Hot);
    }

    #[test]
    fn cpu_wakes_clients_only_on_a_meaningful_change() {
        let cpu = |value: f64, level: Level| Gauge { id: "cpu".into(), label: "CPU".into(), value,
            text: format!("{}%", (value * 100.0).round()), detail: None, level };
        assert!(same_visible_gauges(&[cpu(0.34, Level::Calm)], &[cpu(0.41, Level::Calm)]));
        assert!(!same_visible_gauges(&[cpu(0.34, Level::Calm)], &[cpu(0.45, Level::Calm)]));
        assert!(!same_visible_gauges(&[cpu(0.84, Level::Calm)], &[cpu(0.86, Level::Warm)]));
    }

    #[test]
    fn usage_gauges_keep_reset_daily_share_and_over_pace_warning() {
        let machine = alondra_core::Machine { at: 0, memory_total: 100, memory_available: 50,
            memory_free_percent: Some(50), memory_pressure: None, compressor: None, load: [0.0; 3], cpus: 8, cpu_usage: 0.0,
            disk_total: 100, disk_available: 50, unreliable: false };
        let usage = alondra_core::usage::Usage { week_percent: 53.0, week_resets_at: 1_790_553_600_000,
            today_spent: Some(41.0), today_share: Some(18.0), over_pace: true,
            five_hour_percent: Some(10.0), five_hour_resets_at: Some(1_790_959_200_000), ..Default::default() };
        let result = gauges(&machine, Some(&usage), Lang::En, 0);
        assert!(result[0].detail.as_deref().unwrap().contains("resets"));
        assert_eq!(result[1].text, "41/18%");
        assert!(result[1].label.contains('!'));
        assert!(result[1].detail.as_deref().unwrap().contains("faster"));
        assert_eq!((result[2].id.as_str(), result[2].text.as_str()), ("claude_five_hours", "10%"));
        assert!(result[2].detail.as_deref().unwrap().starts_with("5-hour limit resets "));
        assert!(!result[0].detail.as_deref().unwrap().contains("2026-"), "reset dates are human, not ISO");
    }

    #[test]
    fn an_old_reading_says_how_old_it_is_in_both_languages() {
        let machine = alondra_core::Machine { at: 0, memory_total: 100, memory_available: 50,
            memory_free_percent: Some(50), memory_pressure: None, compressor: None, load: [0.0; 3], cpus: 8, cpu_usage: 0.0,
            disk_total: 100, disk_available: 50, unreliable: false };
        let usage = alondra_core::usage::Usage { week_percent: 53.0, week_resets_at: 1_790_553_600_000,
            today_spent: Some(3.0), today_share: Some(10.0), five_hour_percent: Some(10.0),
            five_hour_resets_at: Some(1_790_959_200_000), measured_at: 1_000, ..Default::default() };
        let two_hours = 1_000 + 2 * 3_600_000;
        let details = |lang, now| gauges(&machine, Some(&usage), lang, now).iter().take(3)
            .map(|gauge| gauge.detail.clone().unwrap()).collect::<Vec<_>>();
        assert!(details(Lang::En, 1_000 + 3_600_000).iter().all(|detail| !detail.contains("measured")), "within the hour it is current");
        assert!(details(Lang::En, two_hours).iter().all(|detail| detail.ends_with(" · measured 2 h ago")));
        assert!(details(Lang::Es, two_hours).iter().all(|detail| detail.ends_with(" · medido hace 2 h")));
        assert!(details(Lang::Es, 1_000 + 3 * 86_400_000)[1].starts_with("Gastado 3% de la cuota diaria del 10%"));
        assert!(details(Lang::Es, 1_000 + 3 * 86_400_000)[1].ends_with("medido hace 3 d"));
        let today = gauges(&machine, Some(&usage), Lang::Es, two_hours);
        assert_eq!((today[0].label.as_str(), today[1].label.as_str(), today[1].text.as_str()), ("Claude semana", "Claude hoy", "3/10%"));
    }
}

#[cfg(test)]
mod check_state_tests {
    use super::check_state;
    use alondra_core::{pr::CheckState as Github, views::CheckState as View};

    #[test]
    fn a_check_that_has_not_finished_is_pending_and_only_a_pass_is_green() {
        assert_eq!(
            [Github::Passed, Github::Failed, Github::Running, Github::Other].map(check_state),
            [View::Pass, View::Fail, View::Pending, View::Pending]
        );
    }
}
