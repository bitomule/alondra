//! One GitHub for the whole daemon: merging, observing, reading and the images of a description
//! all see the same pull requests, so a scenario changes GitHub once and every part of the daemon
//! finds it changed. It lets happen what GitHub lets happen: a PR merges into whatever base it
//! has, however stale, and someone else can merge or close a PR at any time. A repository can
//! move (its old name redirects), have its old name taken by another one, be deleted, or GitHub can
//! stop answering; every pull request has a key no move changes, and a change aimed at one key
//! never reaches another pull request at the same address.
use alondra_daemon_support::locks::Poisonable as _;
use crate::domain_github::{Closed, GhFailure, Located, Observed, PrFacts, PrObserver, ReadIdentity};
use crate::domain_merges::{Checks, MergeState, Pending, PrMerger, Status};
use crate::pr_images::{Failure, Fetch, GitHubImages};
use alondra_domain_model::{MergeMethod, MergeVerdict, PrChecks, PrMergeable, PrState, ProviderLocator, ProviderPrKey, PullRequest, normalize_pr_repository};
use alondra_engine_ports::github::{AuthoredPr, Gh, Read};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Mutex;

struct Pr {
    key: String,
    locator: String,
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
    verdict: MergeVerdict,
    merge_failure: Option<GhFailure>,
    no_access: bool,
    methods: Vec<MergeMethod>,
}

struct World {
    /// Each pull request under the repository that holds it now.
    prs: BTreeMap<(String, u64), Pr>,
    /// The old name of a moved or renamed repository and where GitHub redirects it.
    moved: BTreeMap<String, String>,
    /// Repositories that no longer exist or that the account lost access to.
    gone: BTreeSet<String>,
    /// GitHub answers nothing at all: a network failure.
    unreachable: bool,
    default_branch: String,
    /// The commit each branch of the repository is at, when a test set it.
    branches: BTreeMap<String, String>,
    repository: String,
    delete_head_on_merge: bool,
    commits: u64,
    keys: u64,
    calls: Vec<String>,
    /// Every change aimed at a pull request, with the key the caller named, whether GitHub let it through or not.
    attempts: Vec<(String, u64, String)>,
    /// Every pull request read, as `(repository, number)`, answered or not.
    observations: Vec<(String, u64)>,
    /// The branch checked out in each directory a node works in.
    checkouts: BTreeMap<std::path::PathBuf, String>,
    reads: Vec<String>,
    merges: Vec<(u64, String, MergeMethod)>,
    retargets: Vec<(u64, String)>,
    closes: Vec<(u64, String)>,
}

impl World {
    /// The repository a name leads to now, after GitHub's redirects, unless it is gone.
    fn resolve(&self, repository: &str) -> Option<String> {
        let mut current = normalize_pr_repository(repository);
        for _ in 0..=self.moved.len() {
            match self.moved.get(&current) {
                Some(next) => current.clone_from(next),
                None => break,
            }
        }
        (!self.gone.contains(&current)).then_some(current)
    }

    /// The pull request an address reaches, as GitHub answers it: through the redirect of a moved
    /// repository, "could not resolve" when nothing is there, nothing at all while unreachable.
    fn lookup(&self, repository: &str, number: u64) -> Result<(String, &Pr), GhFailure> {
        if self.unreachable {
            return Err(offline());
        }
        let Some(current) = self.resolve(repository) else {
            return Err(GhFailure::NotFound(format!("Could not resolve to a Repository with the name '{repository}'.")));
        };
        match self.prs.get(&(current.clone(), number)) {
            Some(pr) => Ok((current, pr)),
            None => Err(GhFailure::NotFound(format!("Could not resolve to a PullRequest with the number of {number}."))),
        }
    }

    fn lookup_mut(&mut self, repository: &str, number: u64) -> Result<&mut Pr, GhFailure> {
        let (current, _) = self.lookup(repository, number)?;
        Ok(self.prs.get_mut(&(current, number)).expect("the pull request just found"))
    }

