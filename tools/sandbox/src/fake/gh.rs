//! The `gh` calls Alondra makes, answered from `fakes/github/prs.json`. A PR's head is its branch's tip
//! in the sandbox's bare remote, so a push changes it as it does on GitHub.
use std::collections::BTreeMap;

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::{Fakes, git, write_json};

#[derive(Serialize, Deserialize, Clone, Copy, Debug, Default, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum Checks {
    #[default]
    Success,
    Failure,
    Pending,
    None,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Pr {
    pub number: u64,
    pub repository: String,
    pub branch: String,
    pub base: String,
    /// OPEN, CLOSED or MERGED.
    pub state: String,
    pub draft: bool,
    pub checks: Checks,
    /// Why `gh pr merge` fails, when it should.
    #[serde(default)]
    pub merge_error: Option<String>,
    /// The method and head of the merge that merged it.
    #[serde(default)]
    pub merged: Option<(String, String)>,
}

pub const REPOSITORY: &str = "sandbox/repo";

type Prs = BTreeMap<u64, Pr>;

impl Fakes {
    fn prs_path(&self) -> std::path::PathBuf {
        self.dir.join("github/prs.json")
    }

    pub fn prs(&self) -> Result<Prs> {
        match std::fs::read(self.prs_path()) {
            Ok(bytes) => Ok(serde_json::from_slice(&bytes)?),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Prs::new()),
            Err(error) => Err(error.into()),
        }
    }

    pub fn pr(&self, number: u64) -> Result<Option<Pr>> {
        Ok(self.prs()?.remove(&number))
    }

    /// Changes PR `number` under the lock every `gh` call takes.
    pub fn update_pr(&self, number: u64, change: impl FnOnce(&mut Option<Pr>)) -> Result<()> {
        let _lock = self.lock("gh")?;
        let mut prs = self.prs()?;
        let mut pr = prs.remove(&number);
        change(&mut pr);
        if let Some(pr) = pr {
            prs.insert(number, pr);
        }
        write_json(&self.prs_path(), &prs)
    }

    /// The commit the remote branch of `pr` points at.
    pub fn head(&self, pr: &Pr) -> Result<String> {
        git(&self.remote(), &["rev-parse", &format!("refs/heads/{}", pr.branch)])
    }
}

/// Runs one `gh` command: what to print, or the error and exit status `gh` would give.
pub fn run(fakes: &Fakes, args: &[String]) -> Result<i32> {
    fakes.record("gh", args)?;
    let words: Vec<&str> = args.iter().map(String::as_str).collect();
    if let ["api", "--method", method, "-H", _, path, rest @ ..] = words.as_slice() {
        return super::github_api::api(fakes, method, path, rest == ["--input", "-"]);
    }
    let answer = match words.as_slice() {
        ["pr", "view", url, "--json", fields, rest @ ..] if url.starts_with("https://") => view(fakes, url, fields, rest),
        ["pr", "view", "--json", fields, ..] => view_branch(fakes, fields),
        ["pr", "merge", url, method, "--match-head-commit", head] => merge(fakes, url, method, head),
        ["pr", "ready", url] => change(fakes, url, |pr| pr.draft = false),
        ["pr", "close", url, ..] => change(fakes, url, |pr| pr.state = "CLOSED".into()),
        ["repo", "view", _, "--json", _] => Ok(json!({"mergeCommitAllowed": false, "squashMergeAllowed": true, "rebaseMergeAllowed": false}).to_string()),
        ["api", "graphql", ..] => Ok("false".into()),
        _ => Err(format!("the sandbox's gh does not know `gh {}`", args.join(" "))),
    };
    match answer {
        Ok(text) => {
            println!("{text}");
            Ok(0)
        }
        Err(error) => {
            eprintln!("{error}");
            Ok(1)
        }
    }
}

fn number_of(url: &str) -> Result<u64, String> {
    url.rsplit_once("/pull/")
        .and_then(|(_, number)| number.trim_end_matches('/').parse().ok())
        .ok_or_else(|| format!("not a pull request URL: {url}"))
}

fn found(fakes: &Fakes, url: &str) -> Result<Pr, String> {
    let number = number_of(url)?;
    fakes.pr(number).map_err(|error| error.to_string())?.ok_or_else(|| format!("GraphQL: Could not resolve to a PullRequest with the number of {number}."))
}

