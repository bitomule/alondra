//! `gh` itself: running it, reading what it answers, and observing a pull request with it.
use alondra_core::clock;
use alondra_core::pr::CheckState;
use alondra_domain_rules::{MergeMethod, PrChecks, PrMergeable, PrState};
use alondra_engine_ports::github::{Closed, GhFailure, HttpAnswer, Observed, PrFacts, PrObserver, pr_url};
use serde_json::Value;
use std::path::PathBuf;
use std::time::Duration;

/// How long a delivery waits for GitHub to name one branch's open PR.
const BRANCH_PR_LIMIT: Duration = Duration::from_secs(3);

const OBSERVED_FIELDS: &str = "state,headRefOid,baseRefName,mergeable,mergeStateStatus,statusCheckRollup";

/// GitHub through the Mac's `gh`, and a signed image link through a request without credentials.
#[derive(Clone)]
pub struct GhCli {
    program: PathBuf,
    pub(crate) branch_pr_limit: Duration,
    pub(crate) web: std::sync::OnceLock<reqwest::Client>,
}

impl Default for GhCli {
    fn default() -> Self {
        Self::new()
    }
}

impl GhCli {
    pub fn new() -> Self {
        Self::at("gh".into())
    }
    /// A `gh` at `program`, as a test scripts it.
    pub fn at(program: PathBuf) -> Self {
        Self { program, branch_pr_limit: BRANCH_PR_LIMIT, web: std::sync::OnceLock::new() }
    }
    pub fn with_branch_pr_limit(mut self, limit: Duration) -> Self {
        self.branch_pr_limit = limit;
        self
    }
    pub fn program(&self) -> &std::path::Path {
        &self.program
    }
}

impl PrObserver for GhCli {
    fn observe<'a>(&'a self, host: &'a str, repository: &'a str, number: u64) -> Observed<'a> {
        Box::pin(async move {
            let url = format!("https://{}/{repository}/pull/{number}", alondra_domain_rules::normalize_pr_host(host));
            let run = tokio::process::Command::new(&self.program)
                .args(["pr", "view", &url, "--json", OBSERVED_FIELDS])
                .kill_on_drop(true)
                .output();
            let output = match clock::timeout(Duration::from_secs(30), run).await {
                Ok(Ok(output)) if output.status.success() => output,
                Ok(Ok(output)) => return Err(String::from_utf8_lossy(&output.stderr).trim().to_owned()),
                Ok(Err(error)) => return Err(error.to_string()),
                Err(_) => return Err("gh pr view did not finish within 30 s".into()),
            };
            let value: Value = serde_json::from_slice(&output.stdout).map_err(|e| e.to_string())?;
            let mut facts = parse(&value)?;
            facts.allowed_methods = self.repository_methods(host, repository).await;
            Ok(facts)
        })
    }
    fn close<'a>(&'a self, host: &'a str, repository: &'a str, number: u64, comment: &'a str) -> Closed<'a> {
        Box::pin(async move {
            let url = pr_url(host, repository, number);
            self.run(&["pr", "close", &url, "--comment", comment], None, Duration::from_secs(30)).await
                .map(drop).map_err(|failure| failure.to_string())
        })
    }
}

impl GhCli {
    /// The merge methods the repository settings allow, squash first; empty when GitHub does not answer.
    async fn repository_methods(&self, host: &str, repository: &str) -> Vec<MergeMethod> {
        let url = format!("https://{}/{repository}", alondra_domain_rules::normalize_pr_host(host));
        let args = ["repo", "view", url.as_str(), "--json", "mergeCommitAllowed,squashMergeAllowed,rebaseMergeAllowed"];
        let Ok(out) = self.run(&args, None, Duration::from_secs(30)).await else { return vec![] };
        let Ok(value) = serde_json::from_slice::<Value>(&out) else { return vec![] };
        parse_methods(&value)
    }

    pub async fn run(&self, args: &[&str], dir: Option<&std::path::Path>, limit: Duration) -> Result<Vec<u8>, GhFailure> {
        let output = self.output(args, dir, limit).await?;
        if output.status.success() {
            return Ok(output.stdout);
        }
        Err(GhFailure::Exit(output.status.code(), String::from_utf8_lossy(&output.stderr).trim().to_owned()))
    }

    /// One `gh api` call with `--include`: GitHub's HTTP status, headers and body, whatever the status.
    /// A failure is only `gh` failing before GitHub answered.
    pub async fn api(&self, args: &[&str], limit: Duration) -> Result<HttpAnswer, GhFailure> {
        let args: Vec<&str> = ["api", "--include"].into_iter().chain(args.iter().copied()).collect();
        let output = self.output(&args, None, limit).await?;
        match HttpAnswer::parse(&output.stdout) {
            Some(answer) => Ok(answer),
            None if output.status.success() => Err(GhFailure::Unreadable("gh api answered without an HTTP status".into())),
            None => Err(GhFailure::Exit(output.status.code(), String::from_utf8_lossy(&output.stderr).trim().to_owned())),
        }
    }

