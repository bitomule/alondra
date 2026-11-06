//! Find authored PRs whose work no longer has a live owner.
use crate::domain_github::{GhCli, PrObserver, execute_system};
use crate::domain_runtime::DomainRuntime;
use alondra_core::domain::{Command, DomainState, OpenPrPending, OpenedPr, PullRequest};
use alondra_core::options::OptionsFile;
use anyhow::{Context, Result};
use serde::Deserialize;
use std::collections::BTreeSet;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

const FIRST_SWEEP: Duration = Duration::from_secs(5 * 60);
const SWEEP_INTERVAL: Duration = Duration::from_secs(60 * 60);
const GH_LIMIT: Duration = Duration::from_secs(30);

#[derive(Deserialize)]
struct SearchPr {
    repository: Repository,
    number: u64,
    url: String,
}

#[derive(Deserialize)]
struct Repository {
    #[serde(rename = "nameWithOwner")]
    name_with_owner: String,
}

impl SearchPr {
    fn identity(&self) -> Option<OpenedPr> {
        let repository = alondra_core::domain::normalize_pr_repository(&self.repository.name_with_owner);
        let expected = format!("https://github.com/{repository}/pull/{}", self.number);
        (self.url.eq_ignore_ascii_case(&expected) && self.number > 0).then_some(OpenedPr {
            host: "github.com".into(), repository, number: self.number,
        })
    }
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

pub(crate) async fn sweep(domain: &DomainRuntime, gh: &GhCli, options: &OptionsFile, user_home: &Path) -> Result<()> {
    let repositories = checkout_repositories(&options.projects_root(user_home));
    let output = gh.run(&["search", "prs", "--author", "@me", "--state", "open", "--limit", "1000", "--json", "repository,number,url"],
        None, GH_LIMIT).await.map_err(|e| anyhow::anyhow!(e.to_string()))?;
    let prs: Vec<SearchPr> = serde_json::from_slice(&output).context("decoding the open PR search")?;
    for found in prs {
        let Some(pr) = found.identity() else { continue };
        if !repositories.contains(&pr.repository) || excluded(&*domain.state()?, &pr) { continue; }
        let output = match gh.run(&["pr", "view", &found.url, "--json", "headRefName"], None, GH_LIMIT).await {
            Ok(output) => output,
            Err(error) => { log!("open PR sweep could not read {}: {error}", found.url); continue; }
        };
        #[derive(Deserialize)]
        struct Branch { #[serde(rename = "headRefName")] name: String }
        let branch: Branch = match serde_json::from_slice(&output) {
            Ok(branch) => branch,
            Err(error) => { log!("open PR sweep could not decode the branch of {}: {error}", found.url); continue; }
        };
        let state = domain.state()?;
        if excluded(&state, &pr) || branch_excluded(&state, &branch.name, &options.pr_sweep_ignore_branch_prefixes) { continue; }
        let owner = registered(&state, &pr).map(|p| (p.task_id.clone(), p.id.clone()));
        let command = if let Some((task_id, pr_id)) = owner {
            let facts = match gh.observe(&pr.host, &pr.repository, pr.number).await {
                Ok(facts) => facts,
                Err(error) => { log!("open PR sweep could not read {}: {error}", found.url); continue; }
            };
            Command::AdoptOpenPr { task_id, pr_id, branch: branch.name, status: facts.status() }
        } else {
            Command::RequestPrAdoption { pr, branch: branch.name }
        };
        if let Err(error) = execute_system(domain, command) {
            log!("open PR sweep could not adopt {}: {error}", found.url);
        }
    }
    Ok(())
}

pub async fn run(domain: Arc<DomainRuntime>, home: std::path::PathBuf) {
    let mut interval = tokio::time::interval_at(tokio::time::Instant::now() + FIRST_SWEEP, SWEEP_INTERVAL);
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let gh = GhCli::new();
    loop {
        interval.tick().await;
        let options = crate::domain_schedule_runner::options_at(&home);
        match options {
            Ok(options) => {
                if let Err(error) = sweep(&domain, &gh, &options, &crate::options::user_home()).await {
                    log!("open PR sweep failed: {error}");
                }
            }
            Err(error) => log!("open PR sweep could not read options: {error}"),
        }
    }
}

#[cfg(test)]
mod tests;