    /// A change GitHub makes only to the pull request the caller read there: an address that
    /// answers with another one changes nothing.
    fn checked(&mut self, repository: &str, number: u64, expected: &ProviderPrKey) -> Result<&mut Pr, GhFailure> {
        self.attempts.push((repository.to_owned(), number, expected.0.clone()));
        let pr = self.lookup_mut(repository, number)?;
        if pr.key != expected.0 {
            return Err(GhFailure::IdentityMismatch(format!("{repository}#{number} is pull request {}, not {}", pr.key, expected.0)));
        }
        Ok(pr)
    }

    fn identity(&self, repository: &str, number: u64) -> Result<ReadIdentity, GhFailure> {
        let (current, pr) = self.lookup(repository, number)?;
        Ok(ReadIdentity { key: ProviderPrKey(pr.key.clone()), locator: ProviderLocator(pr.locator.clone()), repository: current, head_ref: pr.head_ref.clone() })
    }
}

fn offline() -> GhFailure {
    GhFailure::Exit(Some(1), "error connecting to api.github.com".into())
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
            moved: BTreeMap::new(),
            gone: BTreeSet::new(),
            unreachable: false,
            default_branch: "main".into(),
            branches: BTreeMap::new(),
            repository: "owner/repo".into(),
            delete_head_on_merge: false,
            commits: 0,
            keys: 0,
            calls: Vec::new(),
            attempts: Vec::new(),
            observations: Vec::new(),
            checkouts: BTreeMap::new(),
            reads: Vec::new(),
            merges: Vec::new(),
            retargets: Vec::new(),
            closes: Vec::new(),
        }))
    }

    fn world(&self) -> std::sync::MutexGuard<'_, World> {
        self.0.lock_or_recover("a test's fixture")
    }

    /// PR `number` of the world's repository, as it is now.
    fn change<T>(&self, number: u64, change: impl FnOnce(&mut Pr) -> T) -> T {
        let mut world = self.world();
        let key = (world.repository.clone(), number);
        change(world.prs.get_mut(&key).unwrap_or_else(|| panic!("the world has no PR {number}")))
    }

    /// The repository the world's pull requests belong to, as `owner/repo`.
    pub fn set_repository(&self, repository: &str) {
        self.world().repository = normalize_pr_repository(repository);
    }

    /// Opens PR `number` from branch `head_ref` into `base`; a PR whose base is another PR's head
    /// branch is stacked on it. Returns the head commit.
    pub fn open_pr(&self, number: u64, head_ref: &str, base: &str) -> String {
        let repository = self.world().repository.clone();
        self.open_pr_in(&repository, number, head_ref, base)
    }

    /// `open_pr` in another repository than the world's.
    pub fn open_pr_in(&self, repository: &str, number: u64, head_ref: &str, base: &str) -> String {
        let head = {
            let mut world = self.world();
            world.commits += 1;
            format!("{:040x}", world.commits)
        };
        self.open_at(repository, number, head_ref, base, &head);
        head
    }

    /// `open_pr` with the head commit a test already knows.
    pub fn open_pr_at(&self, number: u64, head_ref: &str, base: &str, head: &str) {
        let repository = self.world().repository.clone();
        self.open_at(&repository, number, head_ref, base, head);
    }

    fn open_at(&self, repository: &str, number: u64, head_ref: &str, base: &str, head: &str) {
        let mut world = self.world();
        world.keys += 1;
        let by_number = number.to_string();
        let key = if world.prs.values().any(|pr| pr.key == by_number) { format!("{}", 9000 + world.keys) } else { by_number };
        let repository = normalize_pr_repository(repository);
        world.prs.insert((repository, number), Pr {
            locator: format!("PR_node{key}"), key,
            head_ref: head_ref.into(), head: head.to_owned(), base: base.into(), state: PrState::Open, merged_into: None,
            draft: false, checks: Checks::Passed, merge_state: MergeState::Clean, title: format!("Pull request {number}"),
            body: String::new(), required: Vec::new(), bypassable: Vec::new(), verdict: MergeVerdict::mergeable(), merge_failure: None, no_access: false,
            methods: vec![MergeMethod::Squash, MergeMethod::Merge, MergeMethod::Rebase],
        });
    }

    /// The key GitHub has for PR `number` of the world's repository.
    pub fn key_of(&self, number: u64) -> ProviderPrKey {
        ProviderPrKey(self.change(number, |pr| pr.key.clone()))
    }

    /// GitHub moves or renames `from` to `to`: its pull requests keep their numbers and keys, and
    /// the old name redirects to the new one until someone takes it again.
    pub fn move_repository(&self, from: &str, to: &str) {
        let (from, to) = (normalize_pr_repository(from), normalize_pr_repository(to));
        let mut world = self.world();
        let moving: Vec<(String, u64)> = world.prs.keys().filter(|(repository, _)| *repository == from).cloned().collect();
        for key in moving {
            let pr = world.prs.remove(&key).expect("a pull request of the moved repository");
            world.prs.insert((to.clone(), key.1), pr);
        }
        for target in world.moved.values_mut().filter(|target| **target == from) {
            target.clone_from(&to);
        }
        world.moved.remove(&to);
        world.moved.insert(from.clone(), to.clone());
        if world.repository == from {
            world.repository = to;
        }
    }

    /// Someone creates a repository with an old name: GitHub no longer redirects it.
    pub fn reuse_name(&self, repository: &str) {
        let repository = normalize_pr_repository(repository);
        let mut world = self.world();
        world.moved.remove(&repository);
        world.gone.remove(&repository);
    }

    /// The repository is deleted, or the account loses access to it.
    pub fn delete_repository(&self, repository: &str) {
        self.world().gone.insert(normalize_pr_repository(repository));
    }

    /// GitHub answers nothing while `on`: every read and change fails as a network does.
    pub fn set_unreachable(&self, on: bool) {
        self.world().unreachable = on;
    }

    /// The branch `name` of the repository is at the commit `head`, one a test already knows.
    pub fn set_branch(&self, name: &str, head: &str) {
        self.world().branches.insert(name.to_owned(), head.to_owned());
    }

    /// The PR's branch now points at the commit `head`, one a test already knows.
    pub fn set_head(&self, number: u64, head: &str) {
        self.change(number, |pr| head.clone_into(&mut pr.head));
    }

    /// A new commit on the PR's branch; returns the new head.
    pub fn push_commit(&self, number: u64) -> String {
        let head = {
            let mut world = self.world();
            world.commits += 1;
            format!("{:040x}", world.commits)
        };
        self.change(number, |pr| pr.head.clone_from(&head));
        head
    }

    pub fn set_checks(&self, number: u64, checks: Checks) {
        self.change(number, |pr| pr.checks = checks);
    }
    pub fn set_merge_state(&self, number: u64, state: MergeState) {
        self.change(number, |pr| pr.merge_state = state);
    }
    /// What GitHub says of merging the PR: a PR is mergeable until a scenario says otherwise.
    pub fn set_verdict(&self, number: u64, verdict: MergeVerdict) {
        self.change(number, |pr| pr.verdict = verdict);
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
        let repository = world.repository.clone();
        merge(&mut world, &repository, number);
    }
    /// Someone else closes the PR on GitHub.
    pub fn close_externally(&self, number: u64) {
        self.change(number, |pr| pr.state = PrState::Closed);
    }
    /// Someone reopens the closed PR on GitHub.
    pub fn reopen(&self, number: u64) {
        self.change(number, |pr| pr.state = PrState::Open);
    }

    /// The searches and branch reads the daemon made, in order: what a sweep of open PRs asks.
    pub fn sweep_reads(&self) -> Vec<String> {
        self.world().reads.clone()
    }

    /// Every `gh` call Alondra made that changes something, in order, as `gh` would be typed:
    /// `edit 8 --base main`, `merge 8`, `ready 8`, `close 8`.
    /// Each pull request Alondra read, as `(repository, number)`: what a round of reads costs.
    pub fn observations(&self) -> Vec<(String, u64)> {
        self.world().observations.clone()
    }

    /// The node checks out `branch` in `dir`: the open pull request from it is that directory's.
    pub fn check_out(&self, dir: &std::path::Path, branch: &str) {
        self.world().checkouts.insert(dir.to_path_buf(), branch.to_owned());
    }

    /// Each change Alondra aimed at a pull request, as `(repository, number, key it named)`, also
    /// the ones GitHub refused because another pull request answered at that address.
    pub fn attempts(&self) -> Vec<(String, u64, String)> {
        self.world().attempts.clone()
    }

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
    /// The state of PR `number` of `repository`, wherever it lives now.
    pub fn state_in(&self, repository: &str, number: u64) -> Option<PrState> {
        self.world().lookup(repository, number).ok().map(|(_, pr)| pr.state)
    }
    /// The branch a merged PR landed in: its base at merge time.
    pub fn merged_into(&self, number: u64) -> Option<String> {
        self.change(number, |pr| pr.merged_into.clone())
    }

    fn merge_as(&self, pr: &PullRequest, expected: &ProviderPrKey, head: &str, method: MergeMethod, bypass: bool) -> Result<(), GhFailure> {
        let mut world = self.world();
        let github = world.checked(&pr.repository, pr.number, expected)?;
        let result = match github {
            github if github.state != PrState::Open => Err(GhFailure::Exit(Some(1), "Pull request is not open".into())),
            github if github.head != head => Err(GhFailure::Exit(Some(1), "Head branch was modified. Review and try the merge again.".into())),
            github if github.merge_failure.is_some() => Err(github.merge_failure.clone().unwrap_or_else(offline)),
            github if github.draft => Err(GhFailure::Exit(Some(1), "Pull request is still a draft".into())),
            github if github.merge_state == MergeState::Conflicting => Err(GhFailure::Exit(Some(1), "Pull request is not mergeable".into())),
            github if github.checks != Checks::Passed && missing_check(github, bypass).is_some() => Err(GhFailure::Exit(Some(1),
                format!("Pull request #{} is not mergeable: Required status check \"{}\" is expected.", pr.number,
                    missing_check(github, bypass).unwrap_or_default()))),
            _ => Ok(()),
        };
        world.calls.push(if bypass { format!("merge {} --admin", pr.number) } else { format!("merge {}", pr.number) });
        if result.is_ok() {
            world.merges.push((pr.number, head.to_owned(), method));
            let repository = world.resolve(&pr.repository).unwrap_or_default();
            merge(&mut world, &repository, pr.number);
        }
        result
    }
}

