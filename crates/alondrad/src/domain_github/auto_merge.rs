//! 8ccd131's auto-merge on domain v2. For a Task whose Request the user asked to auto-merge,
//! the daemon merges the presented PR once its checks pass and GitHub can merge it: it records
//! the merge as the system's authorization and claim, merges with `gh`, and types into the
//! Agent's session the merge notice or what only the Agent can fix. A Request without
//! auto-merge keeps the user's own authorization.
use super::{PrFacts, PrObserver, execute_system};
use crate::domain_runtime::DomainRuntime;
use crate::domain_store::StoreError;
use alondra_core::automerge::{NO_CHECKS_GRACE, Obstacle};
use alondra_core::domain_v2::{
    AUTO_MERGE_USER, AgentId, AuthorizationId, Command, DeliveryTarget, DomainState, InteractionPayload, MergeAttemptState,
    MergeMethod, PrId, PrState, PullRequest, TaskId,
};
use alondra_core::pr::CheckState;
use std::collections::{HashMap, HashSet};
use std::future::Future;
use std::pin::Pin;

/// A refused merge of the same head waits this long, doubling with each refusal up to the most.
const MERGE_RETRY: i64 = 5 * 60 * 1000;
const MERGE_RETRY_MAX: i64 = 2 * 3600 * 1000;

/// What the pass remembers between rounds, in memory as 8ccd131 did: when each head was first
/// seen, what each Agent was already told about a head, and refused merges waiting to retry.
#[derive(Default)]
pub(crate) struct AutoMerging {
    seen: HashMap<(PrId, String), i64>,
    told: HashSet<(PrId, String, String)>,
    failed: HashMap<(PrId, String), (i64, u32)>,
}

fn wait(times: u32) -> i64 {
    MERGE_RETRY.saturating_mul(1 << times.saturating_sub(1).min(16)).min(MERGE_RETRY_MAX)
}

type Told<'a> = Pin<Box<dyn Future<Output = ()> + Send + 'a>>;

/// Types a line into the Agent's session about its Task.
pub(crate) trait Tell: Send + Sync {
    fn tell<'a>(&'a self, domain: &'a DomainRuntime, agent: &'a AgentId, task: &'a TaskId, text: String) -> Told<'a>;
}

pub(crate) struct Sessions;

impl Tell for Sessions {
    fn tell<'a>(&'a self, domain: &'a DomainRuntime, agent: &'a AgentId, task: &'a TaskId, text: String) -> Told<'a> {
        Box::pin(async move {
            let Ok(body) = serde_json::from_value(serde_json::json!({"text": text, "task_id": task})) else { return };
            let terminal = std::sync::Arc::new(crate::domain_transport::Tmux);
            if let Err(error) = crate::domain_messages::deliver(domain, domain.home(), &alondra_core::domain_v2::Actor::System,
                "Alondra", agent, body, &crate::domain_messages::ClaudeAgents, &terminal).await {
                log!("could not tell {agent} about its PR: {}", error.message);
            }
        })
    }
}

fn url(pr: &PullRequest) -> String {
    format!("https://{}/{}/pull/{}", pr.host, pr.repository, pr.number)
}

fn label(method: MergeMethod) -> &'static str {
    match method {
        MergeMethod::Merge => "merge commit",
        MergeMethod::Squash => "squash",
        MergeMethod::Rebase => "rebase",
    }
}

/// What the Agent of an auto-merge Task hears when only it can move its PR on.
fn obstacle_text(pr: &PullRequest, obstacle: &Obstacle) -> String {
    let url = url(pr);
    match obstacle {
        Obstacle::ChecksFailed(names) => format!("[Alondra] The checks of {url} failed ({}). Fix them and push: Alondra merges it once they pass.", names.join(", ")),
        Obstacle::Conflicts => format!("[Alondra] {url} conflicts with {}: bring {} into the branch, resolve the conflicts and push. Alondra merges it once GitHub can.", pr.base, pr.base),
        Obstacle::Draft => format!("[Alondra] {url} is a draft. Mark it ready (gh pr ready) when it is: Alondra merges it once its checks pass."),
    }
}

enum Step {
    Waiting,
    Blocked(Obstacle),
    Ready,
}

/// Where an open PR of an auto-merge Task stands, as 8ccd131 judged it. `presented` is whether
/// the Agent presented this head as its result: the domain merges only a presented head.
fn step(facts: &PrFacts, first_seen: i64, now: i64, presented: bool) -> Step {
    let failed: Vec<String> = facts.checks.iter().filter(|c| c.state == CheckState::Failed).map(|c| c.name.clone()).collect();
    if facts.draft {
        Step::Blocked(Obstacle::Draft)
    } else if facts.mergeable.as_deref() == Some("CONFLICTING") {
        Step::Blocked(Obstacle::Conflicts)
    } else if !failed.is_empty() {
        Step::Blocked(Obstacle::ChecksFailed(failed))
    } else if facts.checks.iter().any(|c| c.state == CheckState::Running) {
        Step::Waiting
    } else if facts.checks.is_empty() && now - first_seen < NO_CHECKS_GRACE {
        Step::Waiting
    } else if facts.mergeable.as_deref() != Some("MERGEABLE") {
        Step::Waiting
    } else if !presented {
        Step::Waiting
    } else {
        Step::Ready
    }
}

