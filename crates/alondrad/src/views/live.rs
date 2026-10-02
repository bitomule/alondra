use alondra_core::domain_v2::{DomainState, PrState, PullRequest, TaskId, TaskState};
use alondra_core::views::{
    Gauge, InboxTime, Lang, Level, LiveActivity, LiveBackground, LiveHistoryStep, LiveNode, LiveOrchestrator,
    LiveCheck, LivePullRequest, LiveSnapshot, Notice, Tone,
};
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
    agent: &alondra_core::domain_v2::Agent,
    launches: &BTreeMap<alondra_core::domain_v2::RequestId, (Option<String>, Option<String>)>,
) -> Option<String> {
    state.tasks.values()
        .filter(|task| task.agent_id.as_ref() == Some(&agent.id) && !task.archived)
        .find_map(|task| launches.get(&task.request_id).and_then(|(project, _)| project.clone()))
        .or_else(|| agent.name.split_once('/').map(|(project, _)| project.to_owned()).filter(|project| !project.is_empty()))
}

fn node_label(state: alondra_core::NodeState, lang: Lang) -> String {
    match lang {
        Lang::En => state.label().to_owned(),
        Lang::Es => match state {
            alondra_core::NodeState::Working => "trabajando",
            alondra_core::NodeState::WaitingAnswer => "esperando respuesta",
            alondra_core::NodeState::YourTurn => "te toca",
            alondra_core::NodeState::WaitingMerge => "espera tu revisión",
            alondra_core::NodeState::Delivered => "entrega resultado",
            alondra_core::NodeState::WaitingMachine => "esperando máquina",
            alondra_core::NodeState::Done => "terminado",
            alondra_core::NodeState::Unclear => "no sé qué quiere",
            alondra_core::NodeState::Stalled => "parado",
        }.to_owned(),
    }
}

