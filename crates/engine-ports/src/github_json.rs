//! What `gh --json` answers about a pull request, read without running anything: the facts the
//! observer judges, which pull request answered, and the card a PR's `gh pr view` makes. Any
//! GitHub that answers in `gh`'s JSON reads it the same way, the real one and a scenario's.
use super::{PrFacts, ReadIdentity};
use alondra_core::pr::{self as card, Check, CheckState, PullRequest as PrCard};
use alondra_domain_model::{PrChecks, PrMergeable, PrState, ProviderLocator, ProviderPrKey};
use serde_json::Value;

/// What `gh pr view --json` reports of a pull request, without the merge methods its repository
/// allows, which another read gives.
pub fn pr_facts(value: &Value) -> Result<PrFacts, String> {
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
        identity: identity(value),
    })
}

/// Which pull request a `gh` answer is about: `fullDatabaseId` is the key, which no move or
/// rename changes; the node `id` only finds it again, and GitHub may hand a new one; `url` is
/// where GitHub serves it now.
pub fn identity(value: &Value) -> Option<ReadIdentity> {
    let key = match &value["fullDatabaseId"] {
        Value::String(key) => key.trim().to_owned(),
        Value::Number(key) => key.to_string(),
        _ => return None,
    };
    if key.is_empty() || !key.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let locator = value["id"].as_str().map(str::trim).filter(|id| !id.is_empty())?;
    let (_, repository, _) = address(value["url"].as_str()?)?;
    let head_ref = value["headRefName"].as_str().unwrap_or_default().to_owned();
    Some(ReadIdentity { key: ProviderPrKey(key), locator: ProviderLocator(locator.to_owned()), repository, head_ref })
}

/// `https://<host>/<owner>/<repo>/pull/<number>` as host, lower-case `owner/repo` and number.
pub fn address(url: &str) -> Option<(String, String, u64)> {
    let (host, path) = url.split_once("://")?.1.split_once('/')?;
    let (repository, number) = path.split_once("/pull/")?;
    let number = number.trim_end_matches('/').parse().ok()?;
    (repository.split('/').count() == 2).then(|| (alondra_domain_model::normalize_pr_host(host), alondra_domain_model::normalize_pr_repository(repository), number))
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
    let entries = value["statusCheckRollup"].as_array().map(|rollup| latest_entries(rollup)).unwrap_or_default();
    let (mut failed, mut approval, mut running, mut any) = (vec![], vec![], vec![], false);
    for entry in entries {
        let Some(check) = check(entry) else { continue };
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

/// The card as GitHub describes the PR now (`gh pr view --json`) and the repo's allowed merge
/// methods (`gh api repos/{repo}`).
pub fn refreshed(pr: &PrCard, view: &Value, repo: Option<&Value>) -> PrCard {
    let s = |k: &str| view.get(k).and_then(Value::as_str).map(str::to_string);
    let n = |k: &str| view.get(k).and_then(Value::as_u64).unwrap_or(0);
    let state = match s("state").as_deref() {
        Some("MERGED") => card::PrState::Merged,
        Some("CLOSED") => card::PrState::Closed,
        _ => card::PrState::Open,
    };
    let checks = view.get("statusCheckRollup").and_then(Value::as_array).map(|list| latest_runs(list)).unwrap_or_default();
    let allowed = |k: &str| repo.and_then(|r| r.get(k)).and_then(Value::as_bool);
    let merge_methods = match repo {
        Some(_) => [("allow_merge_commit", card::MergeMethod::Merge), ("allow_squash_merge", card::MergeMethod::Squash), ("allow_rebase_merge", card::MergeMethod::Rebase)]
            .into_iter()
            .filter(|(k, _)| allowed(k) != Some(false))
            .map(|(_, m)| m)
            .collect(),
        None => pr.merge_methods.clone(),
    };
    PrCard {
        title: s("title").unwrap_or_else(|| pr.title.clone()),
        body: s("body").filter(|b| !b.trim().is_empty()),
        base: s("baseRefName"),
        head: s("headRefName"),
        state,
        checks,
        merge_methods,
        additions: n("additions"),
        deletions: n("deletions"),
        changed_files: n("changedFiles"),
        mergeable: s("mergeable"),
        merge_state: s("mergeStateStatus"),
        head_sha: s("headRefOid").or_else(|| pr.head_sha.clone()),
        draft: view.get("isDraft").and_then(Value::as_bool).unwrap_or(false),
        fetched: true,
        ..pr.clone()
    }
}

fn started(c: &Value) -> Option<&str> {
    c.get("startedAt").or_else(|| c.get("createdAt")).and_then(Value::as_str).filter(|at| !at.is_empty())
}

fn latest_runs(rollup: &[Value]) -> Vec<Check> {
    latest_entries(rollup).into_iter().filter_map(check).collect()
}

/// Each check once: a check run again keeps only its latest run, as GitHub's own page counts it.
pub fn latest_entries(rollup: &[Value]) -> Vec<&Value> {
    let mut latest: Vec<(&Value, Check)> = Vec::new();
    for entry in rollup {
        let Some(run) = check(entry) else { continue };
        match latest.iter_mut().find(|(_, kept)| kept.name == run.name) {
            Some(kept) => {
                let later = match (started(entry), started(kept.0)) {
                    (None, current) => current.is_none(),
                    (Some(_), None) => true,
                    (Some(candidate), Some(current)) => candidate >= current,
                };
                if later {
                    *kept = (entry, run);
                }
            }
            None => latest.push((entry, run)),
        }
    }
    latest.into_iter().map(|(entry, _)| entry).collect()
}

pub fn check(c: &Value) -> Option<Check> {
    let s = |k: &str| c.get(k).and_then(Value::as_str);
    let name = s("name").or_else(|| s("context"))?.to_string();
    let state = match (s("status"), s("conclusion"), s("state")) {
        (_, _, Some("SUCCESS")) | (Some("COMPLETED"), Some("SUCCESS" | "NEUTRAL" | "SKIPPED"), _) => CheckState::Passed,
        (_, _, Some("FAILURE" | "ERROR")) | (Some("COMPLETED"), Some(_), _) => CheckState::Failed,
        (_, _, Some("PENDING" | "EXPECTED")) | (Some(_), _, _) => CheckState::Running,
        _ => CheckState::Other,
    };
    Some(Check { name, state, url: s("detailsUrl").or_else(|| s("targetUrl")).map(str::to_string) })
}

#[cfg(test)]
#[path = "github_json_tests.rs"]
mod tests;