/// A required check the PR is still waiting for that this merge cannot go past.
fn missing_check(pr: &Pr, bypass: bool) -> Option<String> {
    pr.required.iter().find(|check| !bypass || !pr.bypassable.contains(check)).cloned()
}

fn merge(world: &mut World, repository: &str, number: u64) {
    let Some(merged) = world.prs.get_mut(&(repository.to_owned(), number)) else { return };
    merged.state = PrState::Merged;
    merged.merged_into = Some(merged.base.clone());
    let (head_ref, base) = (merged.head_ref.clone(), merged.base.clone());
    if world.delete_head_on_merge {
        for (_, pr) in world.prs.iter_mut().filter(|((r, _), pr)| r == repository && pr.state == PrState::Open && pr.base == head_ref) {
            pr.base.clone_from(&base);
        }
    }
}

impl PrMerger for GithubWorld {
    fn status<'a>(&'a self, pr: &'a PullRequest) -> Pending<'a, Status> {
        let world = self.world();
        let status = world.lookup(&pr.repository, pr.number).map(|(_, github)| Status {
            state: github.state, head: github.head.clone(), head_ref: github.head_ref.clone(), base: github.base.clone(), checks: github.checks.clone(),
            passed: BTreeSet::new(), merge_state: github.merge_state, review_required: false, identity: world.identity(&pr.repository, pr.number).ok(),
        });
        Box::pin(async move { status })
    }

