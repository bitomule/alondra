//! Find authored PRs whose work no longer has a live owner.
use alondra_core::clock;
use crate::domain_github::{AuthoredPr, PrObserver, execute_system};
use crate::domain_runtime::DomainRuntime;
use alondra_core::domain::{Command, DomainState, OpenPrPending, OpenedPr, PullRequest, TaskState};
use alondra_core::options::OptionsFile;
use anyhow::Result;
use std::collections::BTreeSet;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

const FIRST_SWEEP: Duration = Duration::from_secs(5 * 60);
const SWEEP_INTERVAL: Duration = Duration::from_secs(60 * 60);

fn identity(found: &AuthoredPr) -> Option<OpenedPr> {
    let repository = alondra_core::domain::normalize_pr_repository(&found.repository);
    let expected = format!("https://github.com/{repository}/pull/{}", found.number);
    (found.url.eq_ignore_ascii_case(&expected) && found.number > 0).then_some(OpenedPr {
        host: "github.com".into(), repository, number: found.number,
    })
}

fn registered<'a>(state: &'a DomainState, pr: &OpenedPr) -> Option<&'a PullRequest> {
    state.pull_requests.values().find(|p|
        p.host == pr.host && p.repository == pr.repository && p.number == pr.number)
}

fn excluded(state: &DomainState, pr: &OpenedPr) -> bool {
    state.pr_adoptions.contains(pr) || registered(state, pr).is_some_and(|p|
        state.tasks.get(&p.task_id).is_some_and(|t|
            !t.state.terminal() || matches!(t.open_pr, Some(OpenPrPending::Close { .. }))))
}

fn branch_excluded(state: &DomainState, branch: &str, prefixes: &[String]) -> bool {
    prefixes.iter().any(|prefix| branch.starts_with(prefix))
        || state.branch_has_live_task(branch)
}

fn checkout_repositories(root: &Path) -> BTreeSet<String> {
    let Ok(root) = root.canonicalize() else { return BTreeSet::new() };
    alondra_core::options::projects(&root).into_iter()
        .filter(|(_, path)| path.canonicalize().is_ok_and(|path| path.starts_with(&root)))
        .filter_map(|(_, path)| origin_repository(&path))
        .map(|repo| alondra_core::domain::normalize_pr_repository(&repo))
        .collect()
}

fn origin_repository(checkout: &Path) -> Option<String> {
    if let Some(repository) = alondra_core::issues::origin_of(checkout) { return Some(repository); }
    let gitfile = std::fs::read_to_string(checkout.join(".git")).ok()?;
    let gitdir = checkout.join(gitfile.trim().strip_prefix("gitdir: ")?);
    let common = std::fs::read_to_string(gitdir.join("commondir")).ok()?;
    let config = std::fs::read_to_string(gitdir.join(common.trim()).join("config")).ok()?;
    let mut origin = false;
    for line in config.lines().map(str::trim) {
        if line.starts_with('[') { origin = line == "[remote \"origin\"]"; }
        else if origin && let Some((key, value)) = line.split_once('=') && key.trim() == "url" {
            return alondra_core::issues::repo_of_remote(value.trim());
        }
    }
    None
}

pub(crate) async fn sweep(domain: &DomainRuntime, github: &dyn PrObserver, options: &OptionsFile, user_home: &Path) -> Result<()> {
    let repositories = checkout_repositories(&options.projects_root(user_home));
    let prs = github.authored_open().await.map_err(|e| anyhow::anyhow!(e))?;
    for found in prs {
        let Some(pr) = identity(&found) else { continue };
        if !repositories.contains(&pr.repository) || excluded(&*domain.state()?, &pr) { continue; }
        let branch = match github.head_branch(&found.url).await {
            Ok(branch) => branch,
            Err(error) => { log!("open PR sweep could not read the branch of {}: {error}", found.url); continue; }
        };
        let state = domain.state()?;
        if excluded(&state, &pr) || branch_excluded(&state, &branch, &options.pr_sweep_ignore_branch_prefixes) { continue; }
        let owner = registered(&state, &pr).map(|p| (p.task_id.clone(), p.id.clone()));
        let command = if let Some((task_id, pr_id)) = owner {
            let facts = match github.observe(&pr.host, &pr.repository, pr.number).await {
                Ok(facts) => facts,
                Err(error) => { log!("open PR sweep could not read {}: {error}", found.url); continue; }
            };
            if state.tasks.get(&task_id).is_some_and(|task| task.state == TaskState::Cancelled) {
                Command::ClosePrOfCancelledTask { task_id, pr_id, branch: branch, status: facts.status() }
            } else {
                Command::AdoptOpenPr { task_id, pr_id, branch: branch, status: facts.status() }
            }
        } else {
            Command::RequestPrAdoption { pr, branch: branch }
        };
        if let Err(error) = execute_system(domain, command) {
            log!("open PR sweep could not handle {}: {error}", found.url);
        }
    }
    Ok(())
}

/// Sweeps the open pull requests the user authored, reading GitHub through `github`.
pub async fn run(domain: Arc<DomainRuntime>, home: std::path::PathBuf, github: Arc<dyn PrObserver>) {
    let mut interval = clock::interval_at(clock::instant() + FIRST_SWEEP, SWEEP_INTERVAL);
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        interval.tick().await;
        let options = crate::domain_schedule_runner::options_at(&home);
        match options {
            Ok(options) => {
                if let Err(error) = sweep(&domain, &*github, &options, &crate::options::user_home()).await {
                    log!("open PR sweep failed: {error}");
                }
            }
            Err(error) => log!("open PR sweep could not read options: {error}"),
        }
    }
}

#[cfg(test)]
mod tests;
