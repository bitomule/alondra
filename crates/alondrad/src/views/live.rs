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
use std::{collections::{BTreeMap, BTreeSet, HashMap}, sync::Mutex};
use tokio::sync::watch;

/// A cache whose holder panicked is emptied: the next read fills it again.
fn cache<T: Default>(lock: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    lock.lock().unwrap_or_else(|poisoned| {
        log!("a live read cache was emptied: a thread panicked holding it");
        lock.clear_poison();
        let mut cache = poisoned.into_inner();
        *cache = T::default();
        cache
    })
}

#[derive(Default)]
pub(crate) struct LiveReadCache(Mutex<HashMap<String, (u64, String, Value)>>);

impl LiveReadCache {
    fn get(&self, path: &str, domain_revision: u64, source_revision: &str) -> Option<Value> {
        cache(&self.0).get(path)
            .filter(|(domain, source, _)| *domain == domain_revision && source == source_revision)
            .map(|(_, _, value)| value.clone())
    }

    fn insert(&self, path: String, domain_revision: u64, source_revision: &str, value: Value) {
        let mut cache = cache(&self.0);
        if cache.len() >= 128 && !cache.contains_key(&path) { cache.clear(); }
        cache.insert(path, (domain_revision, source_revision.to_owned(), value));
    }
}

/// Live snapshots by language and view keys, each kept until a fact it was read from changes. The
/// part every key shares is kept once per language, so a key seen for the first time only adds
/// its own data.
#[derive(Default)]
pub(crate) struct LiveSnapshots {
    entries: Mutex<HashMap<String, (Facts, LiveSnapshot)>>,
    #[cfg(test)]
    gathered: std::sync::atomic::AtomicUsize,
    #[cfg(test)]
    keyed: std::sync::atomic::AtomicUsize,
}

/// What a snapshot was read from: the domain revision, every row written to the store, hooks,
/// node reports, the SDK tasks' measured contexts, the usage pause, the admission gate, pull request
/// details, and what only the live sampler sees by looking (clocks, files, gauges), which it
/// announces on the feeds' clock.
#[derive(Clone)]
struct Facts {
    epoch: String,
    revision: alondra_core::domain::Revision,
    store_rows: u64,
    pull_requests: u64,
    contexts: u64,
    domain: watch::Receiver<alondra_core::domain::Revision>,
    hooks: watch::Receiver<()>,
    reports: watch::Receiver<()>,
    usage: watch::Receiver<Option<crate::usage_pause::Exhausted>>,
    gate: watch::Receiver<u64>,
    sampled: watch::Receiver<u64>,
}

impl Facts {
    /// None when the store already holds a newer revision than `state`: a snapshot read now
    /// would mix the two.
    fn observe(client: &crate::client_routes::Client, state: &DomainState) -> Option<Self> {
        let live = &client.domain.live;
        let domain = client.domain.subscribe();
        let hooks = live.changed.subscribe();
        let reports = live.mods.changed.subscribe();
        let usage = live.usage_pause.subscribe();
        let gate = live.gate.subscribe();
        let sampled = client.feeds.subscribe();
        let pull_requests = client.details.version();
        let contexts = client.domain.contexts.version();
        let (store_rows, newest) = client.domain.store.lock().expect("domain store poisoned").write_mark().ok()?;
        (newest <= state.revision && *domain.borrow() == state.revision).then(|| Self {
            epoch: state.data_epoch.0.clone(), revision: state.revision, store_rows, pull_requests, contexts,
            domain, hooks, reports, usage, gate, sampled,
        })
    }

    fn reads(&self, state: &DomainState) -> bool {
        self.epoch == state.data_epoch.0 && self.revision == state.revision
    }

    fn unchanged(&self, now: &Marks) -> bool {
        [self.domain.has_changed(), self.hooks.has_changed(), self.reports.has_changed(),
            self.usage.has_changed(), self.gate.has_changed(), self.sampled.has_changed()]
            .into_iter().all(|changed| matches!(changed, Ok(false)))
            && now.store_rows == Some(self.store_rows) && now.pull_requests == self.pull_requests && now.contexts == self.contexts
    }
}

/// The counted facts as they are now, read once per lookup.
struct Marks {
    store_rows: Option<u64>,
    pull_requests: u64,
    contexts: u64,
}

impl Marks {
    fn now(client: &crate::client_routes::Client) -> Self {
        Self {
            store_rows: client.domain.store.lock().expect("domain store poisoned").write_mark().ok().map(|(rows, _)| rows),
            pull_requests: client.details.version(),
            contexts: client.domain.contexts.version(),
        }
    }
}

impl LiveSnapshots {
    fn get(&self, client: &crate::client_routes::Client, key: &str, state: &DomainState) -> Option<LiveSnapshot> {
        let marks = Marks::now(client);
        let entries = cache(&self.entries);
        let (facts, live) = entries.get(key)?;
        (facts.reads(state) && facts.unchanged(&marks)).then(|| live.clone())
    }

    fn insert(&self, client: &crate::client_routes::Client, key: String, facts: Facts, live: LiveSnapshot) {
        let marks = Marks::now(client);
        if !facts.unchanged(&marks) { return; }
        let mut entries = cache(&self.entries);
        if entries.len() >= 64 && !entries.contains_key(&key) {
            entries.retain(|_, (facts, _)| facts.unchanged(&marks));
            if entries.len() >= 64 { entries.clear(); }
        }
        entries.insert(key, (facts, live));
    }
}

/// Keys whose live data also comes from files or GitHub, which no fact announces.
fn reads_unwatched_sources(key: &str) -> bool {
    ["machine", "new_request", "issue_picker", "mcp_servers"].iter()
        .any(|unwatched| key == *unwatched || key.strip_prefix(unwatched).is_some_and(|rest| rest.starts_with('?')))
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
        ("paused", Lang::En) => "paused",
        ("paused", Lang::Es) => "en pausa",
        (_, Lang::En) => "idle",
        (_, Lang::Es) => "inactivo",
    }.to_owned()
}

pub(crate) async fn gather(
    client: &crate::client_routes::Client,
    state: &std::sync::Arc<DomainState>,
    lang: Lang,
    keys: &[String],
    allow_issue_refresh: bool,
    clock_only: bool,
) -> LiveSnapshot {
    let now = alondra_core::now_millis();
    // The live sampler reads clock-only snapshots to notice what no fact announces; it must
    // always look afresh.
    if clock_only {
        #[cfg(test)]
        client.live_snapshots.gathered.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        return gather_at(client, state, lang, keys, allow_issue_refresh, true, now).await;
    }
    let snapshots = &client.live_snapshots;
    let key = format!("{lang:?}|{}", keys.join("\n"));
    let watched = !keys.iter().any(|key| reads_unwatched_sources(key));
    if watched && let Some(live) = snapshots.get(client, &key, state) {
        return live;
    }
    // GitHub is awaited before the read lane is taken, so a slow issue search never holds it.
    let issue_data = issue_data(client, keys, allow_issue_refresh).await;
    let facts = Facts::observe(client, state);
    let shared = format!("{lang:?}|");
    let mut live = match snapshots.get(client, &shared, state) {
        Some(live) => live,
        None => {
            #[cfg(test)]
            snapshots.gathered.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            let live = gather_at(client, state, lang, &[], false, false, now).await;
            if let Some(facts) = &facts {
                snapshots.insert(client, shared, facts.clone(), live.clone());
            }
            live
        }
    };
    if keys.is_empty() {
        return live;
    }
    keyed_at(client, state, keys, issue_data).await.apply(&mut live);
    if watched && let Some(facts) = facts {
        snapshots.insert(client, key, facts, live.clone());
    }
    live
}

async fn keyed_at(
    client: &crate::client_routes::Client,
    state: &std::sync::Arc<DomainState>,
    keys: &[String],
    issue_data: Option<Value>,
) -> Keyed {
    #[cfg(test)]
    client.live_snapshots.keyed.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let client = client.shared();
    let state = state.clone();
    let keys = keys.to_vec();
    let runtime = tokio::runtime::Handle::current();
    let work = client.read_work.clone();
    work.run(move || runtime.block_on(gather_keyed(&client, &state, &keys, issue_data)))
        .await.unwrap_or_else(|failed| std::panic::resume_unwind(failed.into_panic()))
}

