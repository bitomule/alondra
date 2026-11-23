//! Merging a pull request with `gh`, and what GitHub's rules and checks say about merging it.
use crate::cli::GhCli;
use crate::verdict::{CheckKind, classify, derive, explains_with_rules, gate};
use alondra_domain_rules::{MergeMethod, MergeVerdict, PrState, ProviderPrKey, PullRequest};
use alondra_engine_ports::github::{Checks, GhFailure, HttpAnswer, MergeState, Pending, PrMerger, Status, pr_url};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
use std::time::Duration;

fn merge_state(value: &Value) -> MergeState {
    match (value["mergeable"].as_str(), value["mergeStateStatus"].as_str()) {
        (Some("CONFLICTING"), _) | (_, Some("DIRTY")) => MergeState::Conflicting,
        (_, Some("CLEAN")) => MergeState::Clean,
        (_, Some("UNKNOWN") | None) => MergeState::Computing,
        _ => MergeState::Other,
    }
}

impl GhCli {
    async fn gh_merge(&self, pr: &PullRequest, head: &str, method: MergeMethod, bypass: bool) -> Result<(), GhFailure> {
        let url = pr_url(&pr.host, &pr.repository, pr.number);
        let flag = match method {
            MergeMethod::Merge => "--merge",
            MergeMethod::Squash => "--squash",
            MergeMethod::Rebase => "--rebase",
        };
        let mut args = vec!["pr", "merge", &url, flag, "--match-head-commit", head];
        if bypass { args.push("--admin"); }
        self.run(&args, None, Duration::from_secs(120)).await.map(|_| ())
    }
}