pub(crate) async fn gather(
    client: &crate::v2_client_routes::Client,
    state: &DomainState,
    lang: Lang,
    keys: &[String],
    allow_issue_refresh: bool,
    clock_only: bool,
) -> LiveSnapshot {
    let now = alondra_core::now_millis();
    let launches = client.domain.store.lock().expect("domain store poisoned")
        .request_launch_summaries().unwrap_or_default();
    let nodes = state
        .agents
        .values()
        .filter_map(|agent| {
            let session_id = agent.id.0.clone();
            let reported = client.domain.live.mods.session(&session_id).unwrap_or_default();
            let open_task = state.tasks.values().any(|task| task.agent_id.as_ref() == Some(&agent.id) && !task.archived
                && !matches!(task.state, TaskState::Done | TaskState::Closing | TaskState::Cancelled));
            let node = match client.domain.live.node(&session_id) {
                Some(node) => node,
                None if reported.started_at.is_some() || agent.connected || open_task => {
                    let mut node = alondra_core::Node::new(&session_id, now);
                    node.cwd = reported.cwd.clone();
                    node
                }
                None => return None,
            };
            let project = if node.cwd.is_some() || node.is_orchestrator {
                node.project()
            } else {
                launched_project(state, agent, &launches).unwrap_or_else(|| node.project())
            };
            let activity = if reported.connection.is_none() && (agent.connected || open_task) {
                Activity::Unreachable
            } else {
                reported.activity(now)
            };
            let (state_key, state_label) = match activity {
                Activity::Stalled => ("stalled", node_label(alondra_core::NodeState::Stalled, lang)),
                Activity::Working => ("busy", node_label(alondra_core::NodeState::Working, lang)),
                Activity::Idle | Activity::Unreachable if node.status == alondra_core::NodeStatus::WaitingMachine => {
                    ("waiting_machine", node_label(alondra_core::NodeState::WaitingMachine, lang))
                }
                Activity::Idle | Activity::Unreachable => {
                    let idle = if node.state == alondra_core::NodeState::Working { alondra_core::NodeState::Unclear } else { node.state };
                    ("free", node_label(idle, lang))
                }
            };
            let tool = reported.tool.as_ref().map(|tool| match &tool.summary {
                Some(summary) => format!("{}: {summary}", tool.name),
                None => tool.name.clone(),
            });
            let answer = reported.answer.as_ref().filter(|answer| !answer.text.trim().is_empty());
            let summary = tool.clone()
                .or_else(|| node.daemon_needs.clone())
                .or_else(|| node.daemon_detail.clone())
                .or_else(|| answer.map(|answer| answer.text.clone()))
                .map(|summary| summary.lines().next().unwrap_or("").chars().take(141).collect());
            let background = reported.background_at(now);
            let counts = [alondra_core::BackgroundKind::Agent, alondra_core::BackgroundKind::Shell,
                alondra_core::BackgroundKind::Other].into_iter().filter_map(|kind| {
                    let count = background.iter().filter(|item| item.kind == kind).count();
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
            let background_suspect = background.iter().any(|item| item.suspect);
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
                background: background.iter().map(|item| LiveBackground {
                    kind: serde_json::to_value(item.kind).ok().and_then(|value| value.as_str().map(str::to_owned)).unwrap_or_else(|| "other".into()),
                    label: item.label.clone(),
                    since: timestamp(item.started_at),
                    suspect: item.suspect,
                }).collect(),
                background_badge,
                background_suspect,
                working: matches!(activity, Activity::Working | Activity::Stalled),
                waiting: matches!(node.state, alondra_core::NodeState::WaitingAnswer |
                    alondra_core::NodeState::YourTurn | alondra_core::NodeState::WaitingMerge |
                    alondra_core::NodeState::WaitingMachine),
                state_since: Some(timestamp(node.state_since)),
                last_message: answer.map(|answer| answer.text.clone()),
                last_message_at: answer.map(|answer| timestamp(answer.at)),
                last_turn_aborted: answer.is_some_and(|answer| answer.aborted),
                tool: tool.map(|tool| tool.chars().take(141).collect()),
                context_percent: reported.context.as_ref().and_then(|context| context.percent).map(|percent| percent.round().clamp(0.0, 999.0) as u32),
                context_window: reported.context.as_ref().map(|context| context.window),
                cost_cents: reported.cost_usd.map(|usd| (usd * 100.0).round().max(0.0) as u64),
                unreachable: activity == Activity::Unreachable,
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
        let orders = state
            .projection()
            .inbox
            .into_iter()
            .flat_map(|root| {
                let root_order = (root.id, root.order);
                std::iter::once(root_order)
                    .chain(root.children.into_iter().map(|child| (child.id, child.order)))
            })
            .collect::<Vec<_>>();
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
            gauges(&machine, usage.as_ref(), Lang::En),
            gauges(&machine, usage.as_ref(), Lang::Es),
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
        .filter(|id| *id != "orchestrator")
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
    LiveSnapshot {
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
        search_matches,
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

async fn read_json(client: &crate::v2_client_routes::Client, path: &str) -> Option<Value> {
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
    client: &crate::v2_client_routes::Client,
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

fn search_source_revision(client: &crate::v2_client_routes::Client, state: &DomainState) -> String {
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

fn activity_source_revision(client: &crate::v2_client_routes::Client, id: &str) -> String {
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
    use alondra_core::domain_v2::{Actor, Command, CommandEnvelope};
    use std::{fs, sync::Arc};

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
    async fn activity_and_search_reads_are_reused_until_their_sources_change() {
        let home = std::env::temp_dir().join(format!("alondra-live-cache-{}", crate::auth::random_hex(8)));
        fs::create_dir(&home).unwrap();
        let domain = Arc::new(crate::domain_runtime::DomainRuntime::open(&home).unwrap());
        let client = crate::v2_client_routes::Client::open(domain.clone(), &home, Arc::new(std::sync::Mutex::new(None)));
        for id in ["a", "b"] {
            let state = domain.state().unwrap();
            domain.execute(&Actor::System, &CommandEnvelope {
                protocol_version: 2, data_epoch: state.data_epoch,
                command_id: format!("register-{id}").into(), expected_revision: state.revision,
                command: Command::RegisterAgent { agent_id: id.into(), name: format!("Agent {id}") },
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

/// CPU moves every sample, so it only counts as changed past `CPU_STEP` or on a new level;
/// otherwise every minute would wake every client.
const CPU_STEP: f64 = 0.10;

fn same_visible_gauges(left: &[Gauge], right: &[Gauge]) -> bool {
    left.len() == right.len()
        && left.iter().zip(right).all(|(a, b)| {
            let same_text = if a.id == "cpu" {
                (a.value - b.value).abs() < CPU_STEP
            } else {
                a.text == b.text
            };
            a.id == b.id && same_text && a.level == b.level && a.detail == b.detail
        })
}

fn gauges(
    machine: &alondra_core::Machine,
    usage: Option<&alondra_core::usage::Usage>,
    lang: Lang,
) -> Vec<Gauge> {
    let mut out = Vec::new();
    if let Some(usage) = usage {
        out.push(Gauge {
            id: "claude_week".into(),
            label: match lang {
                Lang::En => "Claude week",
                Lang::Es => "Claude semana",
            }
            .into(),
            value: usage.week_percent / 100.0,
            text: format!("{}%", usage.week_percent.round()),
            detail: Some(match lang {
                Lang::En => format!("Weekly limit resets {}", timestamp(usage.week_resets_at)),
                Lang::Es => format!("El límite semanal se reinicia {}", timestamp(usage.week_resets_at)),
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
                detail: Some(if usage.over_pace {
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
            out.push(Gauge {
                id: "claude_five_hours".into(),
                label: match lang {
                    Lang::En => "Claude 5 h",
                    Lang::Es => "Claude 5 h",
                }
                .into(),
                value: percent / 100.0,
                text: format!("{}%", percent.round()),
                detail: usage.five_hour_resets_at.map(|at| match lang {
                    Lang::En => format!("5-hour limit resets {}", timestamp(at)),
                    Lang::Es => format!("El límite de 5 horas se reinicia {}", timestamp(at)),
                }),
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
            detail: None,
            level: if free < 15 {
                Level::Hot
            } else if free < 30 {
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
        } else if cpu >= 85.0 {
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
            id: alondra_core::domain_v2::PrId(id.into()),
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
    fn gauges_change_only_when_displayed_text_or_level_changes() {
        let first = Gauge { id: "memory".into(), label: "RAM".into(), value: 0.501,
            text: "50%".into(), detail: None, level: Level::Calm };
        let mut sampled = first.clone();
        sampled.value = 0.504;
        assert!(same_visible_gauges(&[first.clone()], &[sampled.clone()]));
        sampled.text = "51%".into();
        assert!(!same_visible_gauges(&[first.clone()], &[sampled.clone()]));
        sampled.text = first.text.clone();
        sampled.level = Level::Warm;
        assert!(!same_visible_gauges(&[first], &[sampled]));
    }

    #[test]
    fn machine_gauges_measure_cpu_after_memory_like_the_header_did() {
        let mut machine = alondra_core::Machine { at: 0, memory_total: 100, memory_available: 50,
            memory_free_percent: Some(50), load: [0.0; 3], cpus: 8, cpu_usage: 34.4,
            disk_total: 100, disk_available: 50, unreliable: false };
        let english = gauges(&machine, None, Lang::En);
        assert_eq!(english.iter().map(|gauge| gauge.id.as_str()).collect::<Vec<_>>(), ["memory", "cpu", "disk"]);
        let cpu = &english[1];
        assert_eq!((cpu.label.as_str(), cpu.text.as_str(), cpu.level), ("CPU", "34%", Level::Calm));
        machine.cpu_usage = 90.0;
        assert_eq!(gauges(&machine, None, Lang::En)[1].level, Level::Warm);
        machine.cpu_usage = 10.0;
        machine.unreliable = true;
        assert_eq!(gauges(&machine, None, Lang::En)[1].level, Level::Hot);
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
            memory_free_percent: Some(50), load: [0.0; 3], cpus: 8, cpu_usage: 0.0,
            disk_total: 100, disk_available: 50, unreliable: false };
        let usage = alondra_core::usage::Usage { week_percent: 53.0, week_resets_at: 1_790_553_600_000,
            today_spent: Some(41.0), today_share: Some(18.0), over_pace: true,
            five_hour_percent: Some(10.0), five_hour_resets_at: Some(1_790_959_200_000), ..Default::default() };
        let result = gauges(&machine, Some(&usage), Lang::En);
        assert!(result[0].detail.as_deref().unwrap().contains("resets"));
        assert_eq!(result[1].text, "41/18%");
        assert!(result[1].label.contains('!'));
        assert!(result[1].detail.as_deref().unwrap().contains("faster"));
        assert_eq!((result[2].id.as_str(), result[2].text.as_str()), ("claude_five_hours", "10%"));
        assert!(result[2].detail.as_deref().unwrap().starts_with("5-hour limit resets 2026-10-02"));
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