    async fn output(&self, args: &[&str], dir: Option<&std::path::Path>, limit: Duration) -> Result<std::process::Output, GhFailure> {
        let mut command = tokio::process::Command::new(&self.program);
        command.args(args).kill_on_drop(true);
        if let Some(dir) = dir {
            command.current_dir(dir);
        }
        match clock::timeout(limit, command.output()).await {
            Ok(Ok(output)) => Ok(output),
            Ok(Err(error)) => Err(GhFailure::Launch(error.kind(), error.to_string())),
            Err(_) => Err(GhFailure::TimedOut(format!("gh {} did not finish within {} s", args.first().copied().unwrap_or_default(), limit.as_secs()))),
        }
    }

    /// The open PR GitHub has for the branch checked out in `dir`, as host, `owner/repo` and number.
    pub async fn branch_pr_within(&self, dir: &std::path::Path, limit: Duration) -> Option<(String, String, u64)> {
        let out = self.run(&["pr", "view", "--json", "url,state"], Some(dir), limit).await.ok()?;
        let value: Value = serde_json::from_slice(&out).ok()?;
        if value["state"].as_str() != Some("OPEN") {
            return None;
        }
        let (host, path) = value["url"].as_str()?.split_once("://")?.1.split_once('/')?;
        let (repository, number) = path.split_once("/pull/")?;
        Some((alondra_domain_rules::normalize_pr_host(host), repository.to_owned(), number.trim_end_matches('/').parse().ok()?))
    }
}


fn parse_methods(value: &Value) -> Vec<MergeMethod> {
    [
        ("squashMergeAllowed", MergeMethod::Squash),
        ("mergeCommitAllowed", MergeMethod::Merge),
        ("rebaseMergeAllowed", MergeMethod::Rebase),
    ]
    .into_iter()
    .filter(|(key, _)| value[*key].as_bool() == Some(true))
    .map(|(_, method)| method)
    .collect()
}

pub fn parse(value: &Value) -> Result<PrFacts, String> {
    let head = value["headRefOid"].as_str()
        .filter(|head| head.len() == 40 && head.bytes().all(|byte| byte.is_ascii_hexdigit()))
        .ok_or("gh reported no PR head")?;
    let base = value["baseRefName"].as_str().filter(|base| !base.is_empty())
        .ok_or("gh reported no PR base")?;
    let state = match value["state"].as_str() {
        Some("OPEN") => PrState::Open,
        Some("CLOSED") => PrState::Closed,
        Some("MERGED") => PrState::Merged,
        other => return Err(format!("gh reported an unknown PR state {other:?}")),
    };
    Ok(PrFacts {
        head: head.to_owned(),
        base: base.to_owned(),
        merged_head: (state == PrState::Merged).then(|| head.to_owned()),
        state,
        allowed_methods: vec![],
        mergeable: mergeable(value),
        checks: checks(value),
    })
}

fn mergeable(value: &Value) -> PrMergeable {
    match (value["mergeable"].as_str(), value["mergeStateStatus"].as_str()) {
        (Some("CONFLICTING"), _) | (_, Some("DIRTY")) => PrMergeable::Conflicting,
        (Some("UNKNOWN") | None, _) | (_, Some("UNKNOWN")) => PrMergeable::Computing,
        (Some("MERGEABLE"), None | Some("CLEAN" | "HAS_HOOKS")) => PrMergeable::Clean,
        _ => PrMergeable::Other,
    }
}

/// The latest run of each check: any failure first, then any that needs someone's approval to run,
/// then any still running.
fn checks(value: &Value) -> PrChecks {
    let entries = value["statusCheckRollup"].as_array().map(|rollup| crate::prs::latest_entries(rollup)).unwrap_or_default();
    let (mut failed, mut approval, mut running, mut any) = (vec![], vec![], vec![], false);
    for entry in entries {
        let Some(check) = crate::prs::check(entry) else { continue };
        any = true;
        match check.state {
            _ if entry["conclusion"].as_str() == Some("ACTION_REQUIRED") => approval.push(check.name),
            CheckState::Failed => failed.push(check.name),
            CheckState::Running => running.push(check.name),
            CheckState::Passed | CheckState::Other => (),
        }
    }
    if !failed.is_empty() {
        PrChecks::Failed { names: failed }
    } else if !approval.is_empty() {
        PrChecks::ActionRequired { names: approval }
    } else if !running.is_empty() {
        PrChecks::Pending { names: running }
    } else if any {
        PrChecks::Passed
    } else {
        PrChecks::None
    }
}

#[cfg(test)]
#[path = "cli_tests.rs"]
mod tests;
