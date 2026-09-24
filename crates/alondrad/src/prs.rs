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
const FIELDS: &str = "title,body,baseRefName,headRefName,additions,deletions,changedFiles,statusCheckRollup,mergeable,mergeStateStatus,state,headRefOid";

pub async fn run(hub: Arc<Hub>) {
    let mut fetched: HashMap<u64, Instant> = HashMap::new();
    let mut merges: HashMap<String, usize> = HashMap::new();
    let mut base_moved: HashMap<String, Instant> = HashMap::new();
    loop {
        tokio::time::sleep(EVERY).await;
        for (repo, count) in hub.read(merged_per_repo) {
            if merges.insert(repo.clone(), count).is_some_and(|before| before < count) {
                base_moved.insert(repo, Instant::now());
            }
        }
        let due: Vec<PullRequest> = hub.read(|v| due(v, &fetched, &base_moved, Instant::now()));
        for pr in due {
            fetched.insert(pr.id, Instant::now());
            refresh(&hub, &pr).await;
        }
    }
}

/// Asks GitHub about one card now and records what changed; returns the card as it is after.
pub async fn refresh(hub: &Hub, pr: &PullRequest) -> PullRequest {
    let Some(view) = gh_json(&["pr", "view", &pr.url, "--json", FIELDS]).await else { return pr.clone() };
    let repo = gh_json(&["api", &format!("repos/{}", pr.repo)]).await;
    let updated = refreshed(pr, &view, repo.as_ref());
    if updated != *pr {
        let _ = hub.append(Event::PrUpdated(updated.clone()));
    }
    updated
}

fn merged_per_repo(v: &View) -> HashMap<String, usize> {
    let mut out = HashMap::new();
    for p in v.prs.values().filter(|p| p.state == PrState::Merged) {
        *out.entry(p.repo.clone()).or_insert(0) += 1;
    }
    out
}

/// Open cards to ask GitHub about now: never fetched, not fetched for `REFRESH`, still waiting
/// for GitHub to work out whether they merge, or in a repo where a PR merged since their last
/// fetch, which may have left them in conflict.
fn due(v: &View, fetched: &HashMap<u64, Instant>, base_moved: &HashMap<String, Instant>, now: Instant) -> Vec<PullRequest> {
    open(v)
        .filter(|p| {
            let Some(last) = fetched.get(&p.id).filter(|_| p.fetched) else { return true };
            now.duration_since(*last) >= REFRESH || p.mergeability_unknown() || base_moved.get(&p.repo).is_some_and(|moved| moved > last)
        })
        .cloned()
        .collect()
}

pub fn open(v: &View) -> impl Iterator<Item = &PullRequest> {
    v.prs.values().filter(|p| p.state == PrState::Open)
}

/// Open cards that want the user now; one waiting for the node's next push stays out.
pub fn in_inbox(v: &View) -> impl Iterator<Item = &PullRequest> {
    open(v).filter(|p| !p.awaiting_push())
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
        merge_state: s("mergeStateStatus"),
        head_sha: s("headRefOid").or_else(|| pr.head_sha.clone()),
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
        assert_eq!((r.head_sha.as_deref(), merged.head_sha.as_deref()), (Some("abc123"), Some("abc123")), "the head is kept when gh leaves it out");
        assert_eq!(merged.merge_methods, [MergeMethod::Merge, MergeMethod::Rebase], "kept when the repo could not be read");
    }

    #[test]
    fn a_conflict_reaches_the_card_as_merge_state() {
        let pr = PullRequest::opened(1, "https://github.com/a/b/pull/7", "s", None, 0).unwrap();
        let r = refreshed(&pr, &json!({"state": "OPEN", "mergeable": "CONFLICTING", "mergeStateStatus": "DIRTY"}), None);
        assert!(r.conflicts() && !r.mergeability_unknown());
        let computing = refreshed(&pr, &json!({"state": "OPEN", "mergeable": "UNKNOWN", "mergeStateStatus": "UNKNOWN"}), None);
        assert!(!computing.conflicts() && computing.mergeability_unknown());
    }

    #[test]
    fn a_card_is_asked_about_again_while_github_computes_it_or_after_its_base_moved() {
        let card = |id: u64, repo: &str, mergeable: &str| {
            let mut p = PullRequest::opened(id, &format!("https://github.com/{repo}/pull/{id}"), "s", None, 0).unwrap();
            p.fetched = true;
            p.mergeable = Some(mergeable.into());
            p
        };
        let mut v = View::default();
        for p in [card(1, "a/b", "MERGEABLE"), card(2, "a/b", "UNKNOWN"), card(3, "a/b", "CONFLICTING"), card(4, "c/d", "MERGEABLE")] {
            v.prs.insert(p.id, p);
        }
        let t0 = Instant::now();
        let now = t0 + Duration::from_secs(30);
        let fetched: HashMap<u64, Instant> = (1..=4).map(|id| (id, t0)).collect();
        let ids = |moved: &HashMap<String, Instant>| due(&v, &fetched, moved, now).iter().map(|p| p.id).collect::<Vec<_>>();
        assert_eq!(ids(&HashMap::new()), vec![2], "only the one GitHub is still computing");
        let moved = HashMap::from([("a/b".to_string(), t0 + Duration::from_secs(10))]);
        assert_eq!(ids(&moved), vec![1, 2, 3], "a merge in a/b can put every open a/b card in conflict");
        assert_eq!(due(&v, &fetched, &HashMap::new(), t0 + REFRESH).len(), 4);
    }
}