impl PrMerger for GhCli {
    fn default_branch<'a>(&'a self, pr: &'a PullRequest) -> Pending<'a, String> {
        Box::pin(async move {
            let url = format!("https://{}/{}", alondra_domain_rules::normalize_pr_host(&pr.host), pr.repository);
            let out = self.run(&["repo", "view", &url, "--json", "defaultBranchRef"], None, Duration::from_secs(30)).await?;
            let value: Value = serde_json::from_slice(&out).map_err(GhFailure::unreadable)?;
            value["defaultBranchRef"]["name"].as_str().filter(|name| !name.is_empty()).map(str::to_owned)
                .ok_or_else(|| GhFailure::Unreadable("GitHub did not provide the repository's default branch".into()))
        })
    }
    /// Like every change below, `gh` cannot check which pull request it changes: the key is the
    /// one `status` read at this address just before, in the same round.
    fn retarget<'a>(&'a self, pr: &'a PullRequest, _expected: &'a ProviderPrKey, base: &'a str) -> Pending<'a, ()> {
        Box::pin(async move {
            let url = pr_url(&pr.host, &pr.repository, pr.number);
            self.run(&["pr", "edit", &url, "--base", base], None, Duration::from_secs(30)).await.map(|_| ())
        })
    }
    fn status<'a>(&'a self, pr: &'a PullRequest) -> Pending<'a, Status> {
        Box::pin(async move {
            let url = pr_url(&pr.host, &pr.repository, pr.number);
            let out = self.run(&["pr", "view", &url, "--json", "state,headRefOid,headRefName,baseRefName,statusCheckRollup,mergeStateStatus,mergeable,fullDatabaseId,id,url"], None,
                Duration::from_secs(30)).await?;
            let value: Value = serde_json::from_slice(&out).map_err(GhFailure::unreadable)?;
            let facts = crate::cli::parse(&value).map_err(GhFailure::Unreadable)?;
            Ok(Status { state: facts.state, head: facts.head, head_ref: value["headRefName"].as_str().unwrap_or_default().to_owned(), base: facts.base, checks: rollup(&value), passed: passed(&value),
                merge_state: merge_state(&value), identity: facts.identity })
        })
    }
    fn merge<'a>(&'a self, pr: &'a PullRequest, _expected: &'a ProviderPrKey, head: &'a str, method: MergeMethod) -> Pending<'a, ()> {
        Box::pin(self.gh_merge(pr, head, method, false))
    }
    fn merge_bypassing<'a>(&'a self, pr: &'a PullRequest, _expected: &'a ProviderPrKey, head: &'a str, method: MergeMethod) -> Pending<'a, ()> {
        Box::pin(self.gh_merge(pr, head, method, true))
    }
    fn is_draft<'a>(&'a self, pr: &'a PullRequest) -> Pending<'a, bool> {
        Box::pin(async move {
            let url = pr_url(&pr.host, &pr.repository, pr.number);
            let out = self.run(&["pr", "view", &url, "--json", "isDraft", "--jq", ".isDraft"], None, Duration::from_secs(30)).await?;
            match String::from_utf8_lossy(&out).trim() {
                "true" => Ok(true),
                "false" => Ok(false),
                other => Err(GhFailure::Unreadable(format!("GitHub did not say whether the PR is a draft: {other}"))),
            }
        })
    }
    fn ready<'a>(&'a self, pr: &'a PullRequest, _expected: &'a ProviderPrKey) -> Pending<'a, ()> {
        Box::pin(async move {
            let url = pr_url(&pr.host, &pr.repository, pr.number);
            self.run(&["pr", "ready", &url], None, Duration::from_secs(60)).await.map(|_| ())
        })
    }
    fn required_checks<'a>(&'a self, pr: &'a PullRequest, base: &'a str) -> Pending<'a, Vec<String>> {
        Box::pin(async move {
            let host = alondra_domain_rules::normalize_pr_host(&pr.host);
            let repo = format!("repos/{}", pr.repository);
            let rules = self.api(&["--hostname", &host, &format!("{repo}/rules/branches/{base}?per_page=100")], Duration::from_secs(30)).await?;
            let mut required = match rules_read(&rules)? {
                Some(rules) => ruleset_checks(&rules),
                None => vec![],
            };
            let classic = self.api(&["--hostname", &host, &format!("{repo}/branches/{base}/protection/required_status_checks")],
                Duration::from_secs(30)).await?;
            if let Some(classic) = rules_read(&classic)? {
                required.extend(classic_checks(&classic));
            }
            required.sort();
            required.dedup();
            Ok(required)
        })
    }
    fn bypassable_checks<'a>(&'a self, pr: &'a PullRequest, base: &'a str) -> Pending<'a, Vec<String>> {
        Box::pin(async move {
            let host = alondra_domain_rules::normalize_pr_host(&pr.host);
            let repo = format!("repos/{}", pr.repository);
            let rules = self.api(&["--hostname", &host, &format!("{repo}/rules/branches/{base}?per_page=100")], Duration::from_secs(30)).await?;
            let Some(rules) = rules_read(&rules)? else { return Ok(vec![]) };
            let mut can_bypass = BTreeMap::new();
            for id in check_rulesets(&rules) {
                let ruleset = self.api(&["--hostname", &host, &format!("{repo}/rulesets/{id}")], Duration::from_secs(30)).await?;
                let ruleset = rules_read(&ruleset)?;
                can_bypass.insert(id, ruleset.is_some_and(|ruleset| viewer_bypasses(&ruleset)));
            }
            let classic = self.api(&["--hostname", &host, &format!("{repo}/branches/{base}/protection/required_status_checks")],
                Duration::from_secs(30)).await?;
            let classic = rules_read(&classic)?.map(|classic| classic_checks(&classic)).unwrap_or_default();
            Ok(bypassable(&rules, &can_bypass, &classic))
        })
    }
    fn can_merge<'a>(&'a self, pr: &'a PullRequest) -> Pending<'a, bool> {
        Box::pin(async move {
            let url = format!("https://{}/{}", alondra_domain_rules::normalize_pr_host(&pr.host), pr.repository);
            let out = self.run(&["repo", "view", &url, "--json", "viewerPermission", "--jq", ".viewerPermission"], None,
                Duration::from_secs(30)).await?;
            match String::from_utf8_lossy(&out).trim() {
                "ADMIN" | "MAINTAIN" | "WRITE" => Ok(true),
                "TRIAGE" | "READ" | "null" => Ok(false),
                other => Err(GhFailure::Unreadable(format!("GitHub reported an unknown permission {other:?}"))),
            }
        })
    }
    fn verdict<'a>(&'a self, pr: &'a PullRequest) -> Pending<'a, MergeVerdict> {
        Box::pin(async move {
            let url = pr_url(&pr.host, &pr.repository, pr.number);
            let out = self.run(&["pr", "view", &url, "--json",
                "state,headRefOid,baseRefName,statusCheckRollup,mergeStateStatus,mergeable,reviewDecision,isDraft"], None, Duration::from_secs(30)).await?;
            let value: Value = serde_json::from_slice(&out).map_err(GhFailure::unreadable)?;
            let facts = crate::cli::parse(&value).map_err(GhFailure::Unreadable)?;
            if facts.state != PrState::Open || facts.head != pr.head {
                return Ok(MergeVerdict::Unknown);
            }
            let (required, bypassable) = if explains_with_rules(gate(&value)) {
                (self.required_checks(pr, &facts.base).await?, self.bypassable_checks(pr, &facts.base).await?)
            } else {
                (vec![], vec![])
            };
            Ok(derive(&value, &required, &bypassable))
        })
    }
    fn in_queue<'a>(&'a self, pr: &'a PullRequest) -> Pending<'a, bool> {
        Box::pin(async move {
            let (owner, name) = pr.repository.split_once('/')
                .ok_or_else(|| GhFailure::Unreadable("the PR's repository is not owner/name".into()))?;
            let host = alondra_domain_rules::normalize_pr_host(&pr.host);
            let query = "query($owner:String!,$name:String!,$number:Int!){repository(owner:$owner,name:$name){pullRequest(number:$number){isInMergeQueue}}}";
            let (owner, name, number) = (format!("owner={owner}"), format!("name={name}"), format!("number={}", pr.number));
            let query = format!("query={query}");
            let out = self.run(&["api", "graphql", "--hostname", &host, "-f", &query, "-F", &owner, "-F", &name, "-F", &number,
                "--jq", ".data.repository.pullRequest.isInMergeQueue"], None, Duration::from_secs(30)).await?;
            match String::from_utf8_lossy(&out).trim() {
                "true" => Ok(true),
                "false" => Ok(false),
                other => Err(GhFailure::Unreadable(format!("GitHub did not say whether the PR is in its merge queue: {other}"))),
            }
        })
    }
}