fn facts(fakes: &Fakes, pr: &Pr, fields: &str) -> Result<Value, String> {
    let head = fakes.head(pr).map_err(|error| error.to_string())?;
    let rollup = match pr.checks {
        Checks::None => json!([]),
        Checks::Success => json!([{"name": "build", "status": "COMPLETED", "conclusion": "SUCCESS"}]),
        Checks::Failure => json!([{"name": "build", "status": "COMPLETED", "conclusion": "FAILURE"}]),
        Checks::Pending => json!([{"name": "build", "status": "IN_PROGRESS", "conclusion": ""}]),
    };
    let remote = fakes.remote();
    let title = git(&remote, &["log", "-1", "--format=%s", &head]).unwrap_or_default();
    let (mut additions, mut deletions, mut files) = (0u64, 0u64, 0u64);
    for line in git(&remote, &["diff", "--numstat", &format!("{}...{head}", pr.base)]).unwrap_or_default().lines() {
        let mut columns = line.split_whitespace();
        additions += columns.next().and_then(|n| n.parse().ok()).unwrap_or(0);
        deletions += columns.next().and_then(|n| n.parse().ok()).unwrap_or(0);
        files += 1;
    }
    let (mergeable, merge_state) = match (pr.state.as_str(), pr.checks) {
        ("OPEN", Checks::Failure) => ("MERGEABLE", "UNSTABLE"),
        ("OPEN", Checks::Pending) => ("MERGEABLE", "BLOCKED"),
        ("OPEN", _) if pr.draft => ("MERGEABLE", "DRAFT"),
        ("OPEN", _) => ("MERGEABLE", "CLEAN"),
        _ => ("UNKNOWN", "UNKNOWN"),
    };
    let all = json!({
        "state": pr.state, "headRefOid": head, "baseRefName": pr.base, "headRefName": pr.branch, "statusCheckRollup": rollup,
        "isDraft": pr.draft, "url": format!("https://github.com/{}/pull/{}", pr.repository, pr.number),
        "title": title, "body": "Made by a scripted sandbox node.", "additions": additions, "deletions": deletions,
        "changedFiles": files, "mergeable": mergeable, "mergeStateStatus": merge_state,
    });
    let mut picked = serde_json::Map::new();
    for field in fields.split(',') {
        picked.insert(field.into(), all.get(field).cloned().ok_or_else(|| format!("Unknown JSON field: {field:?}"))?);
    }
    Ok(Value::Object(picked))
}

fn view(fakes: &Fakes, url: &str, fields: &str, rest: &[&str]) -> Result<String, String> {
    let pr = found(fakes, url)?;
    let value = facts(fakes, &pr, fields)?;
    match rest {
        ["--jq", query] => {
            let field = query.trim_start_matches('.');
            Ok(value.get(field).map(|v| v.as_str().map_or_else(|| v.to_string(), str::to_owned)).unwrap_or_default())
        }
        [] => Ok(value.to_string()),
        _ => Err(format!("the sandbox's gh does not know {rest:?}")),
    }
}

/// `gh pr view` inside a worktree: the PR of the branch checked out there.
fn view_branch(fakes: &Fakes, fields: &str) -> Result<String, String> {
    let here = std::env::current_dir().map_err(|error| error.to_string())?;
    let branch = git(&here, &["rev-parse", "--abbrev-ref", "HEAD"]).map_err(|error| error.to_string())?;
    let prs = fakes.prs().map_err(|error| error.to_string())?;
    let pr = prs.values().find(|pr| pr.branch == branch).ok_or_else(|| format!("no pull requests found for branch \"{branch}\""))?;
    Ok(facts(fakes, pr, fields)?.to_string())
}

fn merge(fakes: &Fakes, url: &str, method: &str, head: &str) -> Result<String, String> {
    let number = number_of(url)?;
    let mut outcome = Err(String::new());
    fakes
        .update_pr(number, |slot| {
            outcome = (|| {
                let pr = slot.as_mut().ok_or_else(|| format!("no pull request {number}"))?;
                if let Some(error) = &pr.merge_error {
                    return Err(error.clone());
                }
                if pr.state != "OPEN" {
                    return Err(format!("Pull request {} is not mergeable: it is {}", pr.number, pr.state.to_lowercase()));
                }
                let current = fakes.head(pr).map_err(|error| error.to_string())?;
                if current != head {
                    return Err(format!("Head branch was modified. Review and try the merge again. ({current} is not {head})"));
                }
                pr.state = "MERGED".into();
                pr.merged = Some((method.trim_start_matches("--").into(), head.into()));
                Ok(format!("✓ Squashed and merged pull request {}", pr.number))
            })();
        })
        .map_err(|error| error.to_string())?;
    outcome
}

fn change(fakes: &Fakes, url: &str, apply: impl FnOnce(&mut Pr)) -> Result<String, String> {
    let number = number_of(url)?;
    let mut missing = false;
    fakes
        .update_pr(number, |slot| match slot {
            Some(pr) => apply(pr),
            None => missing = true,
        })
        .map_err(|error| error.to_string())?;
    if missing {
        return Err(format!("no pull request {number}"));
    }
    Ok(String::new())
}

/// Opens PR `number` for the branch checked out in `worktree`.
pub fn open(fakes: &Fakes, worktree: &std::path::Path, number: u64, draft: bool, checks: Checks) -> Result<Pr> {
    let branch = git(worktree, &["rev-parse", "--abbrev-ref", "HEAD"]).context("the worktree's branch")?;
    if branch == "HEAD" {
        bail!("{} has no branch checked out", worktree.display());
    }
    let pr = Pr {
        number,
        repository: REPOSITORY.into(),
        branch,
        base: "main".into(),
        state: "OPEN".into(),
        draft,
        checks,
        merge_error: None,
        merged: None,
    };
    let stored = pr.clone();
    fakes.update_pr(number, move |slot| *slot = Some(stored))?;
    Ok(pr)
}