    fn default_branch<'a>(&'a self, _pr: &'a PullRequest) -> Pending<'a, String> {
        let base = self.world().default_branch.clone();
        Box::pin(async move { Ok(base) })
    }

    fn retarget<'a>(&'a self, pr: &'a PullRequest, expected: &'a ProviderPrKey, base: &'a str) -> Pending<'a, ()> {
        let mut world = self.world();
        let result = match world.checked(&pr.repository, pr.number, expected) {
            Ok(github) if github.state == PrState::Open => {
                github.base = base.to_owned();
                Ok(())
            }
            Ok(_) => Err(GhFailure::Exit(Some(1), "Cannot change the base branch of a closed pull request".into())),
            Err(failure) => Err(failure),
        };
        if !matches!(result, Err(GhFailure::IdentityMismatch(_))) {
            world.calls.push(format!("edit {} --base {base}", pr.number));
            world.retargets.push((pr.number, base.to_owned()));
        }
        Box::pin(async move { result })
    }

    fn merge<'a>(&'a self, pr: &'a PullRequest, expected: &'a ProviderPrKey, head: &'a str, method: MergeMethod) -> Pending<'a, ()> {
        Box::pin(std::future::ready(self.merge_as(pr, expected, head, method, false)))
    }

    fn merge_bypassing<'a>(&'a self, pr: &'a PullRequest, expected: &'a ProviderPrKey, head: &'a str, method: MergeMethod) -> Pending<'a, ()> {
        Box::pin(std::future::ready(self.merge_as(pr, expected, head, method, true)))
    }

