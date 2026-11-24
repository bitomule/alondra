//! Find the open PRs a task of this daemon registered whose work no longer has a live owner.
//! A PR no task registered is someone else's: the sweep never reads or adopts it.
use alondra_core::clock;
use crate::domain_github::execute_system;
use crate::pr_identity::{identify, read_start};
use alondra_engine_ports::github::{AuthoredPr, GitHub};
use crate::domain_runtime::DomainRuntime;
use alondra_domain_rules::{Command, DomainState, OpenPrPending, OpenedPr, PullRequest, TaskState};
use alondra_core::options::OptionsFile;
use anyhow::{Context, Result};
use std::collections::BTreeSet;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

const FIRST_SWEEP: Duration = Duration::from_secs(5 * 60);
const SWEEP_INTERVAL: Duration = Duration::from_secs(60 * 60);

fn identity(found: &AuthoredPr) -> Option<OpenedPr> {
    let repository = alondra_domain_rules::normalize_pr_repository(&found.repository);
    let expected = format!("https://github.com/{repository}/pull/{}", found.number);
    (found.url.eq_ignore_ascii_case(&expected) && found.number > 0).then_some(OpenedPr {
        host: "github.com".into(), repository, number: found.number,
    })
}

/// The tracked record GitHub's search names, by its current address or one GitHub confirmed was it.
fn registered<'a>(state: &'a DomainState, pr: &OpenedPr) -> Option<&'a PullRequest> {
    state.pull_requests.values().find(|p| p.is_tracked() && !p.identity.is_unresolvable() && p.answers_to(&pr.host, &pr.repository, pr.number))
}

/// The task that registered `pr`, when its work has no live owner and nothing is closing it.
fn ownerless<'a>(state: &'a DomainState, pr: &OpenedPr) -> Option<&'a PullRequest> {
    registered(state, pr).filter(|p| state.tasks.get(&p.task_id).is_some_and(|t|
        t.state.terminal() && !matches!(t.open_pr, Some(OpenPrPending::Close { .. }))))
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
        .map(|repo| alondra_domain_rules::normalize_pr_repository(&repo))
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

pub async fn sweep(domain: &DomainRuntime, github: &GitHub, options: &OptionsFile, user_home: &Path) -> Result<()> {
    let repositories = checkout_repositories(&options.projects_root(user_home));
    let prs = github.reads.authored_open_prs().await.map_err(|e| anyhow::anyhow!(e.to_string())).context("searching the open PRs")?;
    for found in prs {
        let Some(pr) = identity(&found) else { continue };
        if !repositories.contains(&pr.repository) || ownerless(&*domain.state()?, &pr).is_none() { continue; }
        let branch = match github.reads.head_ref(&found.url).await {
            Ok(branch) => branch,
            Err(error) => { log!("open PR sweep could not read the branch of {}: {error}", found.url); continue; }
        };
        let state = domain.state()?;
        let Some(known) = ownerless(&state, &pr).cloned() else { continue };
        if branch_excluded(&state, &branch, &options.pr_sweep_ignore_branch_prefixes) { continue; }
        let started = read_start();
        let facts = match github.observer.observe(&pr.host, &pr.repository, pr.number).await {
            Ok(facts) => facts,
            Err(error) => { log!("open PR sweep could not read {}: {error}", found.url); continue; }
        };
        let Some((known, _)) = identify(domain, &known, &pr.repository, started, Ok((facts.identity.as_ref(), &facts.head)))? else {
            log!("open PR sweep leaves {}: it is not the pull request Alondra has by that name", found.url);
            continue;
        };
        let state = domain.state()?;
        let (task_id, pr_id) = (known.task_id.clone(), known.id.clone());
        let command = if state.tasks.get(&task_id).is_some_and(|task| task.state == TaskState::Cancelled) {
            Command::ClosePrOfCancelledTask { task_id, pr_id, branch, status: facts.status() }
        } else {
            Command::AdoptOpenPr { task_id, pr_id, branch, status: facts.status() }
        };
        if let Err(error) = execute_system(domain, command) {
            log!("open PR sweep could not handle {}: {error}", found.url);
        }
    }
    Ok(())
}

pub async fn run(domain: Arc<DomainRuntime>, home: std::path::PathBuf, user_home: std::path::PathBuf) {
    let mut interval = clock::interval_at(clock::instant() + FIRST_SWEEP, SWEEP_INTERVAL);
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        interval.tick().await;
        let options = crate::options::options_at(&home);
        match options {
            Ok(options) => {
                let Some(github) = domain.ports().port::<GitHub>() else {
                    log!("open PR sweep has no GitHub to read");
                    continue;
                };
                if let Err(error) = sweep(&domain, &github, &options, &user_home).await {
                    log!("open PR sweep failed: {error}");
                }
            }
            Err(error) => log!("open PR sweep could not read options: {error}"),
        }
    }
}

#[cfg(test)]
mod tests;