async fn gather_at(
    client: &crate::client_routes::Client,
    state: &std::sync::Arc<DomainState>,
    lang: Lang,
    keys: &[String],
    allow_issue_refresh: bool,
    clock_only: bool,
    now: Millis,
) -> LiveSnapshot {
    let issue_data = issue_data(client, keys, allow_issue_refresh).await;
    let client = client.shared();
    let state = state.clone();
    let keys = keys.to_vec();
    let runtime = tokio::runtime::Handle::current();
    let work = client.read_work.clone();
    work.run(move || runtime.block_on(gather_inner(&client, &state, lang, &keys, issue_data, clock_only, now)))
        .await.unwrap_or_else(|failed| std::panic::resume_unwind(failed.into_panic()))
}

async fn issue_data(client: &crate::client_routes::Client, keys: &[String], refresh_allowed: bool) -> Option<Value> {
    let keys: Vec<_> = keys.iter().filter(|key| key.as_str() == "issue_picker" || key.starts_with("issue_picker?")).collect();
    if keys.is_empty() { return None; }
    if refresh_allowed {
        let path = if keys.iter().any(|key| key.contains("refresh=1")) { "/issues?refresh=1" } else { "/issues" };
        read_json(client, path).await
    } else {
        client.cached_issues().await.map(|issues| serde_json::to_value(issues).expect("issue list serializes"))
    }
}