/// The checks the active rulesets of a branch require (`GET /repos/{owner}/{repo}/rules/branches/{branch}`).
pub fn ruleset_checks(rules: &Value) -> Vec<String> {
    rules.as_array().into_iter().flatten().filter(|rule| rule["type"].as_str() == Some("required_status_checks"))
        .flat_map(|rule| rule["parameters"]["required_status_checks"].as_array().into_iter().flatten())
        .filter_map(|check| check["context"].as_str().map(str::to_owned)).collect()
}

/// The rulesets whose `required_status_checks` rules apply to the branch.
pub fn check_rulesets(rules: &Value) -> BTreeSet<u64> {
    rules.as_array().into_iter().flatten().filter(|rule| rule["type"].as_str() == Some("required_status_checks"))
        .filter_map(|rule| rule["ruleset_id"].as_u64()).collect()
}

/// A ruleset's `current_user_can_bypass` lets the signed-in account merge a PR past it.
pub fn viewer_bypasses(ruleset: &Value) -> bool {
    matches!(ruleset["current_user_can_bypass"].as_str(), Some("always" | "pull_requests_only" | "exempt"))
}

/// The required checks every requiring ruleset lets the account bypass and classic protection does not require.
pub fn bypassable(rules: &Value, can_bypass: &BTreeMap<u64, bool>, classic: &[String]) -> Vec<String> {
    let mut held = BTreeSet::new();
    let mut bypassed = BTreeSet::new();
    for rule in rules.as_array().into_iter().flatten().filter(|rule| rule["type"].as_str() == Some("required_status_checks")) {
        let bypasses = rule["ruleset_id"].as_u64().and_then(|id| can_bypass.get(&id).copied()).unwrap_or(false);
        let names = rule["parameters"]["required_status_checks"].as_array().into_iter().flatten().filter_map(|check| check["context"].as_str());
        for name in names {
            if bypasses { bypassed.insert(name.to_owned()); } else { held.insert(name.to_owned()); }
        }
    }
    held.extend(classic.iter().cloned());
    bypassed.difference(&held).cloned().collect()
}

/// The rules GitHub answered, or `None` when it has none to show: none set (404), or a private
/// repository whose plan has no branch protection (403 that is not its rate limit). Either way no
/// required check is known, and `mergeStateStatus` decides.
fn rules_read(answer: &HttpAnswer) -> Result<Option<Value>, GhFailure> {
    match answer.status {
        _ if answer.success() => serde_json::from_slice(&answer.body).map(Some).map_err(GhFailure::unreadable),
        404 => Ok(None),
        403 if !answer.rate_limited() => Ok(None),
        _ => Err(answer.failure()),
    }
}

/// The checks a branch's classic protection requires (`.../protection/required_status_checks`).
pub fn classic_checks(protection: &Value) -> Vec<String> {
    let contexts = protection["contexts"].as_array().into_iter().flatten().filter_map(|c| c.as_str().map(str::to_owned));
    let checks = protection["checks"].as_array().into_iter().flatten().filter_map(|c| c["context"].as_str().map(str::to_owned));
    contexts.chain(checks).collect()
}

/// The checks GitHub reports for a PR's head. Only a check that failed sends the PR back;
/// cancelled, stale, skipped and neutral checks neither fail nor hold the merge.
pub fn rollup(value: &Value) -> Checks {
    let Some(list) = value["statusCheckRollup"].as_array().filter(|list| !list.is_empty()) else { return Checks::None };
    let (mut failed, mut action, mut pending) = (vec![], vec![], false);
    for check in list {
        let (name, kind) = classify(check);
        match kind {
            CheckKind::Failed => failed.push(name),
            CheckKind::NeedsAction => action.push(name),
            CheckKind::Passed => {}
            CheckKind::Running => pending = true,
        }
    }
    if !failed.is_empty() {
        Checks::Failed(failed)
    } else if !action.is_empty() {
        Checks::ActionRequired(action)
    } else if pending {
        Checks::Pending
    } else {
        Checks::Passed
    }
}

/// The checks GitHub reports done for a PR's head: none of them holds a merge.
fn passed(value: &Value) -> BTreeSet<String> {
    value["statusCheckRollup"].as_array().into_iter().flatten().filter(|check| {
        let s = |key: &str| check.get(key).and_then(Value::as_str).unwrap_or_default();
        matches!(s("conclusion"), "SUCCESS" | "NEUTRAL" | "SKIPPED") || s("state") == "SUCCESS"
    }).filter_map(|check| ["name", "context"].into_iter().filter_map(|key| check[key].as_str()).find(|name| !name.is_empty()))
        .map(str::to_owned).collect()
}

#[cfg(test)]
#[path = "merges_tests.rs"]
mod tests;