    fn bypassable_checks<'a>(&'a self, pr: &'a PullRequest, _base: &'a str) -> Pending<'a, Vec<String>> {
        let bypassable = self.world().lookup(&pr.repository, pr.number).map(|(_, github)| github.bypassable.clone());
        Box::pin(async move { bypassable })
    }

    fn in_queue<'a>(&'a self, _pr: &'a PullRequest) -> Pending<'a, bool> {
        Box::pin(async { Ok(false) })
    }

    fn is_draft<'a>(&'a self, pr: &'a PullRequest) -> Pending<'a, bool> {
        let draft = self.world().lookup(&pr.repository, pr.number).map(|(_, github)| github.draft);
        Box::pin(async move { draft })
    }

    fn ready<'a>(&'a self, pr: &'a PullRequest, expected: &'a ProviderPrKey) -> Pending<'a, ()> {
        let mut world = self.world();
        let found = world.checked(&pr.repository, pr.number, expected).map(|github| github.draft = false);
        if !matches!(found, Err(GhFailure::IdentityMismatch(_))) {
            world.calls.push(format!("ready {}", pr.number));
        }
        Box::pin(async move { found })
    }

    fn required_checks<'a>(&'a self, pr: &'a PullRequest, _base: &'a str) -> Pending<'a, Vec<String>> {
        let required = self.world().lookup(&pr.repository, pr.number).map(|(_, github)| github.required.clone());
        Box::pin(async move { required })
    }

    fn verdict<'a>(&'a self, pr: &'a PullRequest) -> Pending<'a, MergeVerdict> {
        let verdict = self.world().lookup(&pr.repository, pr.number).map(|(_, github)| github.verdict.clone());
        Box::pin(async move { verdict })
    }

    fn can_merge<'a>(&'a self, pr: &'a PullRequest) -> Pending<'a, bool> {
        let allowed = self.world().lookup(&pr.repository, pr.number).map(|(_, github)| !github.no_access);
        Box::pin(async move { allowed })
    }
}

impl PrObserver for GithubWorld {
    fn observe<'a>(&'a self, _host: &'a str, repository: &'a str, number: u64) -> Observed<'a> {
        let mut world = self.world();
        world.observations.push((repository.to_owned(), number));
        let facts = world.lookup(repository, number).map(|(_, github)| PrFacts {
            head: github.head.clone(),
            base: github.base.clone(),
            base_head: world.branches.get(&github.base).cloned(),
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
            identity: world.identity(repository, number).ok(),
        });
        Box::pin(async move { facts })
    }

    fn close<'a>(&'a self, _host: &'a str, repository: &'a str, number: u64, expected: &'a ProviderPrKey, comment: &'a str) -> Closed<'a> {
        let mut world = self.world();
        let closed = world.checked(repository, number, expected).map(|github| {
            if github.state == PrState::Open {
                github.state = PrState::Closed;
            }
        });
        if !matches!(closed, Err(GhFailure::IdentityMismatch(_))) {
            world.calls.push(format!("close {number}"));
            world.closes.push((number, comment.to_owned()));
        }
        Box::pin(async move { closed })
    }

    fn relocate<'a>(&'a self, _host: &'a str, locator: &'a ProviderLocator) -> Located<'a> {
        let world = self.world();
        let found = if world.unreachable {
            Err(offline())
        } else {
            world.prs.iter().find(|(_, pr)| pr.locator == locator.0)
                .filter(|((repository, _), _)| !world.gone.contains(repository))
                .map(|((repository, number), _)| world.identity(repository, *number))
                .unwrap_or_else(|| Err(GhFailure::NotFound(format!("Could not resolve to a node with the global id of '{}'", locator.0))))
        };
        Box::pin(async move { found })
    }
}

