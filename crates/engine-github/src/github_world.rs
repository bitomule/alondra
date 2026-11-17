//! One GitHub for the whole daemon: merging, observing, reading and the images of a description
//! all see the same pull requests, so a scenario changes GitHub once and every part of the daemon
//! finds it changed. It lets happen what GitHub lets happen: a PR merges into whatever base it
//! has, however stale, and someone else can merge or close a PR at any time.
use crate::domain_github::{Closed, GhFailure, Observed, PrFacts, PrObserver};
use crate::domain_merges::{Checks, MergeState, Pending, PrMerger, Status};
use crate::pr_images::{Failure, Fetch, GitHubImages};
use alondra_domain_rules::{MergeMethod, PrChecks, PrMergeable, PrState, PullRequest};
use alondra_engine_ports::github::{Gh, Read};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Mutex;

struct Pr {
    head_ref: String,
    head: String,
    base: String,
    state: PrState,
    merged_into: Option<String>,
    draft: bool,
    checks: Checks,
    merge_state: MergeState,
    title: String,
    body: String,
    required: Vec<String>,
    bypassable: Vec<String>,
    merge_failure: Option<GhFailure>,
    no_access: bool,
    methods: Vec<MergeMethod>,
}

struct World {
    prs: BTreeMap<u64, Pr>,
    default_branch: String,
    delete_head_on_merge: bool,
    commits: u64,
    calls: Vec<String>,
    merges: Vec<(u64, String, MergeMethod)>,
    retargets: Vec<(u64, String)>,
    closes: Vec<(u64, String)>,
}

pub struct GithubWorld(Mutex<World>);

impl Default for GithubWorld {
    fn default() -> Self {
        Self::new()
    }
}

impl GithubWorld {
    pub fn new() -> Self {
        Self(Mutex::new(World {
            prs: BTreeMap::new(),
            default_branch: "main".into(),
            delete_head_on_merge: false,
            commits: 0,
            calls: Vec::new(),
            merges: Vec::new(),
            retargets: Vec::new(),
            closes: Vec::new(),
        }))
    }

