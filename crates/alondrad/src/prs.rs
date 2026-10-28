//! Reads pull requests and issues from GitHub through `gh`.

use std::time::Duration;

use alondra_core::pr::{Check, CheckState, MergeMethod, PrState, PullRequest};
use serde_json::Value;

const FIELDS: &str = "title,body,baseRefName,headRefName,additions,deletions,changedFiles,statusCheckRollup,mergeable,mergeStateStatus,state,headRefOid,isDraft";

async fn gh_json(args: &[&str]) -> Option<Value> {
    let out = tokio::time::timeout(Duration::from_secs(30), tokio::process::Command::new("gh").args(args).kill_on_drop(true).output()).await.ok()?.ok()?;
    out.status.success().then(|| serde_json::from_slice(&out.stdout).ok()).flatten()
}

/// What Alondra asks of GitHub about a PR or an issue, so tests can stand in for it.
pub trait Gh: Sync {
    fn view(&self, url: &str) -> impl Future<Output = Option<Value>> + Send;

    fn search_issues(&self, owners: &[String]) -> impl Future<Output = Result<Value, String>> + Send {
        let _ = owners;
        async { Err("issue search is not available here".to_string()) }
    }

    fn issue_view(&self, repo: &str, number: u64) -> impl Future<Output = Result<Value, String>> + Send {
        let _ = (repo, number);
        async { Err("issues are not available here".to_string()) }
    }
}

async fn gh_run(args: &[String]) -> Result<Value, String> {
    let run = tokio::process::Command::new("gh").args(args).kill_on_drop(true).output();
    match tokio::time::timeout(Duration::from_secs(30), run).await {
        Ok(Ok(out)) if out.status.success() => serde_json::from_slice(&out.stdout).map_err(|e| e.to_string()),
        Ok(Ok(out)) => Err(String::from_utf8_lossy(&out.stderr).trim().to_string()),
        Ok(Err(e)) => Err(e.to_string()),
        Err(_) => Err("gh did not finish within 30 s".into()),
    }
}

pub struct GhCli;

impl Gh for GhCli {
    async fn view(&self, url: &str) -> Option<Value> {
        gh_json(&["pr", "view", url, "--json", FIELDS]).await
    }

    /// One search across every owner: a call per repo would cost one request each.
    async fn search_issues(&self, owners: &[String]) -> Result<Value, String> {
        let mut args: Vec<String> = ["search", "issues", "--state", "open", "--limit", "100"].map(String::from).into();
        for o in owners {
            args.extend(["--owner".to_string(), o.clone()]);
        }
        args.extend(["--json".into(), "repository,number,title,url,author,authorAssociation,labels,updatedAt,commentsCount".into()]);
        gh_run(&args).await
    }

    async fn issue_view(&self, repo: &str, number: u64) -> Result<Value, String> {
        let args = ["issue", "view", &number.to_string(), "-R", repo, "--json", "url,title,body,author,labels,comments"].map(String::from);
        gh_run(&args).await
    }
}

