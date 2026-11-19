//! What Alondra reads from GitHub with `gh` about a pull request, an issue or a branch, and the card
//! a PR's `gh pr view` makes.
use crate::cli::GhCli;
use alondra_core::pr::{Check, CheckState, MergeMethod, PrState, PullRequest};
pub use alondra_engine_ports::github::Gh;
use alondra_engine_ports::github::{AuthoredPr, GhFailure, Read};
use serde::Deserialize;
use serde_json::Value;
use std::path::Path;
use std::time::Duration;

const FIELDS: &str = "title,body,baseRefName,headRefName,additions,deletions,changedFiles,statusCheckRollup,mergeable,mergeStateStatus,state,headRefOid,isDraft";
const LIMIT: Duration = Duration::from_secs(30);

#[derive(Deserialize)]
struct Found {
    repository: Repository,
    number: u64,
    url: String,
}

#[derive(Deserialize)]
struct Repository {
    #[serde(rename = "nameWithOwner")]
    name_with_owner: String,
}

#[derive(Deserialize)]
struct Branch {
    #[serde(rename = "headRefName")]
    name: String,
}

impl GhCli {
    async fn json(&self, args: &[&str]) -> Result<Value, String> {
        let out = self.run(args, None, LIMIT).await?;
        serde_json::from_slice(&out).map_err(|e| e.to_string())
    }
}

impl Gh for GhCli {
    fn head_oid<'a>(&'a self, url: &'a str) -> Read<'a, Result<String, alondra_engine_ports::github::HeadError>> {
        Box::pin(crate::diff::head(self, url))
    }

    fn diff<'a>(&'a self, url: &'a str) -> Read<'a, Result<alondra_engine_ports::github::PrDiff, alondra_engine_ports::github::DiffError>> {
        Box::pin(crate::diff::read(self, url))
    }

    fn viewed_files<'a>(&'a self, pr: &'a alondra_domain_rules::PullRequest) -> Read<'a, Result<alondra_engine_ports::github::Marks, String>> {
        Box::pin(crate::pr_viewed::read(self, pr))
    }

    fn view<'a>(&'a self, url: &'a str) -> Read<'a, Option<Value>> {
        Box::pin(async move { self.json(&["pr", "view", url, "--json", FIELDS]).await.ok() })
    }

    /// One search across every owner: a call per repo would cost one request each.
    fn search_issues<'a>(&'a self, owners: &'a [String]) -> Read<'a, Result<Value, String>> {
        Box::pin(async move {
            let mut args = vec!["search", "issues", "--state", "open", "--limit", "100"];
            for owner in owners {
                args.extend(["--owner", owner.as_str()]);
            }
            args.extend(["--json", "repository,number,title,url,author,authorAssociation,labels,updatedAt,commentsCount"]);
            self.json(&args).await
        })
    }

    fn issue_view<'a>(&'a self, repo: &'a str, number: u64) -> Read<'a, Result<Value, String>> {
        Box::pin(async move {
            let number = number.to_string();
            self.json(&["issue", "view", &number, "-R", repo, "--json", "url,title,body,author,labels,comments"]).await
        })
    }

    fn authored_open_prs(&self) -> Read<'_, Result<Vec<AuthoredPr>, GhFailure>> {
        Box::pin(async move {
            let args = ["search", "prs", "--author", "@me", "--state", "open", "--limit", "1000", "--json", "repository,number,url"];
            let found: Vec<Found> = serde_json::from_slice(&self.run(&args, None, LIMIT).await?).map_err(GhFailure::unreadable)?;
            Ok(found.into_iter().map(|found| AuthoredPr { repository: found.repository.name_with_owner, number: found.number, url: found.url }).collect())
        })
    }

    fn head_ref<'a>(&'a self, url: &'a str) -> Read<'a, Result<String, GhFailure>> {
        Box::pin(async move {
            let out = self.run(&["pr", "view", url, "--json", "headRefName"], None, LIMIT).await?;
            serde_json::from_slice::<Branch>(&out).map(|branch| branch.name).map_err(GhFailure::unreadable)
        })
    }

    fn branch_pr<'a>(&'a self, dir: &'a Path) -> Read<'a, Option<(String, String, u64)>> {
        Box::pin(self.branch_pr_within(dir, self.branch_pr_limit))
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
mod tests {
    use super::*;
    use serde_json::json;

    /// A `gh` that answers the search of authored PRs and the branch of PR 7, and fails otherwise.
    fn scripted_gh() -> (crate::test_support::Directory, GhCli) {
        use std::os::unix::fs::PermissionsExt;
        let dir = crate::test_support::Directory::new();
        std::fs::create_dir_all(&*dir).unwrap();
        let program = dir.join("gh");
        std::fs::write(&program, "#!/bin/sh\ncase \"$*\" in\n\
            \"search prs --author @me --state open --limit 1000 --json repository,number,url\") \
            printf '%s' '[{\"repository\":{\"nameWithOwner\":\"Owner/Site\"},\"number\":7,\"url\":\"https://github.com/Owner/Site/pull/7\"}]';;\n\
            \"pr view https://github.com/Owner/Site/pull/7 --json headRefName\") printf '%s' '{\"headRefName\":\"feature\"}';;\n\
            *) echo 'no pull requests found' >&2; exit 1;;\nesac\n").unwrap();
        std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o700)).unwrap();
        let gh = GhCli::at(program);
        (dir, gh)
    }

    #[tokio::test]
    async fn the_authored_open_prs_and_a_prs_branch_are_read_through_the_port() {
        let (_dir, gh) = scripted_gh();
        let github: &dyn Gh = &gh;
        assert_eq!(github.authored_open_prs().await.unwrap(), [AuthoredPr {
            repository: "Owner/Site".into(), number: 7, url: "https://github.com/Owner/Site/pull/7".into(),
        }]);
        assert_eq!(github.head_ref("https://github.com/Owner/Site/pull/7").await.unwrap(), "feature");
        let failure = github.head_ref("https://github.com/Owner/Site/pull/8").await.unwrap_err();
        assert!(failure.to_string().contains("no pull requests found"), "{failure}");
    }

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