    fn world(&self) -> std::sync::MutexGuard<'_, World> {
        self.0.lock().unwrap()
    }

    fn change<T>(&self, number: u64, change: impl FnOnce(&mut Pr) -> T) -> T {
        change(self.world().prs.get_mut(&number).unwrap_or_else(|| panic!("the world has no PR {number}")))
    }

    /// Opens PR `number` from branch `head_ref` into `base`; a PR whose base is another PR's head
    /// branch is stacked on it. Returns the head commit.
    pub fn open_pr(&self, number: u64, head_ref: &str, base: &str) -> String {
        let head = {
            let mut world = self.world();
            world.commits += 1;
            format!("{:040x}", world.commits)
        };
        self.open_pr_at(number, head_ref, base, &head);
        head
    }

    /// `open_pr` with the head commit a test already knows.
    pub fn open_pr_at(&self, number: u64, head_ref: &str, base: &str, head: &str) {
        let head = head.to_owned();
        self.world().prs.insert(number, Pr {
            head_ref: head_ref.into(), head, base: base.into(), state: PrState::Open, merged_into: None,
            draft: false, checks: Checks::Passed, merge_state: MergeState::Clean, title: format!("Pull request {number}"),
            body: String::new(), required: Vec::new(), bypassable: Vec::new(), merge_failure: None, no_access: false,
            methods: vec![MergeMethod::Squash, MergeMethod::Merge, MergeMethod::Rebase],
        });
    }

    /// A new commit on the PR's branch; returns the new head.
    pub fn push_commit(&self, number: u64) -> String {
        let mut world = self.world();
        world.commits += 1;
        let head = format!("{:040x}", world.commits);
        world.prs.get_mut(&number).unwrap_or_else(|| panic!("the world has no PR {number}")).head.clone_from(&head);
        head
    }

    pub fn set_checks(&self, number: u64, checks: Checks) {
        self.change(number, |pr| pr.checks = checks);
    }
    pub fn set_merge_state(&self, number: u64, state: MergeState) {
        self.change(number, |pr| pr.merge_state = state);
    }
    pub fn set_draft(&self, number: u64, draft: bool) {
        self.change(number, |pr| pr.draft = draft);
    }
    pub fn set_body(&self, number: u64, body: &str) {
        self.change(number, |pr| pr.body = body.into());
    }
    pub fn require_checks(&self, number: u64, names: &[&str]) {
        self.change(number, |pr| pr.required = names.iter().map(|name| (*name).to_owned()).collect());
    }
    /// Rulesets that let the signed-in account bypass these required checks of the PR's base.
    pub fn allow_bypass(&self, number: u64, names: &[&str]) {
        self.change(number, |pr| pr.bypassable = names.iter().map(|name| (*name).to_owned()).collect());
    }
    /// GitHub refuses every merge of the PR with `failure`.
    pub fn refuse_merge(&self, number: u64, failure: GhFailure) {
        self.change(number, |pr| pr.merge_failure = Some(failure));
    }
    pub fn deny_merge_access(&self, number: u64) {
        self.change(number, |pr| pr.no_access = true);
    }
    /// What the repository's "delete head branches" setting does: a merged PR's dependants move to its base.
    pub fn delete_head_on_merge(&self, on: bool) {
        self.world().delete_head_on_merge = on;
    }

    /// Someone else merges the PR on GitHub, into the base it has now.
    pub fn merge_externally(&self, number: u64) {
        let mut world = self.world();
        merge(&mut world, number);
    }
    /// Someone else closes the PR on GitHub.
    pub fn close_externally(&self, number: u64) {
        self.change(number, |pr| pr.state = PrState::Closed);
    }

    /// Every `gh` call Alondra made that changes something, in order, as `gh` would be typed:
    /// `edit 8 --base main`, `merge 8`, `ready 8`, `close 8`.
    pub fn gh_calls(&self) -> Vec<String> {
        self.world().calls.clone()
    }
    pub fn retargets(&self) -> Vec<(u64, String)> {
        self.world().retargets.clone()
    }
    pub fn merges(&self) -> Vec<(u64, String, MergeMethod)> {
        self.world().merges.clone()
    }
    pub fn closes(&self) -> Vec<(u64, String)> {
        self.world().closes.clone()
    }
    pub fn base_of(&self, number: u64) -> String {
        self.change(number, |pr| pr.base.clone())
    }
    pub fn state_of(&self, number: u64) -> PrState {
        self.change(number, |pr| pr.state)
    }
    /// The branch a merged PR landed in: its base at merge time.
    pub fn merged_into(&self, number: u64) -> Option<String> {
        self.change(number, |pr| pr.merged_into.clone())
    }

    fn merge_as(&self, pr: &PullRequest, head: &str, method: MergeMethod, bypass: bool) -> Result<(), GhFailure> {
        let mut world = self.world();
        world.calls.push(if bypass { format!("merge {} --admin", pr.number) } else { format!("merge {}", pr.number) });
        let result = match world.prs.get(&pr.number) {
            None => Err(unknown()),
            Some(github) if github.state != PrState::Open => Err(GhFailure::Exit(Some(1), "Pull request is not open".into())),
            Some(github) if github.head != head => Err(GhFailure::Exit(Some(1), "Head branch was modified. Review and try the merge again.".into())),
            Some(github) if github.merge_failure.is_some() => Err(github.merge_failure.clone().unwrap_or_else(unknown)),
            Some(github) if github.draft => Err(GhFailure::Exit(Some(1), "Pull request is still a draft".into())),
            Some(github) if github.merge_state == MergeState::Conflicting => Err(GhFailure::Exit(Some(1), "Pull request is not mergeable".into())),
            Some(github) if github.checks != Checks::Passed && missing_check(github, bypass).is_some() => Err(GhFailure::Exit(Some(1),
                format!("Pull request #{} is not mergeable: Required status check \"{}\" is expected.", pr.number,
                    missing_check(github, bypass).unwrap_or_default()))),
            Some(_) => Ok(()),
        };
        if result.is_ok() {
            world.merges.push((pr.number, head.to_owned(), method));
            merge(&mut world, pr.number);
        }
        result
    }
}

/// A required check the PR is still waiting for that this merge cannot go past.
fn missing_check(pr: &Pr, bypass: bool) -> Option<String> {
    pr.required.iter().find(|check| !bypass || !pr.bypassable.contains(check)).cloned()
}

fn merge(world: &mut World, number: u64) {
    let Some(merged) = world.prs.get_mut(&number) else { return };
    merged.state = PrState::Merged;
    merged.merged_into = Some(merged.base.clone());
    let (head_ref, base) = (merged.head_ref.clone(), merged.base.clone());
    if world.delete_head_on_merge {
        for dependant in world.prs.values_mut().filter(|pr| pr.state == PrState::Open && pr.base == head_ref) {
            dependant.base.clone_from(&base);
        }
    }
}