/// The Task's current final presentation, when it presents this PR at this head.
fn presentation(state: &DomainState, pr: &PullRequest, head: &str) -> Option<alondra_core::domain_v2::InteractionId> {
    let task = state.tasks.get(&pr.task_id)?;
    let id = task.final_presentation.as_ref()?;
    let InteractionPayload::Delivery { targets, .. } = &state.interactions.get(id)?.payload else { return None };
    targets.contains(&DeliveryTarget::PullRequest { pr_id: pr.id.clone(), head: head.to_owned() }).then(|| id.clone())
}

fn refused(result: Result<(), StoreError>, what: &str) -> anyhow::Result<bool> {
    match result {
        Ok(()) => Ok(false),
        Err(StoreError::Domain(error)) => {
            log!("auto-merge {what} refused: {error}");
            Ok(true)
        }
        Err(StoreError::Storage(error)) => Err(error),
    }
}

/// One pass over the open PRs of auto-merge Tasks.
pub(crate) async fn run_once(domain: &DomainRuntime, observer: &impl PrObserver, merging: &mut AutoMerging,
    tell: &impl Tell, now: i64) -> anyhow::Result<()> {
    let state = domain.state()?;
    let open: Vec<PullRequest> = state.pull_requests.values().filter(|pr| pr.state == PrState::Open).cloned().collect();
    let heads: HashSet<(PrId, String)> = open.iter().map(|pr| (pr.id.clone(), pr.head.clone())).collect();
    merging.seen.retain(|key, _| heads.contains(key));
    merging.failed.retain(|key, _| heads.contains(key));
    for pr in open {
        let Some(task) = state.tasks.get(&pr.task_id).filter(|task| !task.state.terminal()) else { continue };
        let Some(agent) = task.agent_id.clone() else { continue };
        if !domain.request_launch(&task.request_id)?.is_some_and(|launch| launch.options.auto_merge) {
            continue;
        }
        let facts = match observer.observe(&pr.host, &pr.repository, pr.number).await {
            Ok(facts) if facts.state == PrState::Open && facts.head == pr.head => facts,
            Ok(_) => continue,
            Err(reason) => {
                log!("GitHub could not report {} for auto-merge: {reason}", url(&pr));
                continue;
            }
        };
        let key = (pr.id.clone(), pr.head.clone());
        let first_seen = *merging.seen.entry(key.clone()).or_insert(now);
        let presented = presentation(&state, &pr, &pr.head);
        let method = pr.allowed_methods.first().copied().unwrap_or(MergeMethod::Merge);
        match step(&facts, first_seen, now, presented.is_some()) {
            Step::Waiting => {}
            Step::Blocked(obstacle) => {
                if merging.told.insert((key.0.clone(), key.1.clone(), format!("{obstacle:?}"))) {
                    tell.tell(domain, &agent, &task.id, obstacle_text(&pr, &obstacle)).await;
                }
            }
            Step::Ready => {
                if merging.failed.get(&key).is_some_and(|(at, times)| now - at < wait(*times)) {
                    continue;
                }
                let Some(presentation_id) = presented else { continue };
                let reusable = state.merge_authorizations.values().find(|a| a.pr_id == pr.id && a.head == pr.head
                    && a.valid && a.user_id == AUTO_MERGE_USER && a.attempt == MergeAttemptState::Unclaimed).map(|a| a.id.clone());
                let authorization_id = match reusable {
                    Some(id) => id,
                    None => {
                        let id = AuthorizationId(format!("auto-merge-{}", crate::auth::random_hex(8)));
                        if refused(execute_system(domain, Command::AuthorizeMerge { authorization_id: id.clone(), presentation_id,
                            pr_id: pr.id.clone(), expected_head: pr.head.clone(), method }), "authorization")? {
                            continue;
                        }
                        id
                    }
                };
                if refused(execute_system(domain, Command::ClaimMergeAuthorization { authorization_id: authorization_id.clone(),
                    remote_head: facts.head.clone(), remote_base: facts.base.clone(), base_verified: true }), "claim")? {
                    continue;
                }
                match observer.merge(&pr.host, &pr.repository, pr.number, method, &pr.head).await {
                    Ok(()) => {
                        log!("{} merged by Alondra: its checks passed", url(&pr));
                        merging.failed.remove(&key);
                        tell.tell(domain, &agent, &task.id, format!("[Alondra] Alondra merged {} ({}) once its checks passed.", url(&pr), label(method))).await;
                    }
                    Err(reason) => {
                        refused(execute_system(domain, Command::ObserveMergeAttempt { authorization_id, state: MergeAttemptState::Failed }), "failure")?;
                        let times = merging.failed.get(&key).map_or(0, |(_, times)| *times) + 1;
                        merging.failed.insert(key.clone(), (now, times));
                        log!("{} not merged by Alondra: {reason}", url(&pr));
                        if merging.told.insert((key.0.clone(), key.1.clone(), "merge".into())) {
                            tell.tell(domain, &agent, &task.id, format!("[Alondra] Alondra could not merge {}: {reason}. It tries again in {} min, and at once after a push.",
                                url(&pr), wait(times) / 60_000)).await;
                        }
                    }
                }
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests;