async fn gather_inner(
    client: &crate::client_routes::Client,
    state: &DomainState,
    lang: Lang,
    keys: &[String],
    issue_data: Option<Value>,
    clock_only: bool,
    now: Millis,
) -> LiveSnapshot {
    let (exhausted, waits_for_session) = match client.domain.live.usage_pause.current() {
        Some(standing) if standing.no_session => (None, true),
        standing => (standing, false),
    };
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
            let activity = match reported.activity() {
                Activity::Stalled if crate::domain_holds::waits_on_user(state, &session_id) => Activity::Working,
                activity => activity,
            };
            let sdk = (agent.phase == NodePhase::ThroughSdk).then(|| state.sdk_live(&agent.id));
            let state_key = match (&agent.phase, activity) {
                (NodePhase::ThroughSdk, _) => match sdk.as_ref().and_then(Option::as_ref) {
                    Some(live) if alondra_core::domain::node_at_work(live) => "busy",
                    Some(alondra_domain::LiveState::WaitingUser(cards)) if cards.iter().all(|card| card.0.starts_with(alondra_core::domain::NODE_PAUSE_PREFIX)) => "paused",
                    Some(alondra_domain::LiveState::WaitingCapacity(_)) => "waiting_machine",
                    _ => "free",
                },
                (NodePhase::Working | NodePhase::Starting { .. }, Activity::Stalled) => "stalled",
                (NodePhase::Working | NodePhase::Starting { .. }, Activity::Failed) => "failed",
                (NodePhase::Working | NodePhase::Starting { .. } | NodePhase::Delivered { in_turn: true }
                    | NodePhase::WaitingBackground(_) | NodePhase::Resuming { .. }, _) => "busy",
                (NodePhase::Paused(alondra_core::domain::Pause { reason: alondra_core::domain::PauseReason::ByUser, .. }), _) => "paused",
                (NodePhase::Paused(_) | NodePhase::Lost, _) => "waiting_machine",
                _ => "free",
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
            let running: Vec<(&str, &alondra_core::BackgroundItem)> = reported.background.iter()
                .filter(|(_, item)| reported.shell_end(item).is_none()).map(|(id, item)| (id.as_str(), item)).collect();
            let counts = [alondra_core::BackgroundKind::Agent, alondra_core::BackgroundKind::Shell,
                alondra_core::BackgroundKind::Other].into_iter().filter_map(|kind| {
                    let count = running.iter().filter(|(_, item)| item.kind == kind).count();
                    (count > 0).then(|| {
                        let noun = match (lang, kind) {
                            (Lang::En, alondra_core::BackgroundKind::Agent) => "subagent",
                            (Lang::En, alondra_core::BackgroundKind::Shell) => "shell",
                            (Lang::En, alondra_core::BackgroundKind::Other) => "task",
                            (Lang::Es, alondra_core::BackgroundKind::Agent) => "subagente",
                            (Lang::Es, alondra_core::BackgroundKind::Shell) => "shell",
                            (Lang::Es, alondra_core::BackgroundKind::Other) => "tarea",
                        };
                        format!("{count} {noun}{}", if count == 1 { "" } else { "s" })
                    })
                }).collect::<Vec<_>>();
            let background_badge = (!counts.is_empty()).then(|| counts.join(" · "));
            let context = state.tasks.values().filter(|task| task.agent_id.as_ref() == Some(&agent.id))
                .find_map(|task| client.domain.contexts.used(&task.id))
                .map(|used| (used.percent(), used.window));
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
                    state: run_state(agent.started_at, agent.end),
                    workflow: None,
                }).collect(),
                workflows: reported.work.workflows.iter().map(|workflow| {
                    let state = workflow.transcript_dir.as_deref()
                        .and_then(|dir| crate::workflow_state::read(dir, workflow.end.is_some()));
                    LiveRun {
                        label: workflow.name.clone(),
                        kind: None,
                        model: None,
                        agents: state.as_ref().map(|state| state.running + state.done)
                            .or_else(|| workflow.transcript_dir.as_deref().and_then(crate::node_work::workflow_agents)),
                        since: timestamp(workflow.started_at),
                        state: run_state(workflow.started_at, workflow.end),
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
                background: reported.background.iter().filter(|(id, _)| !reported.work.runs(id))
                    .map(|(_, item)| (item, reported.shell_end(item)))
                    .chain(reported.work.ended_tasks.iter().map(|task| (&task.item, Some(task.end))))
                    .map(|(item, end)| LiveBackground {
                        kind: serde_json::to_value(item.kind).ok().and_then(|value| value.as_str().map(str::to_owned)).unwrap_or_else(|| "other".into()),
                        label: item.label.clone(),
                        since: timestamp(item.started_at),
                        state: run_state(item.started_at, end),
                    }).collect(),
                background_badge,
                working: match &sdk {
                    Some(live) => matches!(live, Some(alondra_domain::LiveState::Working(alondra_domain::Worker::Turn))),
                    None => agent.phase == NodePhase::Working,
                },
                waiting: match &sdk {
                    Some(live) => matches!(live, Some(alondra_domain::LiveState::WaitingUser(_))),
                    None => matches!(agent.phase, NodePhase::WaitingUser { .. } | NodePhase::Paused(_)),
                },
                state_since: state_since.map(timestamp),
                last_message: answer.map(|answer| answer.text.clone()),
                last_message_at: answer.map(|answer| timestamp(answer.at)),
                last_turn_aborted: answer.is_some_and(|answer| answer.aborted),
                tool: tool.map(|tool| tool.chars().take(141).collect()),
                tool_since: reported.tool.as_ref().map(|tool| crate::usage_pause::clock_text(tool.since, now, lang == Lang::Es)),
                context_percent: context.and_then(|(percent, _)| percent).map(|percent| percent.round().clamp(0.0, 999.0) as u32),
                context_window: context.and_then(|(_, window)| window),
                cost_cents: reported.sdk.as_ref().and_then(|readings| readings.cost_cents),
                model: reported.model.clone(),
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
                url: Some(crate::domain_github::pr_url(&pr.host, &pr.repository, pr.number)),
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
        waits_for_sign_in: value["waits_for_sign_in"].as_bool().unwrap_or(false),
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
    let sample = crate::log::recover(&client.gauges, "the machine gauges").clone();
    let gauges = if let Some((machine, usage)) = sample {
        let mut published = cache(&client.views_gauges);
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
    let (task_since, task_started, cancel_reasons) = if clock_only { (BTreeMap::new(), BTreeMap::new(), BTreeMap::new()) } else {
        let (times, started, reasons) = {
            let store = client.domain.store.lock().expect("domain store poisoned");
            (store.task_state_times().unwrap_or_default(), store.task_started_times().unwrap_or_default(), store.cancel_reasons().unwrap_or_default())
        };
        let visible = || state.tasks.values().filter(|task| !task.archived);
        let since = visible()
            .filter_map(|task| times.get(&task.id).map(|at| (task.id.0.clone(), timestamp(*at))))
            .collect();
        let started = visible().filter_map(|task| {
            let since = *started.get(&task.id)?;
            let until = matches!(task.state, TaskState::Done | TaskState::Cancelled).then(|| times.get(&task.id).copied()).flatten();
            Some((task.id.0.clone(), alondra_core::views::LiveTaskStart {
                since: timestamp(since),
                until: until.map(timestamp),
                ran_secs: until.map(|until| until.saturating_sub(since).max(0) as u64 / 1000),
            }))
        }).collect();
        (since, started, reasons.into_iter().map(|(task, reason)| (task.0, reason)).collect())
    };
    let task_options = state.tasks.values().map(|task| {
        let (_, effort) = launches.get(&task.request_id).cloned().unwrap_or_default();
        (task.id.0.clone(), alondra_core::views::live::LiveTaskOptions { effort })
    }).collect();
    let keyed = gather_keyed(client, state, keys, issue_data).await;
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
        projects: keyed.projects,
        options: keyed.options,
        project_options: keyed.project_options,
        issues: keyed.issues,
        issue_error: keyed.issue_error,
        activity: keyed.activity,
        task_history: keyed.task_history,
        task_options,
        task_since,
        task_started,
        cancel_reasons,
        search_matches: keyed.search_matches,
        usage_exhausted: exhausted.map(|exhausted| alondra_core::views::LiveUsageExhausted {
            since: timestamp(exhausted.since),
            until: timestamp(exhausted.until),
            paused_by_user: exhausted.by_user,
        }),
        waits_for_session,
        launch_refusals,
        start_waits,
        pacing: client.domain.live.gate.live(&client.domain, now),
        allowed_rules: keyed.allowed_rules,
        allow_always_missing: keyed.allow_always_missing,
        mcp_probes: keyed.mcp_probes,
        mcp_removed: keyed.mcp_removed,
        metrics: keyed.metrics,
    }
}

/// The live data only some view keys show; the rest of a snapshot is the same for every key.
#[derive(Default)]
struct Keyed {
    projects: Vec<alondra_core::options::ProjectEntry>,
    options: Vec<alondra_core::options::Spec>,
    project_options: BTreeMap<String, Vec<alondra_core::options::Spec>>,
    issues: Vec<alondra_core::issues::Issue>,
    issue_error: Option<String>,
    activity: BTreeMap<String, Vec<LiveActivity>>,
    task_history: BTreeMap<String, Vec<LiveHistoryStep>>,
    search_matches: Option<Vec<String>>,
    allowed_rules: Vec<alondra_core::views::LiveAllowedRule>,
    allow_always_missing: Vec<String>,
    mcp_probes: BTreeMap<String, alondra_core::views::LiveMcpProbe>,
    mcp_removed: Vec<alondra_core::views::LiveMcpRemoval>,
    metrics: Option<alondra_core::metrics::report::MetricsInputs>,
}

impl Keyed {
    fn apply(self, live: &mut LiveSnapshot) {
        live.projects = self.projects;
        live.options = self.options;
        live.project_options = self.project_options;
        live.issues = self.issues;
        live.issue_error = self.issue_error;
        live.activity = self.activity;
        live.task_history = self.task_history;
        live.search_matches = self.search_matches;
        live.allowed_rules = self.allowed_rules;
        live.allow_always_missing = self.allow_always_missing;
        live.mcp_probes = self.mcp_probes;
        live.mcp_removed = self.mcp_removed;
        live.metrics = self.metrics;
    }
}

async fn gather_keyed(
    client: &crate::client_routes::Client,
    state: &DomainState,
    keys: &[String],
    issue_data: Option<Value>,
) -> Keyed {
    let wants_new = keys
        .iter()
        .any(|key| key.as_str() == "new_request" || key.starts_with("new_request?"));
    let wants_mcp = keys.iter().any(|key| key == alondra_core::views::mcp_servers::KEY);
    let mut projects = Vec::new();
    if wants_mcp && !wants_new
        && let Some(data) = read_json(client, "/projects").await
    {
        projects = serde_json::from_value(data["projects"].clone()).unwrap_or_default();
    }
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
    let (issues, issue_error) = match issue_data {
        Some(data) => (serde_json::from_value(data["issues"].clone()).unwrap_or_default(), data["error"].as_str().map(str::to_owned)),
        None => (Vec::new(), None),
    };
    let mut activity = BTreeMap::new();
    for id in activity_agents(state, keys) {
        let id = id.as_str();
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
    let (mcp_probes, mcp_removed) = if wants_mcp { (client.mcp_probes.snapshot(), client.mcp_probes.removals()) } else { Default::default() };
    let wants_metrics = keys.iter().any(|key| alondra_core::views::Family::of(key) == Some(alondra_core::views::Family::Metrics));
    let metrics = wants_metrics.then(|| client.domain.metrics_inputs(alondra_core::now_millis()))
        .and_then(|read| read.inspect_err(|error| log!("the metrics could not be read: {error:#}")).ok());
    Keyed { projects, options, project_options, issues, issue_error, activity, task_history, search_matches, allowed_rules, allow_always_missing, mcp_probes, mcp_removed, metrics }
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
    let response = client.read_inner(path).await?;
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

/// The nodes whose activity an open screen shows: the node of an open task.
fn activity_agents(state: &DomainState, keys: &[String]) -> BTreeSet<String> {
    keys.iter()
        .filter_map(|key| {
            let task = state.tasks.get(&TaskId(key.strip_prefix("task:")?.to_owned()))?;
            task.agent_id.as_ref().filter(|_| !task.state.terminal()).map(|agent| agent.0.clone())
        })
        .filter(|id| !matches!(state.resolve_target(id), alondra_core::domain::Target::Orchestrator(_)))
        .collect()
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
        // Opened off the runtime so the live sampler does not start: these tests gather by hand
        // and play the sampler with `feeds.wake()`.
        let client = std::thread::scope(|scope| scope.spawn(||
            crate::client_routes::Client::open(domain.clone(), &home, Arc::new(Mutex::new(None)))).join().unwrap());
        execute(&domain, Actor::Orchestrator { generation: 0 }, "request", Command::CreateRequest {
            request_id: "request".into(), body: "Work".into(), auto_merge: false, maintenance: false, message: false,
        });
        execute(&domain, Actor::Orchestrator { generation: 0 }, "title", Command::TitleRequest { request_id: "request".into(), title: "Work".into() });
        for id in ["room", "launch", "relaunch", "send", "closed", "retry"] {
            execute(&domain, Actor::Orchestrator { generation: 0 }, id, Command::CreateTask { plan_items: None,
                task_id: id.into(), request_id: "request".into(), title: id.into(), agent_id: None,
                review_prs: true, dependencies: Default::default(), stack: None,
                launch: Some(alondra_core::domain::LaunchPlan::new("node", home.to_str().unwrap())),
            });
        }
        (home, domain, client)
    }

    fn gathered(client: &crate::client_routes::Client) -> usize {
        client.live_snapshots.gathered.load(std::sync::atomic::Ordering::Relaxed)
    }

    fn keyed(client: &crate::client_routes::Client) -> usize {
        client.live_snapshots.keyed.load(std::sync::atomic::Ordering::Relaxed)
    }

    #[tokio::test]
    async fn a_live_snapshot_is_gathered_once_until_a_fact_it_reads_changes() {
        let (_home, domain, client) = wait_fixture();
        let home = ["home".to_owned()];
        let state = domain.state().unwrap();
        let first = gather(&client, &state, Lang::En, &home, true, false).await;
        assert_eq!(gather(&client, &state, Lang::En, &home, true, false).await, first);
        assert_eq!(gathered(&client), 1, "nothing changed, so the second view reuses the snapshot");
        let getter_calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let calls = getter_calls.clone();
        domain.store.lock().unwrap().on_live_read(move |_| {
            calls.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        });
        let before = (gathered(&client), keyed(&client));
        domain.store.lock().unwrap().begin_live_read_trace();
        for _ in 0..3 {
            assert_eq!(gather(&client, &state, Lang::En, &home, true, false).await, first);
        }
        let trace = domain.store.lock().unwrap().end_live_read_trace();
        assert_eq!(getter_calls.load(std::sync::atomic::Ordering::Relaxed), 0, "warm snapshots must not call instrumented live getters");
        assert!(trace.is_empty(), "warm snapshots must not execute instrumented live getters: {trace:?}");
        assert_eq!((gathered(&client), keyed(&client)), before, "warm snapshots reuse shared and keyed data");
        gather(&client, &state, Lang::Es, &home, true, false).await;
        assert_eq!(gathered(&client), 2, "each language has its own snapshot");
        let task = ["task:room".to_owned()];
        let history = gather(&client, &state, Lang::En, &task, true, false).await;
        assert_eq!((gathered(&client), keyed(&client)), (2, 3), "a new view key reads only its own data");
        assert!(history.task_history.contains_key("room"));
        assert_eq!(gather(&client, &state, Lang::En, &task, true, false).await, history);
        assert_eq!(keyed(&client), 3, "and reuses it while nothing changes");

        let changes: [(&str, Box<dyn Fn() + '_>); 6] = [
            ("a store write outside commands", Box::new(|| domain.store.lock().unwrap().set_start_retry(&"retry".into(), Some((1, 2_000_000_000_000))).unwrap())),
            ("a hook", Box::new(|| domain.live.observe(&serde_json::json!({"session_id": "node", "hook_event_name": "Stop", "last_assistant_message": "Done"})))),
            ("a node report", Box::new(|| report(&domain, serde_json::json!({"session_id": "node", "event": "turn.complete", "answer": "Done"}), 1_000))),
            ("a closed node's task", Box::new(|| domain.live.remember_closed_task("closed", crate::client_routes::ClosedNode { name: "Closed".into(), cwd: None }))),
            ("pull request details", Box::new(|| client.details.record("pr", Default::default()))),
            ("the live sampler", Box::new(|| client.feeds.wake())),
        ];
        for (fact, change) in changes {
            let before = gathered(&client);
            change();
            gather(&client, &state, Lang::En, &home, true, false).await;
            assert_eq!(gathered(&client), before + 1, "{fact} gathers again");
            gather(&client, &state, Lang::En, &home, true, false).await;
            assert_eq!(gathered(&client), before + 1, "after {fact} the new snapshot is reused");
        }
        let retried = gather(&client, &state, Lang::En, &home, true, false).await;
        assert!(matches!(retried.start_waits["retry"], alondra_core::views::LiveStartWait::Retry { .. }));

        let before = gathered(&client);
        execute(&domain, Actor::Orchestrator { generation: 0 }, "retitle", Command::TitleRequest { request_id: "request".into(), title: "Work again".into() });
        gather(&client, &state, Lang::En, &home, true, false).await;
        gather(&client, &state, Lang::En, &home, true, false).await;
        assert_eq!(gathered(&client), before + 2, "an older revision than the store's is never kept");
        let state = domain.state().unwrap();
        gather(&client, &state, Lang::En, &home, true, false).await;
        gather(&client, &state, Lang::En, &home, true, false).await;
        assert_eq!(gathered(&client), before + 3, "the new revision is gathered once");

        let (before, keyed_before) = (gathered(&client), keyed(&client));
        for key in ["machine", "new_request?project=app", "issue_picker"] {
            gather(&client, &state, Lang::En, &[key.to_owned()], false, false).await;
            gather(&client, &state, Lang::En, &[key.to_owned()], false, false).await;
        }
        assert_eq!((gathered(&client), keyed(&client)), (before, keyed_before + 6), "data from files and GitHub is always read afresh");
        gather(&client, &state, Lang::En, &home, false, true).await;
        gather(&client, &state, Lang::En, &home, false, true).await;
        assert_eq!(gathered(&client), before + 2, "so are the live sampler's own reads");
    }

    #[tokio::test]
    async fn a_live_snapshot_read_while_a_fact_changes_is_not_kept() {
        let (_home, domain, client) = wait_fixture();
        let home = ["home".to_owned()];
        let state = domain.state().unwrap();
        let fired = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let (weak, once) = (Arc::downgrade(&client), fired.clone());
        domain.store.lock().unwrap().on_live_read(move |getter| {
            if getter == "task_state_times" && !once.swap(true, std::sync::atomic::Ordering::SeqCst)
                && let Some(client) = weak.upgrade() { client.feeds.wake(); }
        });
        gather(&client, &state, Lang::En, &home, true, false).await;
        assert!(fired.load(std::sync::atomic::Ordering::SeqCst));
        gather(&client, &state, Lang::En, &home, true, false).await;
        assert_eq!(gathered(&client), 2, "the snapshot read across the change is gathered again");
        gather(&client, &state, Lang::En, &home, true, false).await;
        assert_eq!(gathered(&client), 2, "the one read after it is kept");
    }

    #[tokio::test]
    async fn a_task_runs_since_its_first_start_in_its_history_through_waits_and_restarts() {
        let (_home, domain, client) = wait_fixture();
        let start = 1_700_000_000_000;
        for (state, at) in [("not_started", start - 600_000), ("running", start), ("needs_you", start + 60_000), ("running", start + 120_000)] {
            domain.store.lock().unwrap().add_history_step("room", state, at);
        }
        let state = domain.state().unwrap();
        let live = gather_at(&client, &state, Lang::En, &["home".to_owned()], true, false, start + 180_000).await;
        assert_eq!(live.task_started.get("room"), Some(&alondra_core::views::LiveTaskStart {
            since: timestamp(start), until: None, ran_secs: None,
        }), "the first start counts, not the queue before it nor the restart after the wait");
        assert!(!live.task_started.contains_key("launch"), "a task that never ran has no start");
    }

    #[tokio::test]
    async fn live_gather_bounds_sqlite_work_on_a_long_task_history() {
        let mut measured = Vec::new();
        for steps in [2, 20] {
            let (home, domain, client) = wait_fixture();
            domain.store.lock().unwrap().add_task_history(314, steps);
            let connection = rusqlite::Connection::open(crate::domain_database_migration::active_path(&home)).unwrap();
            let tasks: i64 = connection.query_row("SELECT count(*) FROM tasks", [], |row| row.get(0)).unwrap();
            let tasks = u64::try_from(tasks).unwrap();
            let history: i64 = connection.query_row("SELECT count(*) FROM task_history", [], |row| row.get(0)).unwrap();
            let history = u64::try_from(history).unwrap();
            let latest: i64 = connection.query_row("SELECT a.at FROM task_history h JOIN command_audit a ON a.revision=h.revision WHERE h.task_id='room' ORDER BY h.revision DESC LIMIT 1", [], |row| row.get(0)).unwrap();
            assert_eq!(tasks, 320, "both fixtures retain the same Task population");
            assert!(history >= tasks * steps as u64, "the real SQL history must be populated");
            let state = domain.state().unwrap();
            domain.store.lock().unwrap().begin_live_read_trace();
            let live = gather_at(&client, &state, Lang::En, &["home".to_owned()], true, false, 1_700_000_000_000).await;
            let trace = domain.store.lock().unwrap().end_live_read_trace();
            assert_eq!(live.task_since.len(), 6, "finished archived Tasks stay out of the visible snapshot");
            assert_eq!(live.task_since["room"], timestamp(latest), "the actual latest history value reaches the snapshot");
            let mut getters = BTreeMap::new();
            for read in &trace {
                *getters.entry(read.getter).or_insert(0) += 1;
                assert!(!read.failed, "live getter must complete: {read:?}");
                assert!(read.statement_attempts <= 1, "one statement per live getter: {read:?}");
            }
            assert_eq!(getters, BTreeMap::from([
                ("request_launch_summaries", 1), ("launch_refusals", 1), ("start_retries", 1),
                ("task_was_launched", 6), ("revision_times", 1), ("request_times", 1),
                ("task_state_times", 1), ("task_started_times", 1), ("cancel_reasons", 1),
            ]), "a cold home snapshot has a bounded set of real live getter calls");
            let started = trace.iter().find(|read| read.getter == "task_started_times").unwrap();
            let started_work = u64::try_from(started.vm_steps.expect("the first-start statement reports VM steps")).unwrap();
            assert!(started_work <= 64 * (history + tasks), "first Task start read exceeded linear SQLite work budget: {started_work} VM steps");
            let read = trace.iter().find(|read| read.getter == "task_state_times").unwrap();
            assert_eq!(read.statement_attempts, 1);
            assert_eq!(read.rows as u64, tasks, "the latest-state query returns one row per Task, not per history entry");
            let work = u64::try_from(read.vm_steps.expect("the executed latest-state statement must report VM steps")).unwrap();
            assert!(work > 0, "the work budget must measure the actual SQLite statement");
            // SQLite opcodes cover the grouped scan and per-Task join, with room for fixed
            // setup/sort overhead. This bounds this statement, not all work of a snapshot.
            assert!(work <= 64 * (history + tasks), "latest Task state read exceeded linear SQLite work budget: {work} VM steps for {history} history rows and {tasks} Tasks");
            measured.push((history, work));
        }
        assert!(measured[1].0 > 9 * measured[0].0, "the second fixture contains approximately ten times the SQL history");
        assert!(measured[1].1 <= 10 * measured[0].1, "latest-state SQLite work grows at most with the history increase: {measured:?}");
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
        execute(&domain, Actor::Orchestrator { generation: 0 }, "task", Command::CreateTask { plan_items: None,
            task_id: "task".into(), request_id: "request".into(), title: "Work".into(), agent_id: Some("assigned".into()),
            review_prs: true, dependencies: Default::default(), stack: None,
            launch: Some(alondra_core::domain::LaunchPlan::new("node", home.to_str().unwrap())),
        });
        report(&domain, serde_json::json!({"session_id": "reported", "event": "session.start", "version": "test"}), 1_000);
        let state = domain.state().unwrap();
        let first = gather_at(&client, &state, Lang::En, &[], false, false, 2_000).await;
        let later = gather_at(&client, &state, Lang::En, &[], false, false, 2_000 + 30 * 60_000).await;
        let first_view = client.views.render("tasks", &alondra_core::domain::ReadSnapshot::build(state.clone()), &first, Client::Mac, Lang::En, Transport::Loopback).unwrap();
        let later_view = client.views.render("tasks", &alondra_core::domain::ReadSnapshot::build(state.clone()), &later, Client::Mac, Lang::En, Transport::Loopback).unwrap();
        assert_eq!(client.views.build_count(), 1, "reading an unobserved node again must reuse the view");
        assert_eq!(first.nodes.iter().map(|node| node.agent_id.as_str()).collect::<Vec<_>>(), ["assigned", "connected", "reported"]);
        for node in &first.nodes {
            assert_eq!(node.state_since, None, "a session or task is not an observed node transition");
        }
        assert_eq!(serde_json::to_value(&later).unwrap(), serde_json::to_value(&first).unwrap());
        assert_eq!(later_view, first_view);
    }

    #[tokio::test]
    async fn observed_nodes_keep_transition_times() {
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
        let before = gather_at(&client, &state, Lang::En, &[], false, false, since + 1).await;
        let later = gather_at(&client, &state, Lang::En, &[], false, false, since + 30 * 60_000).await;
        assert_eq!(before.nodes[0].state_since, Some(timestamp(since)));
        assert_eq!(later.nodes[0].state_since, before.nodes[0].state_since);
        let before_view = client.views.render("tasks", &alondra_core::domain::ReadSnapshot::build(state.clone()), &before, Client::Mac, Lang::En, Transport::Loopback).unwrap();
        let later_view = client.views.render("tasks", &alondra_core::domain::ReadSnapshot::build(state.clone()), &later, Client::Mac, Lang::En, Transport::Loopback).unwrap();
        assert_eq!(later_view, before_view, "time alone changes nothing a node shows");
        report(&domain, serde_json::json!({"session_id": "known", "event": "turn.complete", "answer": "New answer"}), since + 1_000);
        let recent_answer = gather_at(&client, &state, Lang::En, &[], false, false, since + 30 * 60_000).await;
        assert_eq!(recent_answer.nodes[0].last_message.as_deref(), Some("New answer"));
        assert_eq!(recent_answer.nodes[0].state_since, Some(timestamp(since)));
    }

    #[tokio::test]
    async fn report_updates_and_a_call_whose_shell_is_gone_invalidate_unobserved_nodes() {
        use alondra_core::views::{Client, Transport};
        let home = crate::test_support::Directory::new();
        fs::create_dir_all(&home).unwrap();
        let domain = Arc::new(crate::domain_runtime::DomainRuntime::open(&home).unwrap());
        let client = crate::client_routes::Client::open(domain.clone(), &home, Arc::new(Mutex::new(None)));
        execute(&domain, Actor::System, "agent", Command::RegisterAgent { agent_id: "reported".into(), name: "Reported".into(), at: 0 });
        report(&domain, serde_json::json!({"session_id": "reported", "event": "session.start", "version": "test"}), 1_000);
        report(&domain, serde_json::json!({"session_id": "reported", "event": "tool.start", "tool": "Bash", "input": {"command": "build"}}), 2_000);
        let state = domain.state().unwrap();
        let limit = 2_000 + 48 * 60 * 60_000;
        let working = gather_at(&client, &state, Lang::En, &[], false, false, limit).await;
        assert_eq!(working.nodes[0].state_key.as_deref(), Some("busy"), "two days in one call is still work");
        assert_eq!(working.nodes[0].tool_since, Some(crate::usage_pause::clock_text(2_000, limit, false)));
        domain.live.mods.set_shell_alive("reported", Some(false), limit);
        domain.live.mods.set_shell_alive("reported", Some(false), limit + 1);
        let stalled = gather_at(&client, &state, Lang::En, &[], false, false, limit + 1).await;
        assert_eq!(stalled.nodes[0].state_key.as_deref(), Some("stalled"), "its shell is gone: the call will never end");
        let working_view = client.views.render("tasks", &alondra_core::domain::ReadSnapshot::build(state.clone()), &working, Client::Mac, Lang::En, Transport::Loopback).unwrap();
        let stalled_view = client.views.render("tasks", &alondra_core::domain::ReadSnapshot::build(state.clone()), &stalled, Client::Mac, Lang::En, Transport::Loopback).unwrap();
        assert_ne!(stalled_view.cursor, working_view.cursor);
        report(&domain, serde_json::json!({"session_id": "reported", "event": "turn.complete", "answer": "Done", "aborted": true}), limit + 2);
        let answer = gather_at(&client, &state, Lang::En, &[], false, false, limit + 2).await;
        assert_eq!(answer.nodes[0].state_key.as_deref(), Some("busy"), "its state is the node machine's, which still has it starting");
        assert_eq!(answer.nodes[0].last_message.as_deref(), Some("Done"));
        assert_eq!(answer.nodes[0].last_message_at, Some(timestamp(limit + 2)));
        assert!(answer.nodes[0].last_turn_aborted);
        assert_eq!(answer.nodes[0].state_since, None);
        let answer_view = client.views.render("tasks", &alondra_core::domain::ReadSnapshot::build(state.clone()), &answer, Client::Mac, Lang::En, Transport::Loopback).unwrap();
        assert_ne!(answer_view.cursor, stalled_view.cursor);
        assert_eq!(client.views.build_count(), 3);
    }

    #[tokio::test]
    async fn a_node_of_the_agent_sdk_shows_what_its_host_observed() {
        use serde_json::json;
        let home = crate::test_support::Directory::new();
        fs::create_dir_all(&home).unwrap();
        let domain = Arc::new(crate::domain_runtime::DomainRuntime::open(&home).unwrap());
        let client = crate::client_routes::Client::open(domain.clone(), &home, Arc::new(Mutex::new(None)));
        execute(&domain, Actor::System, "agent", Command::RegisterAgent { agent_id: "sdk-a".into(), name: "Writer".into(), at: 0 });
        let message = |parent: Option<&str>, content: Value| json!({"type": "assistant", "parent_tool_use_id": parent,
            "message": {"role": "assistant", "model": "claude-opus-5-5", "content": content}});
        for (at, observed) in [
            json!({"type": "system", "subtype": "init", "cwd": home.to_str().unwrap(), "model": "claude-opus-5-5"}),
            message(None, json!([{"type": "text", "text": "Notes drafted."}])),
            json!({"type": "result", "subtype": "success", "result": "Notes drafted.", "total_cost_usd": 0.42}),
            message(None, json!([{"type": "tool_use", "id": "toolu_todo", "name": "TodoWrite", "input": {"todos": [
                {"content": "Write the changelog", "status": "in_progress", "activeForm": "Writing the changelog"}]}}])),
            json!({"type": "system", "subtype": "task_started", "task_id": "a1", "tool_use_id": "toolu_agent", "task_type": "local_agent",
                "description": "Check the links", "subagent_type": "general-purpose"}),
            json!({"type": "system", "subtype": "background_tasks_changed", "tasks": [
                {"task_id": "b1", "task_type": "local_bash", "description": "sleep 600"},
                {"task_id": "a1", "task_type": "local_agent", "description": "Check the links"}]}),
            message(None, json!([{"type": "tool_use", "id": "toolu_bash", "name": "Bash", "input": {"command": "cargo test"}}])),
            message(Some("toolu_agent"), json!([{"type": "tool_use", "id": "toolu_read", "name": "Read", "input": {"file_path": "/w/README.md"}}])),
        ].into_iter().enumerate() {
            domain.live.mods.apply_sdk("sdk-a", &observed, 1_000 + at as Millis);
        }
        let state = domain.state().unwrap();
        let live = gather_at(&client, &state, Lang::En, &[], false, false, 2_000).await;
        let node = live.nodes.iter().find(|node| node.agent_id == "sdk-a").expect("the SDK node is in the snapshot");
        assert_eq!(node.tool.as_deref(), Some("Bash: cargo test"));
        assert_eq!(node.last_message.as_deref(), Some("Notes drafted."));
        assert_eq!(node.last_message_at, Some(timestamp(1_002)));
        assert_eq!(node.cost_cents, Some(42));
        assert_eq!(node.model.as_deref(), Some("claude-opus-5-5"));
        assert_eq!(node.background_badge.as_deref(), Some("1 subagent · 1 shell"));
        assert_eq!(node.background.iter().map(|item| item.label.as_str()).collect::<Vec<_>>(), ["sleep 600"], "a running subagent shows as a run, not twice");
        assert_eq!(node.subagents.iter().map(|run| run.label.as_str()).collect::<Vec<_>>(), ["Check the links"]);
        assert_eq!(node.steps.iter().map(|step| (step.subject.as_str(), step.state)).collect::<Vec<_>>(), [("Write the changelog", StepState::InProgress)]);
        let threads = domain.live.threads("sdk-a").expect("its activity");
        assert_eq!(threads.iter().map(|thread| (thread.agent_id.as_deref(), thread.summary.as_deref())).collect::<Vec<_>>(),
            [(None, Some("cargo test")), (Some("a1"), Some("/w/README.md"))]);
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
    async fn the_gate_and_tasks_list_count_the_same_nodes_working() {
        use alondra_core::views::{Client, Transport};
        use crate::node_reports::{Envelope, Report};
        let home = crate::test_support::Directory::new();
        fs::create_dir_all(&home).unwrap();
        let domain = Arc::new(crate::domain_runtime::DomainRuntime::open(&home).unwrap());
        domain.live.gate.tune(crate::admission::Tuning { spacing: 0, limit: Some(crate::admission::Limit::Fixed(2)) });
        let client = crate::client_routes::Client::open(domain.clone(), &home, Arc::new(Mutex::new(None)));
        execute(&domain, Actor::User { user_id: "u".into() }, "request", Command::CreateRequest {
            request_id: "request".into(), body: "Work".into(), auto_merge: false, maintenance: false, message: false,
        });
        execute(&domain, Actor::User { user_id: "u".into() }, "title", Command::TitleRequest { request_id: "request".into(), title: "Work".into() });
        let now = alondra_core::now_millis();
        for id in ["build", "docs", "harness"] {
            let agent = format!("agent-{id}");
            execute(&domain, Actor::Orchestrator { generation: 0 }, &format!("create-{id}"), Command::CreateTask { plan_items: None,
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
        assert!(domain.live.gate.hold_back(&domain, "agent-harness", crate::pauses::Reason::Declared {
            reason: "The build ran out of memory".into(),
        }, now));

        let state = domain.state().unwrap();
        let gate = domain.live.gate.snapshot(&domain, now).unwrap();
        let live = gather_at(&client, &state, Lang::En, &[], false, false, now).await;
        let tasks = client.views.render("tasks", &alondra_core::domain::ReadSnapshot::build(state.clone()), &live, Client::Mac, Lang::En, Transport::Loopback).unwrap();
        let mut listed = vec![];
        rows_with(&serde_json::to_value(&tasks).unwrap(), "Working", &mut listed);
        listed.sort();
        let live_working: Vec<&str> = live.nodes.iter().filter(|node| node.working).map(|node| node.agent_id.as_str()).collect();

        assert_eq!(gate.working_tasks.iter().map(|task| task.0.as_str()).collect::<Vec<_>>(), ["build", "docs"]);
        assert_eq!(live.pacing.as_ref().map(|pacing| pacing.working), Some(2));
        assert_eq!(listed, ["build", "docs"], "the Tasks list says Working of the same tasks");
        assert_eq!(live_working, ["agent-build", "agent-docs"], "the live nodes do not count the node waiting for the machine as working");
    }

    #[test]
    fn published_gauges_debounce_for_a_minute_and_ignore_invisible_values() {
        let now = std::time::Instant::now();
        let mut published = None;
        let gauge = |text: &str, value: f64| Gauge {
            id: "usage".into(), label: "Usage".into(), value, text: text.into(),
            detail: None, level: Level::Calm, parts: vec![]
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

    /// What a node does right now is on its task's screen too, while the task is open.
    #[test]
    fn an_open_task_screen_reads_its_nodes_activity_and_a_finished_one_does_not() {
        let home = crate::test_support::Directory::new();
        fs::create_dir_all(&home).unwrap();
        let domain = crate::domain_runtime::DomainRuntime::open(&home).unwrap();
        let run = |actor: Actor, id: &str, command: Command| {
            let state = domain.state().unwrap();
            domain.execute(&actor, &CommandEnvelope {
                protocol_version: 2, data_epoch: state.data_epoch.clone(), command_id: id.into(),
                expected_revision: state.revision, command,
            }, None, || Ok(())).unwrap();
        };
        run(Actor::System, "agent", Command::RegisterAgent { agent_id: "a".into(), name: "Phase B".into(), at: 0 });
        run(Actor::User { user_id: "u".into() }, "request", Command::CreateRequest {
            request_id: "r".into(), body: "Index".into(), auto_merge: false, maintenance: false, message: false });
        run(Actor::Orchestrator { generation: 0 }, "task", Command::CreateTask {
            task_id: "t-b".into(), request_id: "r".into(), title: "Phase B".into(), agent_id: Some("a".into()),
            review_prs: false, dependencies: Default::default(), stack: None,
            launch: Some(crate::launch_plans::LaunchPlan { mission: Some("Phase B".into()), ..crate::launch_plans::LaunchPlan::new("t-b", "/tmp") }),
            plan_items: None,
        });
        let keys = |keys: &[&str]| keys.iter().map(|key| key.to_string()).collect::<Vec<_>>();
        let mut state = (*domain.state().unwrap()).clone();
        assert_eq!(activity_agents(&state, &keys(&["task:t-b", "home"])), BTreeSet::from(["a".to_owned()]));
        assert!(activity_agents(&state, &keys(&["node:a"])).is_empty(), "no screen reads a node by its own key");
        state.tasks.get_mut(&TaskId("t-b".into())).unwrap().state = TaskState::Done;
        assert!(activity_agents(&state, &keys(&["task:t-b", "task:missing"])).is_empty());
    }

    #[tokio::test]
    async fn live_reads_go_on_after_threads_panicked_holding_the_gate_the_machine_watch_and_the_caches() {
        let home = crate::test_support::Directory::new();
        fs::create_dir_all(&home).unwrap();
        let domain = Arc::new(crate::domain_runtime::DomainRuntime::open(&home).unwrap());
        let client = crate::client_routes::Client::open(domain.clone(), &home, Arc::new(Mutex::new(None)));
        let state = domain.state().unwrap();
        assert!(gather(&client, &state, Lang::En, &["home".into()], false, false).await.pacing.is_some());
        domain.live.gate.poison();
        domain.live.machine_watch.poison();
        crate::log::poison(&client.live_snapshots.entries);
        crate::log::poison(&client.live_reads.0);
        crate::log::poison(&client.views_gauges);
        crate::log::poison(&client.gauges);
        domain.live.gate.poke();
        let live = gather(&client, &state, Lang::En, &["home".into()], false, false).await;
        assert!(live.pacing.is_some());
        assert!(client.live_snapshots.entries.lock().is_ok_and(|entries| !entries.is_empty()));
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

/// The Claude accounts' combined margin, with one part per account. Only a margin from a real
/// limit (every account at its limit, signed out, off or paid per use) is hot: the reserve and
/// today's slice rank the accounts but never hold work back.
fn margin_gauge(
    usage: &alondra_core::usage::Usage,
    margin: f64,
    week_reset: Option<String>,
    aged: impl Fn(String) -> Option<String>,
    lang: Lang,
    now: Millis,
) -> Gauge {
    let held = usage.none_can_work();
    let level = if held { Level::Hot } else if margin < LOW_MARGIN { Level::Warm } else { Level::Calm };
    let mut detail = if held {
        match lang {
            Lang::En => "Every Claude account is at its limit: new work waits".to_owned(),
            Lang::Es => "Todas las cuentas de Claude están en su límite: el trabajo nuevo espera".to_owned(),
        }
    } else {
        match lang {
            Lang::En => "Room left in the week, today's share and the 5-hour window; work only waits when every account is at its limit".to_owned(),
            Lang::Es => "Lo que queda de la semana, de la parte de hoy y de la ventana de 5 h; el trabajo solo espera cuando todas las cuentas llegan a su límite".to_owned(),
        }
    };
    if let Some(reset) = week_reset {
        detail.push_str(&match lang {
            Lang::En => format!(" · the week resets {reset}"),
            Lang::Es => format!(" · la semana se reinicia {reset}"),
        });
    }
    Gauge {
        id: "claude_margin".into(),
        label: match lang {
            Lang::En => "Claude · margin",
            Lang::Es => "Claude · margen",
        }
        .into(),
        value: margin,
        text: format!("{}%", (margin * 100.0).round()),
        detail: aged(detail),
        level,
        parts: usage.accounts.iter().map(|line| account_part(line, lang, now)).collect(),
    }
}

/// Below this margin the gauge warns, though work still starts.
const LOW_MARGIN: f64 = 0.15;

fn account_part(line: &alondra_core::usage::AccountLine, lang: Lang, now: Millis) -> alondra_core::views::GaugePart {
    use alondra_core::usage::AccountStand;
    let whole = |percent: f64| percent.round();
    let mut windows = Vec::new();
    if let Some(week) = line.week_percent {
        windows.push(match lang {
            Lang::En => format!("week {}%", whole(week)),
            Lang::Es => format!("semana {}%", whole(week)),
        });
    }
    if let Some(five) = line.five_hour_percent {
        windows.push(format!("5 h {}%", whole(five)));
    }
    if let Some((spent, share)) = line.today_spent.zip(line.today_share) {
        windows.push(match lang {
            Lang::En => format!("today {}/{}%", whole(spent), whole(share)),
            Lang::Es => format!("hoy {}/{}%", whole(spent), whole(share)),
        });
    }
    let windows = (!windows.is_empty()).then(|| windows.join(" · "));
    let margin = line.margin.unwrap_or(0.0);
    let (text, detail, level) = match &line.stand {
        AccountStand::CanWork => (
            line.margin.map_or_else(|| "—".to_owned(), |margin| format!("{}%", (margin * 100.0).round())),
            windows.or_else(|| Some(match lang {
                Lang::En => "Not measured yet".to_owned(),
                Lang::Es => "Aún sin medir".to_owned(),
            })),
            if line.margin.is_some_and(|margin| margin < LOW_MARGIN) { Level::Warm } else { Level::Calm },
        ),
        AccountStand::AtLimit { until } => {
            let limit = match (until, lang) {
                (Some(until), Lang::En) => format!("At its limit; back {}", reset_time::five_hour_reset(*until, now, reset_time::local_offset_secs, lang)),
                (Some(until), Lang::Es) => format!("En su límite; vuelve {}", reset_time::five_hour_reset(*until, now, reset_time::local_offset_secs, lang)),
                (None, Lang::En) => "At its limit".to_owned(),
                (None, Lang::Es) => "En su límite".to_owned(),
            };
            (
                match lang { Lang::En => "limit", Lang::Es => "límite" }.to_owned(),
                Some(windows.map_or_else(|| limit.clone(), |windows| format!("{limit} · {windows}"))),
                Level::Hot,
            )
        }
        AccountStand::SignedOut => (
            match lang { Lang::En => "signed out", Lang::Es => "sin sesión" }.to_owned(),
            Some(match lang {
                Lang::En => format!("Sign in on the Mac: alondra accounts login \"{}\"", line.name),
                Lang::Es => format!("Inicia sesión en el Mac: alondra accounts login \"{}\"", line.name),
            }),
            Level::Hot,
        ),
        AccountStand::Disabled => (match lang { Lang::En => "off", Lang::Es => "desactivada" }.to_owned(), None, Level::Calm),
        AccountStand::PayPerUse => (
            match lang { Lang::En => "per use", Lang::Es => "por uso" }.to_owned(),
            Some(match lang {
                Lang::En => "Paid per use: only requests that choose it run on it".to_owned(),
                Lang::Es => "De pago por uso: solo la usan las peticiones que la eligen".to_owned(),
            }),
            Level::Calm,
        ),
    };
    alondra_core::views::GaugePart { label: line.name.clone(), value: margin, text, detail, level }
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
        if let Some(margin) = usage.margin {
            let week_reset = (usage.week_resets_at > now).then_some(week_reset);
            out.push(margin_gauge(usage, margin, week_reset, &aged, lang, now));
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
                parts: vec![],
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
            parts: vec![],
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
        parts: vec![],
    });
    if machine.disk_total > 0 {
        let usable = machine.disk_available.saturating_add(machine.disk_purgeable);
        let used = machine.disk_total.saturating_sub(usable);
        let ratio = used as f64 / machine.disk_total as f64;
        let free = usable as f64 / 1_000_000_000.0;
        let purgeable = machine.disk_purgeable as f64 / 1_000_000_000.0;
        let shown = |text: String, purgeable_text: String| if machine.disk_purgeable >= 1_000_000_000 { format!("{text} ({purgeable_text})") } else { text };
        out.push(Gauge {
            id: "disk".into(),
            label: match lang {
                Lang::En => "Disk",
                Lang::Es => "Disco",
            }
            .into(),
            value: ratio,
            text: match lang {
                Lang::En => shown(format!("{free:.0} GB free"), format!("{purgeable:.0} GB purgeable")),
                Lang::Es => shown(format!("{free:.0} GB libres"), format!("{purgeable:.0} GB purgables")),
            },
            detail: None,
            level: if ratio >= 0.95 {
                Level::Hot
            } else if ratio >= 0.85 {
                Level::Warm
            } else {
                Level::Calm
            },
            parts: vec![],
        });
    }
    out
}

fn run_state(started_at: Millis, end: Option<crate::node_work::End>) -> alondra_core::views::RunState {
    match end {
        None => alondra_core::views::RunState::Running,
        Some(end) => alondra_core::views::RunState::Ended { outcome: end.outcome, ran_secs: end.at.saturating_sub(started_at).max(0) as u64 / 1000 },
    }
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
            text: format!("{}%", (value * 100.0).round()), detail: None, level, parts: vec![] };
        assert!(same_visible_gauges(&[memory(0.50, Level::Calm)], &[memory(0.51, Level::Calm)]));
        assert!(same_visible_gauges(&[memory(0.50, Level::Calm)], &[memory(0.59, Level::Calm)]));
        assert!(!same_visible_gauges(&[memory(0.50, Level::Calm)], &[memory(0.61, Level::Calm)]));
        assert!(!same_visible_gauges(&[memory(0.69, Level::Calm)], &[memory(0.71, Level::Warm)]));
    }

    #[test]
    fn disk_wakes_clients_only_past_its_step_or_on_a_new_level() {
        let disk = |value: f64, text: &str, level: Level| Gauge { id: "disk".into(), label: "Disk".into(), value,
            text: text.into(), detail: None, level, parts: vec![] };
        assert!(same_visible_gauges(&[disk(0.880, "58 GB free", Level::Warm)], &[disk(0.881, "57 GB free", Level::Warm)]));
        assert!(!same_visible_gauges(&[disk(0.880, "58 GB free", Level::Warm)], &[disk(0.901, "48 GB free", Level::Warm)]));
        assert!(!same_visible_gauges(&[disk(0.849, "70 GB free", Level::Calm)], &[disk(0.851, "69 GB free", Level::Warm)]));
    }

    #[test]
    fn every_claude_reading_change_wakes_clients() {
        let week = |text: &str, detail: &str| Gauge { id: "claude_margin".into(), label: "Claude · margin".into(),
            value: 0.83, text: text.into(), detail: Some(detail.into()), level: Level::Calm, parts: vec![] };
        assert!(same_visible_gauges(&[week("83%", "resets Monday")], &[week("83%", "resets Monday")]));
        assert!(!same_visible_gauges(&[week("83%", "resets Monday")], &[week("84%", "resets Monday")]));
        assert!(!same_visible_gauges(&[week("83%", "resets Monday")], &[week("83%", "resets Monday · measured 2 h ago")]));
    }

    #[test]
    fn a_minute_of_ram_and_cpu_wiggle_keeps_the_published_set() {
        let machine = |memory_free: u32, cpu: f32| alondra_core::Machine { at: 0, memory_total: 100,
            memory_available: 50, memory_free_percent: Some(memory_free), memory_pressure: None, compressor: None, load: [0.0; 3], cpus: 8,
            cpu_usage: cpu, disk_total: 1_000, disk_available: 120, disk_purgeable: 0, unreliable: false };
        let usage = |week: f64| alondra_core::usage::Usage { week_percent: week, week_resets_at: 0,
            today_spent: None, today_share: None, per_day_left: 0.0, over_pace: false, five_hour_percent: Some(10.0),
            five_hour_resets_at: Some(0), measured_at: 0, margin: Some(1.0 - week / 100.0), accounts: vec![] };
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
        assert_eq!(reading.iter().find(|gauge| gauge.id == "claude_margin").unwrap().text, "16%");
        assert_eq!(reading.iter().find(|gauge| gauge.id == "memory").unwrap().text, "42%",
            "a published set carries the current machine readings along with the new Claude one");
    }

    #[test]
    fn machine_gauges_measure_cpu_after_memory_like_the_header_did() {
        let mut machine = alondra_core::Machine { at: 0, memory_total: 100, memory_available: 50,
            memory_free_percent: Some(50), memory_pressure: None, compressor: None, load: [0.0; 3], cpus: 8, cpu_usage: 34.4,
            disk_total: 100, disk_available: 50, disk_purgeable: 0, unreliable: false };
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
    fn the_disk_gauge_counts_what_macos_can_purge_and_names_it_apart() {
        const GB: u64 = 1_000_000_000;
        let mut machine = alondra_core::Machine { disk_total: 500 * GB, disk_available: 10_800_000_000,
            disk_purgeable: 16_600_000_000, ..Default::default() };
        let disk = |machine: &alondra_core::Machine, lang: Lang| gauges(machine, None, lang, 0).into_iter().find(|gauge| gauge.id == "disk").unwrap();
        let english = disk(&machine, Lang::En);
        assert_eq!((english.text.as_str(), english.level), ("27 GB free (17 GB purgeable)", Level::Warm));
        assert_eq!(english.value, 1.0 - 27_400_000_000.0 / 500e9);
        assert_eq!(disk(&machine, Lang::Es).text, "27 GB libres (17 GB purgables)");
        machine.disk_purgeable = 400_000_000;
        assert_eq!(disk(&machine, Lang::Es).text, "11 GB libres");
    }

    #[test]
    fn cpu_wakes_clients_only_on_a_meaningful_change() {
        let cpu = |value: f64, level: Level| Gauge { id: "cpu".into(), label: "CPU".into(), value,
            text: format!("{}%", (value * 100.0).round()), detail: None, level, parts: vec![] };
        assert!(same_visible_gauges(&[cpu(0.34, Level::Calm)], &[cpu(0.41, Level::Calm)]));
        assert!(!same_visible_gauges(&[cpu(0.34, Level::Calm)], &[cpu(0.45, Level::Calm)]));
        assert!(!same_visible_gauges(&[cpu(0.84, Level::Calm)], &[cpu(0.86, Level::Warm)]));
    }

    fn line(name: &str, stand: alondra_core::usage::AccountStand, margin: Option<f64>) -> alondra_core::usage::AccountLine {
        alondra_core::usage::AccountLine { name: name.into(), stand, margin, week_percent: Some(30.0), five_hour_percent: Some(10.0),
            today_spent: Some(2.0), today_share: Some(15.0) }
    }

    fn at_rest() -> alondra_core::Machine {
        alondra_core::Machine { at: 0, memory_total: 100, memory_available: 50,
            memory_free_percent: Some(50), memory_pressure: None, compressor: None, load: [0.0; 3], cpus: 8, cpu_usage: 0.0,
            disk_total: 100, disk_available: 50, disk_purgeable: 0, unreliable: false }
    }

    #[test]
    fn the_margin_replaces_the_week_and_today_with_one_part_per_account() {
        use alondra_core::usage::AccountStand;
        let usage = alondra_core::usage::Usage { week_percent: 53.0, week_resets_at: 1_790_553_600_000,
            today_spent: Some(41.0), today_share: Some(18.0), over_pace: true, margin: Some(0.35),
            five_hour_percent: Some(10.0), five_hour_resets_at: Some(1_790_959_200_000),
            accounts: vec![line("Principal", AccountStand::CanWork, Some(0.7)), line("Trabajo", AccountStand::CanWork, Some(0.0)),
                line("Facturas", AccountStand::PayPerUse, None)],
            ..Default::default() };
        let result = gauges(&at_rest(), Some(&usage), Lang::En, 0);
        assert_eq!(result.iter().map(|gauge| gauge.id.as_str()).collect::<Vec<_>>(), ["claude_margin", "claude_five_hours", "memory", "cpu", "disk"]);
        let margin = &result[0];
        assert_eq!((margin.label.as_str(), margin.text.as_str(), margin.level), ("Claude · margin", "35%", Level::Calm),
            "over today's share and with one account past its reserve, work still starts: the margin does not say otherwise");
        assert!(margin.detail.as_deref().unwrap().contains("work only waits when every account is at its limit"));
        assert!(!margin.detail.as_deref().unwrap().contains("2026-"), "reset dates are human, not ISO");
        let parts: Vec<_> = margin.parts.iter().map(|part| (part.label.as_str(), part.text.as_str(), part.level)).collect();
        assert_eq!(parts, [("Principal", "70%", Level::Calm), ("Trabajo", "0%", Level::Warm), ("Facturas", "per use", Level::Calm)]);
        assert_eq!(margin.parts[0].detail.as_deref(), Some("week 30% · 5 h 10% · today 2/15%"));
        let spanish = gauges(&at_rest(), Some(&usage), Lang::Es, 0);
        assert_eq!((spanish[0].label.as_str(), spanish[0].parts[0].detail.as_deref()), ("Claude · margen", Some("semana 30% · 5 h 10% · hoy 2/15%")));
        assert_eq!(spanish[0].parts[2].detail.as_deref(), Some("De pago por uso: solo la usan las peticiones que la eligen"));
        assert_eq!((result[1].text.as_str(), result[1].detail.as_deref().map(|d| d.starts_with("5-hour limit resets "))), ("10%", Some(true)));
    }

    #[test]
    fn the_margin_is_hot_only_while_every_account_is_at_its_real_limit() {
        use alondra_core::usage::AccountStand;
        let usage = |accounts| alondra_core::usage::Usage { margin: Some(0.0), accounts, ..Default::default() };
        let reserve_only = usage(vec![line("Principal", AccountStand::CanWork, Some(0.0))]);
        let margin = gauges(&at_rest(), Some(&reserve_only), Lang::En, 0).remove(0);
        assert_eq!(margin.level, Level::Warm, "an account past its reserve or today's share still takes work");
        let held = usage(vec![line("Principal", AccountStand::AtLimit { until: None }, Some(0.0)), line("Trabajo", AccountStand::SignedOut, Some(0.0))]);
        let margin = gauges(&at_rest(), Some(&held), Lang::Es, 0).remove(0);
        assert_eq!(margin.level, Level::Hot);
        assert!(margin.detail.as_deref().unwrap().starts_with("Todas las cuentas de Claude están en su límite"));
        let parts: Vec<_> = margin.parts.iter().map(|part| (part.text.as_str(), part.detail.clone().unwrap())).collect();
        assert_eq!(parts, [("límite", "En su límite · semana 30% · 5 h 10% · hoy 2/15%".to_owned()),
            ("sin sesión", "Inicia sesión en el Mac: alondra accounts login \"Trabajo\"".to_owned())]);
    }

    #[test]
    fn an_old_reading_says_how_old_it_is_in_both_languages() {
        let usage = alondra_core::usage::Usage { week_percent: 53.0, week_resets_at: 1_790_553_600_000,
            today_spent: Some(3.0), today_share: Some(10.0), five_hour_percent: Some(10.0), margin: Some(0.5),
            five_hour_resets_at: Some(1_790_959_200_000), measured_at: 1_000, ..Default::default() };
        let two_hours = 1_000 + 2 * 3_600_000;
        let details = |lang, now| gauges(&at_rest(), Some(&usage), lang, now).iter().take(2)
            .map(|gauge| gauge.detail.clone().unwrap()).collect::<Vec<_>>();
        assert!(details(Lang::En, 1_000 + 3_600_000).iter().all(|detail| !detail.contains("measured")), "within the hour it is current");
        assert!(details(Lang::En, two_hours).iter().all(|detail| detail.ends_with(" · measured 2 h ago")));
        assert!(details(Lang::Es, two_hours).iter().all(|detail| detail.ends_with(" · medido hace 2 h")));
        assert!(details(Lang::Es, 1_000 + 3 * 86_400_000)[0].ends_with("medido hace 3 d"));
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