/// The repository and number of a pull request's URL.
fn address_of(url: &str) -> Option<(String, u64)> {
    let path = url.split_once("://")?.1.split_once('/')?.1;
    let (repository, number) = path.split_once("/pull/")?;
    Some((repository.to_owned(), number.trim_end_matches('/').parse().ok()?))
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
    fn authored_open_prs(&self) -> Read<'_, Result<Vec<AuthoredPr>, GhFailure>> {
        let mut world = self.world();
        world.reads.push("search open PRs".into());
        let world = world;
        let found = if world.unreachable {
            Err(offline())
        } else {
            Ok(world.prs.iter().filter(|((repository, _), pr)| pr.state == PrState::Open && !world.gone.contains(repository))
                .map(|((repository, number), _)| AuthoredPr {
                    repository: repository.clone(), number: *number, url: format!("https://github.com/{repository}/pull/{number}"),
                }).collect())
        };
        Box::pin(async { found })
    }
    fn branch_pr<'a>(&'a self, dir: &'a std::path::Path) -> Read<'a, Option<(String, String, u64)>> {
        let world = self.world();
        let found = (!world.unreachable).then(|| world.checkouts.get(dir)).flatten().and_then(|branch| {
            world.prs.iter().find(|((repository, _), pr)| pr.head_ref == *branch && pr.state == PrState::Open && !world.gone.contains(repository))
                .map(|((repository, number), _)| ("github.com".to_owned(), repository.clone(), *number))
        });
        Box::pin(async { found })
    }
    fn head_ref<'a>(&'a self, url: &'a str) -> Read<'a, Result<String, GhFailure>> {
        let found = match address_of(url) {
            Some((repository, number)) => {
                let mut world = self.world();
                world.reads.push(format!("branch of PR {number}"));
                world.lookup(&repository, number).map(|(_, pr)| pr.head_ref.clone())
            }
            None => Err(GhFailure::Unreadable(format!("{url} is not a pull request"))),
        };
        Box::pin(async move { found })
    }
}

impl GithubWorld {
    /// `gh pr view --json` of the pull request at `url`, after GitHub's redirect.
    fn viewed(&self, url: &str) -> Option<Value> {
        let world = self.world();
        let (repository, number) = address_of(url)?;
        let (current, pr) = world.lookup(&repository, number).ok()?;
        let (mergeable, status) = match pr.merge_state {
            MergeState::Clean => ("MERGEABLE", "CLEAN"),
            MergeState::Conflicting => ("CONFLICTING", "DIRTY"),
            MergeState::Computing => ("UNKNOWN", "UNKNOWN"),
            MergeState::Other => ("MERGEABLE", "BLOCKED"),
        };
        Some(json!({
            "title": pr.title, "body": pr.body, "baseRefName": pr.base, "baseRefOid": world.branches.get(&pr.base), "headRefName": pr.head_ref, "headRefOid": pr.head,
            "additions": 1, "deletions": 0, "changedFiles": 1, "isDraft": pr.draft,
            "state": match pr.state { PrState::Merged => "MERGED", PrState::Closed => "CLOSED", _ => "OPEN" },
            "mergeable": mergeable, "mergeStateStatus": status, "statusCheckRollup": rollup(&pr.checks),
            "fullDatabaseId": pr.key, "id": pr.locator, "url": format!("https://github.com/{current}/pull/{number}"),
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
    fn body_html<'a>(&'a self, _host: &'a str, repository: &'a str, number: u64) -> Fetch<'a, String> {
        let body = self.world().lookup(repository, number).map(|(_, pr)| pr.body.clone()).map_err(|_| Failure::NotFound);
        Box::pin(async move { body })
    }
    fn download<'a>(&'a self, _url: &'a str) -> Fetch<'a, Vec<u8>> {
        Box::pin(async { Err(Failure::NotFound) })
    }
}

#[cfg(test)]
#[path = "github_world_tests.rs"]
mod tests;