fn unknown() -> GhFailure {
    GhFailure::Exit(Some(1), "no such PR".into())
}

impl PrMerger for GithubWorld {
    fn status<'a>(&'a self, pr: &'a PullRequest) -> Pending<'a, Status> {
        let status = self.world().prs.get(&pr.number).map(|github| Status {
            state: github.state, head: github.head.clone(), head_ref: github.head_ref.clone(), base: github.base.clone(), checks: github.checks.clone(),
            passed: BTreeSet::new(), merge_state: github.merge_state,
        });
        Box::pin(async move { status.ok_or_else(unknown) })
    }

    fn default_branch<'a>(&'a self, _pr: &'a PullRequest) -> Pending<'a, String> {
        let base = self.world().default_branch.clone();
        Box::pin(async move { Ok(base) })
    }

    fn retarget<'a>(&'a self, pr: &'a PullRequest, base: &'a str) -> Pending<'a, ()> {
        let mut world = self.world();
        world.calls.push(format!("edit {} --base {base}", pr.number));
        world.retargets.push((pr.number, base.to_owned()));
        let result = match world.prs.get_mut(&pr.number) {
            Some(github) if github.state == PrState::Open => {
                github.base = base.to_owned();
                Ok(())
            }
            Some(_) => Err(GhFailure::Exit(Some(1), "Cannot change the base branch of a closed pull request".into())),
            None => Err(unknown()),
        };
        Box::pin(async move { result })
    }

    fn merge<'a>(&'a self, pr: &'a PullRequest, head: &'a str, method: MergeMethod) -> Pending<'a, ()> {
        Box::pin(std::future::ready(self.merge_as(pr, head, method, false)))
    }

    fn merge_bypassing<'a>(&'a self, pr: &'a PullRequest, head: &'a str, method: MergeMethod) -> Pending<'a, ()> {
        Box::pin(std::future::ready(self.merge_as(pr, head, method, true)))
    }

    fn bypassable_checks<'a>(&'a self, pr: &'a PullRequest, _base: &'a str) -> Pending<'a, Vec<String>> {
        let bypassable = self.world().prs.get(&pr.number).map(|github| github.bypassable.clone());
        Box::pin(async move { bypassable.ok_or_else(unknown) })
    }

    fn in_queue<'a>(&'a self, _pr: &'a PullRequest) -> Pending<'a, bool> {
        Box::pin(async { Ok(false) })
    }

    fn is_draft<'a>(&'a self, pr: &'a PullRequest) -> Pending<'a, bool> {
        let draft = self.world().prs.get(&pr.number).map(|github| github.draft);
        Box::pin(async move { draft.ok_or_else(unknown) })
    }

    fn ready<'a>(&'a self, pr: &'a PullRequest) -> Pending<'a, ()> {
        let mut world = self.world();
        world.calls.push(format!("ready {}", pr.number));
        let found = world.prs.get_mut(&pr.number).map(|github| github.draft = false);
        Box::pin(async move { found.ok_or_else(unknown) })
    }

    fn required_checks<'a>(&'a self, pr: &'a PullRequest, _base: &'a str) -> Pending<'a, Vec<String>> {
        let required = self.world().prs.get(&pr.number).map(|github| github.required.clone());
        Box::pin(async move { required.ok_or_else(unknown) })
    }

    fn can_merge<'a>(&'a self, pr: &'a PullRequest) -> Pending<'a, bool> {
        let allowed = self.world().prs.get(&pr.number).map(|github| !github.no_access);
        Box::pin(async move { allowed.ok_or_else(unknown) })
    }
}

