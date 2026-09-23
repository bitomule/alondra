//! Keeps each open PR card in step with GitHub through `gh`: title, body, checks, what can be
//! merged and how.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use alondra_core::pr::{Check, CheckState, MergeMethod, PrState, PullRequest};
use alondra_core::{Event, View};
use serde_json::Value;

use crate::hub::Hub;

const EVERY: Duration = Duration::from_secs(20);
const REFRESH: Duration = Duration::from_secs(120);
const FIELDS: &str = "title,body,baseRefName,headRefName,additions,deletions,changedFiles,statusCheckRollup,mergeable,state";

pub async fn run(hub: Arc<Hub>) {
    let mut fetched: HashMap<u64, Instant> = HashMap::new();
    loop {
        tokio::time::sleep(EVERY).await;
        let due: Vec<PullRequest> = hub.read(|v| open(v).filter(|p| !p.fetched || fetched.get(&p.id).is_none_or(|t| t.elapsed() >= REFRESH)).cloned().collect());
        for pr in due {
            fetched.insert(pr.id, Instant::now());
            let Some(view) = gh_json(&["pr", "view", &pr.url, "--json", FIELDS]).await else { continue };
            let repo = gh_json(&["api", &format!("repos/{}", pr.repo)]).await;
            let updated = refreshed(&pr, &view, repo.as_ref());
            if updated != pr {
                let _ = hub.append(Event::PrUpdated(updated));
            }
        }
    }
}

pub fn open(v: &View) -> impl Iterator<Item = &PullRequest> {
    v.prs.values().filter(|p| p.state == PrState::Open)
}

async fn gh_json(args: &[&str]) -> Option<Value> {
    let out = tokio::time::timeout(Duration::from_secs(30), tokio::process::Command::new("gh").args(args).kill_on_drop(true).output()).await.ok()?.ok()?;
    out.status.success().then(|| serde_json::from_slice(&out.stdout).ok()).flatten()
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
    let checks = view.get("statusCheckRollup").and_then(Value::as_array).map(|list| list.iter().filter_map(check).collect()).unwrap_or_default();
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
        fetched: true,
        ..pr.clone()
    }
}

fn check(c: &Value) -> Option<Check> {
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
            "title": "Menus become cards", "body": "What and why", "baseRefName": "fase-1", "headRefName": "menus",
            "additions": 120, "deletions": 8, "changedFiles": 5, "mergeable": "MERGEABLE", "state": "OPEN",
            "statusCheckRollup": [
                {"__typename": "CheckRun", "name": "test", "status": "COMPLETED", "conclusion": "SUCCESS", "detailsUrl": "https://x/1"},
                {"__typename": "CheckRun", "name": "lint", "status": "COMPLETED", "conclusion": "FAILURE", "detailsUrl": "https://x/2"},
                {"__typename": "CheckRun", "name": "build", "status": "IN_PROGRESS", "conclusion": ""},
                {"__typename": "StatusContext", "context": "ci/legacy", "state": "PENDING", "targetUrl": "https://x/3"}
            ]
        });
        let repo = json!({"allow_merge_commit": true, "allow_squash_merge": false, "allow_rebase_merge": true});
        let r = refreshed(&pr, &view, Some(&repo));
        assert_eq!((r.title.as_str(), r.base.as_deref(), r.changed_files, r.fetched), ("Menus become cards", Some("fase-1"), 5, true));
        assert_eq!(r.checks.iter().map(|c| (c.name.as_str(), c.state)).collect::<Vec<_>>(), [
            ("test", CheckState::Passed),
            ("lint", CheckState::Failed),
            ("build", CheckState::Running),
            ("ci/legacy", CheckState::Running)
        ]);
        assert_eq!(r.merge_methods, [MergeMethod::Merge, MergeMethod::Rebase]);
        let merged = refreshed(&r, &json!({"state": "MERGED"}), None);
        assert_eq!(merged.state, PrState::Merged);
        assert_eq!(merged.merge_methods, [MergeMethod::Merge, MergeMethod::Rebase], "kept when the repo could not be read");
    }
}