/// The card as GitHub describes the PR now (`gh pr view --json`) and the repo's allowed merge
/// methods (`gh api repos/{repo}`).
pub fn refreshed(pr: &PullRequest, view: &Value, repo: Option<&Value>) -> PullRequest {
    let s = |k: &str| view.get(k).and_then(Value::as_str).map(str::to_string);
    let n = |k: &str| view.get(k).and_then(Value::as_u64).unwrap_or(0);
    let state = match s("state").as_deref() {
        Some("MERGED") => PrState::Merged,
        Some("CLOSED") => PrState::Closed,
        _ => PrState::Open,
    };
    let checks = view.get("statusCheckRollup").and_then(Value::as_array).map(|list| latest_runs(list)).unwrap_or_default();
    let allowed = |k: &str| repo.and_then(|r| r.get(k)).and_then(Value::as_bool);
    let merge_methods = match repo {
        Some(_) => [("allow_merge_commit", MergeMethod::Merge), ("allow_squash_merge", MergeMethod::Squash), ("allow_rebase_merge", MergeMethod::Rebase)]
            .into_iter()
            .filter(|(k, _)| allowed(k) != Some(false))
            .map(|(_, m)| m)
            .collect(),
        None => pr.merge_methods.clone(),
    };
    PullRequest {
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

/// Each check once: a check run again keeps only its latest run, as GitHub's own page counts it.
fn latest_runs(rollup: &[Value]) -> Vec<Check> {
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
    latest.into_iter().map(|(_, run)| run).collect()
}

pub(crate) fn check(c: &Value) -> Option<Check> {
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
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn a_card_takes_what_github_says_about_the_pr() {
        let pr = PullRequest::opened(1, "https://github.com/a/b/pull/7", "s", None, 0).unwrap();
        let view = json!({
            "title": "Menus become cards", "body": "What and why", "baseRefName": "main", "headRefName": "menus",
            "additions": 120, "deletions": 8, "changedFiles": 5, "mergeable": "MERGEABLE", "state": "OPEN", "headRefOid": "abc123",
            "statusCheckRollup": [
                {"__typename": "CheckRun", "name": "test", "status": "COMPLETED", "conclusion": "SUCCESS", "detailsUrl": "https://x/1"},
                {"__typename": "CheckRun", "name": "lint", "status": "COMPLETED", "conclusion": "FAILURE", "detailsUrl": "https://x/2"},
                {"__typename": "CheckRun", "name": "build", "status": "IN_PROGRESS", "conclusion": ""},
                {"__typename": "StatusContext", "context": "ci/legacy", "state": "PENDING", "targetUrl": "https://x/3"}
            ]
        });
        let repo = json!({"allow_merge_commit": true, "allow_squash_merge": false, "allow_rebase_merge": true});
        let r = refreshed(&pr, &view, Some(&repo));
        assert_eq!((r.title.as_str(), r.base.as_deref(), r.changed_files, r.fetched), ("Menus become cards", Some("main"), 5, true));
        assert_eq!(r.checks.iter().map(|c| (c.name.as_str(), c.state)).collect::<Vec<_>>(), [
            ("test", CheckState::Passed),
            ("lint", CheckState::Failed),
            ("build", CheckState::Running),
            ("ci/legacy", CheckState::Running)
        ]);
        assert_eq!(r.merge_methods, [MergeMethod::Merge, MergeMethod::Rebase]);
        let merged = refreshed(&r, &json!({"state": "MERGED"}), None);
        assert_eq!(merged.state, PrState::Merged);
        assert_eq!((r.head_sha.as_deref(), merged.head_sha.as_deref()), (Some("abc123"), Some("abc123")), "the head is kept when gh leaves it out");
        assert_eq!(merged.merge_methods, [MergeMethod::Merge, MergeMethod::Rebase], "kept when the repo could not be read");
    }

    #[test]
    fn a_check_run_again_counts_once_by_its_latest_run() {
        let pr = PullRequest::opened(1, "https://github.com/a/b/pull/7", "s", None, 0).unwrap();
        let r = refreshed(&pr, &json!({"state": "OPEN", "statusCheckRollup": [
            {"name": "build", "status": "COMPLETED", "conclusion": "FAILURE", "startedAt": "2026-10-27T10:00:00Z"},
            {"name": "lint", "status": "COMPLETED", "conclusion": "SUCCESS", "startedAt": "2026-10-27T10:00:00Z"},
            {"name": "build", "status": "IN_PROGRESS", "conclusion": "", "startedAt": "2026-10-27T10:05:00Z"},
            {"context": "deploy", "state": "FAILURE", "createdAt": "2026-10-27T10:06:00Z"},
            {"context": "deploy", "state": "SUCCESS", "createdAt": "2026-10-27T10:01:00Z"},
        ]}), None);
        let checks: Vec<_> = r.checks.iter().map(|check| (check.name.as_str(), check.state)).collect();
        assert_eq!(checks, [("build", CheckState::Running), ("lint", CheckState::Passed), ("deploy", CheckState::Failed)]);
    }

    #[test]
    fn a_conflict_reaches_the_card_as_merge_state() {
        let pr = PullRequest::opened(1, "https://github.com/a/b/pull/7", "s", None, 0).unwrap();
        let r = refreshed(&pr, &json!({"state": "OPEN", "mergeable": "CONFLICTING", "mergeStateStatus": "DIRTY"}), None);
        assert!(r.conflicts() && !r.mergeability_unknown());
        let computing = refreshed(&pr, &json!({"state": "OPEN", "mergeable": "UNKNOWN", "mergeStateStatus": "UNKNOWN"}), None);
        assert!(!computing.conflicts() && computing.mergeability_unknown());
    }
}