impl PrObserver for GithubWorld {
    fn observe<'a>(&'a self, _host: &'a str, _repository: &'a str, number: u64) -> Observed<'a> {
        let facts = self.world().prs.get(&number).map(|github| PrFacts {
            head: github.head.clone(),
            base: github.base.clone(),
            merged_head: (github.state == PrState::Merged).then(|| github.head.clone()),
            state: github.state,
            allowed_methods: github.methods.clone(),
            mergeable: match github.merge_state {
                MergeState::Clean => PrMergeable::Clean,
                MergeState::Conflicting => PrMergeable::Conflicting,
                MergeState::Computing => PrMergeable::Computing,
                MergeState::Other => PrMergeable::Other,
            },
            checks: match &github.checks {
                Checks::None => PrChecks::None,
                Checks::Pending => PrChecks::Pending { names: Vec::new() },
                Checks::Passed => PrChecks::Passed,
                Checks::Failed(names) => PrChecks::Failed { names: names.clone() },
                Checks::ActionRequired(names) => PrChecks::ActionRequired { names: names.clone() },
            },
        });
        Box::pin(async move { facts.ok_or_else(|| "no such PR".to_owned()) })
    }

    fn close<'a>(&'a self, _host: &'a str, _repository: &'a str, number: u64, comment: &'a str) -> Closed<'a> {
        let mut world = self.world();
        world.calls.push(format!("close {number}"));
        world.closes.push((number, comment.to_owned()));
        let closed = world.prs.get_mut(&number).map(|github| {
            if github.state == PrState::Open {
                github.state = PrState::Closed;
            }
        });
        Box::pin(async move { closed.ok_or_else(|| "no such PR".to_owned()) })
    }
}

fn number_of(url: &str) -> Option<u64> {
    url.trim_end_matches('/').rsplit('/').next()?.parse().ok()
}

fn rollup(checks: &Checks) -> Vec<Value> {
    let completed = |name: &str, conclusion: &str| json!({"__typename": "CheckRun", "name": name, "status": "COMPLETED", "conclusion": conclusion});
    match checks {
        Checks::None => Vec::new(),
        Checks::Pending => vec![json!({"__typename": "CheckRun", "name": "ci", "status": "IN_PROGRESS", "conclusion": ""})],
        Checks::Passed => vec![completed("ci", "SUCCESS")],
        Checks::Failed(failed) => failed.iter().map(|name| completed(name, "FAILURE")).collect(),
        Checks::ActionRequired(waiting) => waiting.iter().map(|name| completed(name, "ACTION_REQUIRED")).collect(),
    }
}

impl Gh for GithubWorld {
    fn view<'a>(&'a self, url: &'a str) -> Read<'a, Option<Value>> {
        Box::pin(async move { self.viewed(url) })
    }
}

impl GithubWorld {
    fn viewed(&self, url: &str) -> Option<Value> {
        let world = self.world();
        let pr = world.prs.get(&number_of(url)?)?;
        let (mergeable, status) = match pr.merge_state {
            MergeState::Clean => ("MERGEABLE", "CLEAN"),
            MergeState::Conflicting => ("CONFLICTING", "DIRTY"),
            MergeState::Computing => ("UNKNOWN", "UNKNOWN"),
            MergeState::Other => ("MERGEABLE", "BLOCKED"),
        };
        Some(json!({
            "title": pr.title, "body": pr.body, "baseRefName": pr.base, "headRefName": pr.head_ref, "headRefOid": pr.head,
            "additions": 1, "deletions": 0, "changedFiles": 1, "isDraft": pr.draft,
            "state": match pr.state { PrState::Merged => "MERGED", PrState::Closed => "CLOSED", _ => "OPEN" },
            "mergeable": mergeable, "mergeStateStatus": status, "statusCheckRollup": rollup(&pr.checks),
        }))
    }
}

impl GitHubImages for GithubWorld {
    fn size<'a>(&'a self, _host: &'a str, _repository: &'a str, _path: &'a str, _reference: &'a str) -> Fetch<'a, u64> {
        Box::pin(async { Err(Failure::NotFound) })
    }
    fn file<'a>(&'a self, _host: &'a str, _repository: &'a str, _path: &'a str, _reference: &'a str) -> Fetch<'a, Vec<u8>> {
        Box::pin(async { Err(Failure::NotFound) })
    }
    fn repository<'a>(&'a self, _host: &'a str, _repository: &'a str) -> Fetch<'a, ()> {
        Box::pin(async { Ok(()) })
    }
    fn body_html<'a>(&'a self, _host: &'a str, _repository: &'a str, number: u64) -> Fetch<'a, String> {
        let body = self.world().prs.get(&number).map(|pr| pr.body.clone());
        Box::pin(async move { body.ok_or(Failure::NotFound) })
    }
    fn download<'a>(&'a self, _url: &'a str) -> Fetch<'a, Vec<u8>> {
        Box::pin(async { Err(Failure::NotFound) })
    }
}

#[cfg(test)]
#[path = "github_world_tests.rs"]
mod tests;
