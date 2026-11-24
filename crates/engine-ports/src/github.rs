//! GitHub as the engine sees it: observing, merging and reading pull requests, and the images their
//! descriptions cite. `github-gh` implements it with the Mac's `gh`; a scenario registers a GitHub
//! of its own in its place.
use alondra_domain_rules::{MergeMethod, MergeVerdict, OpenPrStatus, PrChecks, PrMergeable, PrState, ProviderLocator, ProviderPrKey, PullRequest};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
use std::future::Future;
use std::path::Path;
use std::pin::Pin;
use std::sync::Arc;

/// Every part of GitHub, each behind its own trait so a test stands in for one at a time.
#[derive(Clone)]
pub struct GitHub {
    pub observer: Arc<dyn PrObserver>,
    pub merger: Arc<dyn PrMerger>,
    pub images: Arc<dyn GitHubImages>,
    pub reads: Arc<dyn Gh>,
}

impl GitHub {
    /// One object that stands for the whole of GitHub.
    pub fn of<W>(world: Arc<W>) -> Self
    where
        W: PrObserver + PrMerger + GitHubImages + Gh,
    {
        Self { observer: world.clone(), merger: world.clone(), images: world.clone(), reads: world }
    }
}

/// What GitHub reports for one PR. `merged_head` is the PR head at merge time,
/// never the merge commit: squash and rebase produce a different commit that
/// can never match the presented and authorized head.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PrFacts {
    pub head: String,
    pub base: String,
    pub state: PrState,
    pub merged_head: Option<String>,
    pub allowed_methods: Vec<MergeMethod>,
    pub mergeable: PrMergeable,
    pub checks: PrChecks,
    /// Which pull request answered, when GitHub said.
    pub identity: Option<ReadIdentity>,
}

/// Which pull request a read reached, in the same answer as everything else it read: the
/// adapter's stable key and locator, the address GitHub serves it at now (after any redirect of a
/// moved or renamed repository) and the branch it comes from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReadIdentity {
    pub key: ProviderPrKey,
    pub locator: ProviderLocator,
    /// `owner/repo`, lower case.
    pub repository: String,
    pub head_ref: String,
}

impl PrFacts {
    pub fn status(&self) -> OpenPrStatus {
        OpenPrStatus { state: self.state, head: self.head.clone(), base: self.base.clone(),
            mergeable: self.mergeable, checks: self.checks.clone() }
    }
}

pub type Observed<'a> = Pin<Box<dyn Future<Output = Result<PrFacts, GhFailure>> + Send + 'a>>;
pub type Closed<'a> = Pin<Box<dyn Future<Output = Result<(), GhFailure>> + Send + 'a>>;
pub type Located<'a> = Pin<Box<dyn Future<Output = Result<ReadIdentity, GhFailure>> + Send + 'a>>;

pub trait PrObserver: Send + Sync + 'static {
    fn observe<'a>(&'a self, host: &'a str, repository: &'a str, number: u64) -> Observed<'a>;
    /// Closes the PR at this address on GitHub with `comment`, only while it is `expected`: the
    /// caller read that key there in this same round. An answer from another pull request is
    /// [`GhFailure::IdentityMismatch`] and closes nothing.
    fn close<'a>(&'a self, _host: &'a str, _repository: &'a str, _number: u64, _expected: &'a ProviderPrKey, _comment: &'a str) -> Closed<'a> {
        Box::pin(async { Err(GhFailure::Unreadable("this observer cannot close pull requests".to_owned())) })
    }
    /// Finds the pull request by its locator, whatever its address is now.
    fn relocate<'a>(&'a self, _host: &'a str, _locator: &'a ProviderLocator) -> Located<'a> {
        Box::pin(async { Err(GhFailure::Unreadable("this observer cannot find pull requests by their locator".to_owned())) })
    }
    /// Which pull request answers at this address, read without anything else.
    fn read_identity<'a>(&'a self, host: &'a str, repository: &'a str, number: u64) -> Located<'a> {
        Box::pin(async move {
            let facts = self.observe(host, repository, number).await?;
            facts.identity.ok_or_else(|| GhFailure::Unreadable("GitHub did not say which pull request it is".into()))
        })
    }
}

/// Closes the PR at this address with `comment` only after a read of it there answers with
/// `expected`: a moved repository whose old name another one took never gets its pull request
/// closed, even by an observer whose `close` cannot check the key. One more read, only when the
/// user closes from a card.
pub async fn close_checked(observer: &impl PrObserver, host: &str, repository: &str, number: u64, expected: &ProviderPrKey, comment: &str)
    -> Result<(), GhFailure> {
    let read = observer.read_identity(host, repository, number).await?;
    if read.key != *expected {
        return Err(GhFailure::IdentityMismatch(format!("{repository}#{number} is another pull request now")));
    }
    observer.close(host, repository, number, expected, comment).await
}

impl PrObserver for Arc<dyn PrObserver> {
    fn observe<'a>(&'a self, host: &'a str, repository: &'a str, number: u64) -> Observed<'a> {
        (**self).observe(host, repository, number)
    }
    fn close<'a>(&'a self, host: &'a str, repository: &'a str, number: u64, expected: &'a ProviderPrKey, comment: &'a str) -> Closed<'a> {
        (**self).close(host, repository, number, expected, comment)
    }
    fn relocate<'a>(&'a self, host: &'a str, locator: &'a ProviderLocator) -> Located<'a> {
        (**self).relocate(host, locator)
    }
    fn read_identity<'a>(&'a self, host: &'a str, repository: &'a str, number: u64) -> Located<'a> {
        (**self).read_identity(host, repository, number)
    }
}

impl<T: PrObserver> PrObserver for Arc<T> {
    fn observe<'a>(&'a self, host: &'a str, repository: &'a str, number: u64) -> Observed<'a> {
        (**self).observe(host, repository, number)
    }
    fn close<'a>(&'a self, host: &'a str, repository: &'a str, number: u64, expected: &'a ProviderPrKey, comment: &'a str) -> Closed<'a> {
        (**self).close(host, repository, number, expected, comment)
    }
    fn relocate<'a>(&'a self, host: &'a str, locator: &'a ProviderLocator) -> Located<'a> {
        (**self).relocate(host, locator)
    }
    fn read_identity<'a>(&'a self, host: &'a str, repository: &'a str, number: u64) -> Located<'a> {
        (**self).read_identity(host, repository, number)
    }
}

/// Why a `gh` run failed, as the system and `gh` report it. The text in each case is only shown.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum GhFailure {
    /// `gh` could not be started here.
    Launch(std::io::ErrorKind, String),
    TimedOut(String),
    /// `gh` exited with this code, `None` when a signal ended it. 4 is `gh` needing a sign-in (`gh help exit-codes`).
    Exit(Option<i32>, String),
    /// GitHub answered with an HTTP status the call does not expect.
    Http(u16, String),
    /// `gh` answered something Alondra cannot read.
    Unreadable(String),
    /// GitHub answered for certain that nothing is there, or that this account cannot see it.
    NotFound(String),
    /// The address answered with another pull request than the one the action was for: nothing
    /// was done.
    IdentityMismatch(String),
    /// GitHub's API limit for this account is spent: the same call works again once it resets,
    /// with nothing for anyone to do.
    RateLimited(String),
}

impl GhFailure {
    /// `gh` cannot work on this Mac until someone installs it or signs it in.
    pub fn needs_setup(&self) -> bool {
        matches!(self, Self::Launch(..) | Self::Exit(Some(4), _))
    }

    pub fn signed_out(&self) -> bool {
        matches!(self, Self::Exit(Some(4), _))
    }

    pub fn timed_out(&self) -> bool {
        matches!(self, Self::TimedOut(_))
    }

    pub fn not_found(&self) -> bool {
        matches!(self, Self::NotFound(_))
    }

    pub fn rate_limited(&self) -> bool {
        matches!(self, Self::RateLimited(_))
    }

    pub fn unreadable(error: impl std::fmt::Display) -> Self {
        Self::Unreadable(error.to_string())
    }
}

impl std::fmt::Display for GhFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Launch(_, text) | Self::TimedOut(text) | Self::Exit(_, text) | Self::Unreadable(text) | Self::NotFound(text)
                | Self::IdentityMismatch(text) | Self::RateLimited(text) => f.write_str(text),
            Self::Http(status, text) => write!(f, "{text} (HTTP {status})"),
        }
    }
}

/// A `gh` that exited 1 with `stderr`, as tests script it.
impl From<&str> for GhFailure {
    fn from(stderr: &str) -> Self {
        Self::Exit(Some(1), stderr.to_owned())
    }
}

impl From<GhFailure> for String {
    fn from(failure: GhFailure) -> Self {
        failure.to_string()
    }
}

/// What GitHub answered to a `gh api --include` call.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HttpAnswer {
    pub status: u16,
    /// Header names in lower case.
    pub headers: BTreeMap<String, String>,
    pub body: Vec<u8>,
}

impl HttpAnswer {
    /// `HTTP/<version> <status> <reason>`, header lines, an empty line, then the body.
    pub fn parse(stdout: &[u8]) -> Option<Self> {
        let end = stdout.windows(4).position(|window| window == b"\r\n\r\n")?;
        let head = std::str::from_utf8(&stdout[..end]).ok()?;
        let mut lines = head.lines();
        let status = lines.next()?.strip_prefix("HTTP/")?.split_whitespace().nth(1)?.parse().ok()?;
        let headers = lines.filter_map(|line| line.split_once(':'))
            .map(|(name, value)| (name.trim().to_ascii_lowercase(), value.trim().to_owned())).collect();
        Some(Self { status, headers, body: stdout[end + 4..].to_vec() })
    }

    pub fn success(&self) -> bool {
        (200..300).contains(&self.status)
    }

    /// GitHub refused for its rate limit: a 429, or a 403 with no requests left or a wait to respect.
    pub fn rate_limited(&self) -> bool {
        self.status == 429 || (self.status == 403
            && (self.headers.get("x-ratelimit-remaining").is_some_and(|left| left == "0") || self.headers.contains_key("retry-after")))
    }

    /// GitHub's GraphQL API answers its spent limit with a 200 and an error of type `RATE_LIMITED`.
    pub fn graphql_rate_limited(&self) -> bool {
        self.success() && serde_json::from_slice::<Value>(&self.body).ok()
            .and_then(|value| value["errors"].as_array().map(|errors| errors.iter().any(|error| error["type"].as_str() == Some("RATE_LIMITED"))))
            .unwrap_or(false)
    }

    /// The answer as a failure, with GitHub's `message` for showing.
    pub fn failure(&self) -> GhFailure {
        let message = serde_json::from_slice::<Value>(&self.body).ok()
            .and_then(|value| value["message"].as_str().map(str::to_owned)).unwrap_or_default();
        if self.rate_limited() {
            return GhFailure::RateLimited(format!("{message} (HTTP {})", self.status));
        }
        GhFailure::Http(self.status, message)
    }
}

pub fn pr_url(host: &str, repository: &str, number: u64) -> String {
    format!("https://{}/{repository}/pull/{number}", alondra_domain_rules::normalize_pr_host(host))
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Checks {
    /// GitHub reports no check at all, which may only mean none has started yet.
    None,
    Pending,
    Passed,
    Failed(Vec<String>),
    /// Checks that wait for someone to act on GitHub.
    ActionRequired(Vec<String>),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Status {
    pub state: PrState,
    pub head: String,
    /// The name of the branch the PR comes from.
    pub head_ref: String,
    pub base: String,
    pub checks: Checks,
    /// The names of the checks that succeeded or need nothing more.
    pub passed: BTreeSet<String>,
    pub merge_state: MergeState,
    /// GitHub's `reviewDecision` is REVIEW_REQUIRED: the base needs an approving review first.
    pub review_required: bool,
    /// Which pull request answered, when GitHub said.
    pub identity: Option<ReadIdentity>,
}

/// What GitHub's `mergeable` and `mergeStateStatus` say about merging the PR into its base.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MergeState {
    /// CLEAN: nothing the base requires is missing, required checks included.
    Clean,
    /// `mergeable` CONFLICTING or `mergeStateStatus` DIRTY: it conflicts with its base.
    Conflicting,
    /// UNKNOWN: GitHub has not worked it out yet.
    Computing,
    /// BLOCKED, BEHIND, UNSTABLE or HAS_HOOKS: only GitHub's answer to the merge tells.
    Other,
}

pub type Pending<'a, T> = Pin<Box<dyn Future<Output = Result<T, GhFailure>> + Send + 'a>>;

pub trait PrMerger: Send + Sync + 'static {
    fn status<'a>(&'a self, pr: &'a PullRequest) -> Pending<'a, Status>;
    fn default_branch<'a>(&'a self, pr: &'a PullRequest) -> Pending<'a, String>;
    /// Every change takes the key the caller read at the PR's current address in this same round:
    /// an address that answers with another pull request is [`GhFailure::IdentityMismatch`] and
    /// changes nothing.
    fn retarget<'a>(&'a self, pr: &'a PullRequest, expected: &'a ProviderPrKey, base: &'a str) -> Pending<'a, ()>;
    /// Merges the PR only while GitHub still has it at `head`. Success may only mean GitHub put
    /// it in its merge queue.
    fn merge<'a>(&'a self, pr: &'a PullRequest, expected: &'a ProviderPrKey, head: &'a str, method: MergeMethod) -> Pending<'a, ()>;
    /// `merge` using the signed-in account's bypass of the base's rules (`gh pr merge --admin`).
    fn merge_bypassing<'a>(&'a self, pr: &'a PullRequest, expected: &'a ProviderPrKey, head: &'a str, method: MergeMethod) -> Pending<'a, ()>;
    fn in_queue<'a>(&'a self, pr: &'a PullRequest) -> Pending<'a, bool>;
    fn is_draft<'a>(&'a self, pr: &'a PullRequest) -> Pending<'a, bool>;
    fn ready<'a>(&'a self, pr: &'a PullRequest, expected: &'a ProviderPrKey) -> Pending<'a, ()>;
    /// The checks the PR's base branch requires, from its rulesets and its classic branch protection.
    fn required_checks<'a>(&'a self, pr: &'a PullRequest, base: &'a str) -> Pending<'a, Vec<String>>;
    /// The required checks of `base` the signed-in account may bypass: every ruleset that requires
    /// them lets it bypass, and classic branch protection does not require them.
    fn bypassable_checks<'a>(&'a self, pr: &'a PullRequest, base: &'a str) -> Pending<'a, Vec<String>>;
    /// Whether the signed-in account may merge into the PR's repository (`viewerPermission` WRITE or above).
    fn can_merge<'a>(&'a self, pr: &'a PullRequest) -> Pending<'a, bool>;
    /// Whether GitHub would let the PR merge now, and what stops it: its merge state, review decision,
    /// required checks and the rules the signed-in account may bypass, reduced to the domain's verdict.
    fn verdict<'a>(&'a self, pr: &'a PullRequest) -> Pending<'a, MergeVerdict>;
}

impl PrMerger for Arc<dyn PrMerger> {
    fn verdict<'a>(&'a self, pr: &'a PullRequest) -> Pending<'a, MergeVerdict> { (**self).verdict(pr) }
    fn default_branch<'a>(&'a self, pr: &'a PullRequest) -> Pending<'a, String> { (**self).default_branch(pr) }
    fn retarget<'a>(&'a self, pr: &'a PullRequest, expected: &'a ProviderPrKey, base: &'a str) -> Pending<'a, ()> { (**self).retarget(pr, expected, base) }
    fn status<'a>(&'a self, pr: &'a PullRequest) -> Pending<'a, Status> { (**self).status(pr) }
    fn merge<'a>(&'a self, pr: &'a PullRequest, expected: &'a ProviderPrKey, head: &'a str, method: MergeMethod) -> Pending<'a, ()> {
        (**self).merge(pr, expected, head, method)
    }
    fn merge_bypassing<'a>(&'a self, pr: &'a PullRequest, expected: &'a ProviderPrKey, head: &'a str, method: MergeMethod) -> Pending<'a, ()> {
        (**self).merge_bypassing(pr, expected, head, method)
    }
    fn in_queue<'a>(&'a self, pr: &'a PullRequest) -> Pending<'a, bool> { (**self).in_queue(pr) }
    fn is_draft<'a>(&'a self, pr: &'a PullRequest) -> Pending<'a, bool> { (**self).is_draft(pr) }
    fn ready<'a>(&'a self, pr: &'a PullRequest, expected: &'a ProviderPrKey) -> Pending<'a, ()> { (**self).ready(pr, expected) }
    fn required_checks<'a>(&'a self, pr: &'a PullRequest, base: &'a str) -> Pending<'a, Vec<String>> { (**self).required_checks(pr, base) }
    fn bypassable_checks<'a>(&'a self, pr: &'a PullRequest, base: &'a str) -> Pending<'a, Vec<String>> {
        (**self).bypassable_checks(pr, base)
    }
    fn can_merge<'a>(&'a self, pr: &'a PullRequest) -> Pending<'a, bool> { (**self).can_merge(pr) }
}

impl<T: PrMerger> PrMerger for Arc<T> {
    fn verdict<'a>(&'a self, pr: &'a PullRequest) -> Pending<'a, MergeVerdict> { (**self).verdict(pr) }
    fn default_branch<'a>(&'a self, pr: &'a PullRequest) -> Pending<'a, String> { (**self).default_branch(pr) }
    fn retarget<'a>(&'a self, pr: &'a PullRequest, expected: &'a ProviderPrKey, base: &'a str) -> Pending<'a, ()> { (**self).retarget(pr, expected, base) }
    fn status<'a>(&'a self, pr: &'a PullRequest) -> Pending<'a, Status> { (**self).status(pr) }
    fn merge<'a>(&'a self, pr: &'a PullRequest, expected: &'a ProviderPrKey, head: &'a str, method: MergeMethod) -> Pending<'a, ()> {
        (**self).merge(pr, expected, head, method)
    }
    fn merge_bypassing<'a>(&'a self, pr: &'a PullRequest, expected: &'a ProviderPrKey, head: &'a str, method: MergeMethod) -> Pending<'a, ()> {
        (**self).merge_bypassing(pr, expected, head, method)
    }
    fn in_queue<'a>(&'a self, pr: &'a PullRequest) -> Pending<'a, bool> { (**self).in_queue(pr) }
    fn is_draft<'a>(&'a self, pr: &'a PullRequest) -> Pending<'a, bool> { (**self).is_draft(pr) }
    fn ready<'a>(&'a self, pr: &'a PullRequest, expected: &'a ProviderPrKey) -> Pending<'a, ()> { (**self).ready(pr, expected) }
    fn required_checks<'a>(&'a self, pr: &'a PullRequest, base: &'a str) -> Pending<'a, Vec<String>> { (**self).required_checks(pr, base) }
    fn bypassable_checks<'a>(&'a self, pr: &'a PullRequest, base: &'a str) -> Pending<'a, Vec<String>> {
        (**self).bypassable_checks(pr, base)
    }
    fn can_merge<'a>(&'a self, pr: &'a PullRequest) -> Pending<'a, bool> { (**self).can_merge(pr) }
}

/// Where GitHub keeps the uploaded images of a description, behind short-lived signed links.
pub const SIGNED_IMAGE_HOST: &str = "private-user-images.githubusercontent.com";

/// A path as a URL holds it: what is already escaped stays as it is.
pub fn encoded_path(path: &str) -> String {
    path.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' | b'/' | b'%' => (b as char).to_string(),
            _ => format!("%{b:02X}"),
        })
        .collect()
}

/// Why GitHub did not give something.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Failure {
    NotFound,
    SignedOut,
    TooLarge(u64),
    Transient,
}

pub type Fetch<'a, T> = Pin<Box<dyn Future<Output = Result<T, Failure>> + Send + 'a>>;

/// What the daemon asks GitHub for the images of a description.
pub trait GitHubImages: Send + Sync + 'static {
    /// The size of a file of `repository` at `reference`.
    fn size<'a>(&'a self, host: &'a str, repository: &'a str, path: &'a str, reference: &'a str) -> Fetch<'a, u64>;
    fn file<'a>(&'a self, host: &'a str, repository: &'a str, path: &'a str, reference: &'a str) -> Fetch<'a, Vec<u8>>;
    /// Whether `repository` can be read at all.
    fn repository<'a>(&'a self, host: &'a str, repository: &'a str) -> Fetch<'a, ()>;
    /// The description as GitHub renders it, whose uploaded images carry short-lived signed links.
    fn body_html<'a>(&'a self, host: &'a str, repository: &'a str, number: u64) -> Fetch<'a, String>;
    /// A signed link, fetched without any credential.
    fn download<'a>(&'a self, url: &'a str) -> Fetch<'a, Vec<u8>>;
}

pub type Read<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

pub const MAX_PR_DIFF_BYTES: usize = 2 * 1024 * 1024;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PrDiff {
    /// At most MAX_PR_DIFF_BYTES + 1 bytes; the extra byte detects truncation.
    pub bytes: Vec<u8>,
    pub truncated: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum HeadError {
    Unavailable(GhFailure),
    InvalidResponse,
    InvalidHead,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DiffError {
    Start(GhFailure),
    Read,
    Finish,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Viewed {
    Viewed,
    Unviewed,
    /// Viewed once, then a push changed the file.
    Changed,
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
pub struct Mark {
    pub path: String,
    pub viewed: Viewed,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Marks {
    pub node: String,
    pub head: String,
    pub files: Vec<Mark>,
}

/// An open pull request the signed-in account authored, as GitHub's search names it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AuthoredPr {
    /// `owner/repo`, as GitHub spells it.
    pub repository: String,
    pub number: u64,
    pub url: String,
}

/// What Alondra reads from GitHub about a PR, an issue or a branch.
pub trait Gh: Send + Sync {
    fn head_oid<'a>(&'a self, url: &'a str) -> Read<'a, Result<String, HeadError>> {
        let _ = url;
        Box::pin(async { Err(HeadError::Unavailable(GhFailure::Unreadable("PR heads are not available here".into()))) })
    }

    fn diff<'a>(&'a self, url: &'a str) -> Read<'a, Result<PrDiff, DiffError>> {
        let _ = url;
        Box::pin(async { Err(DiffError::Finish) })
    }

    fn viewed_files<'a>(&'a self, pr: &'a PullRequest) -> Read<'a, Result<Marks, String>> {
        let _ = pr;
        Box::pin(async { Err("viewed files are not available here".into()) })
    }

    /// What GitHub's GraphQL answers to `query` for the repository `owner/name` and the PR `number`
    /// on `host`, as the bytes GitHub sent. The adapter owns the deadline of the read.
    fn pr_graphql<'a>(&'a self, host: &'a str, owner: &'a str, name: &'a str, number: u64, query: &'a str) -> Read<'a, Result<Vec<u8>, GhFailure>> {
        let _ = (host, owner, name, number, query);
        Box::pin(async { Err(GhFailure::Unreadable("GraphQL reads are not available here".into())) })
    }

    /// `gh pr view --json` of the PR at `url`, or nothing when GitHub does not answer.
    fn view<'a>(&'a self, url: &'a str) -> Read<'a, Option<Value>>;

    fn search_issues<'a>(&'a self, owners: &'a [String]) -> Read<'a, Result<Value, String>> {
        let _ = owners;
        Box::pin(async { Err("issue search is not available here".to_string()) })
    }

    fn issue_view<'a>(&'a self, repo: &'a str, number: u64) -> Read<'a, Result<Value, String>> {
        let _ = (repo, number);
        Box::pin(async { Err("issues are not available here".to_string()) })
    }

    /// Every open PR the signed-in account authored.
    fn authored_open_prs(&self) -> Read<'_, Result<Vec<AuthoredPr>, GhFailure>> {
        Box::pin(async { Err(GhFailure::Unreadable("the search of authored PRs is not available here".into())) })
    }

    /// The name of the branch the PR at `url` comes from.
    fn head_ref<'a>(&'a self, url: &'a str) -> Read<'a, Result<String, GhFailure>> {
        let _ = url;
        Box::pin(async { Err(GhFailure::Unreadable("PR branches are not available here".into())) })
    }

    /// The open PR GitHub has for the branch checked out in `dir`, as host, `owner/repo` and number.
    fn branch_pr<'a>(&'a self, dir: &'a Path) -> Read<'a, Option<(String, String, u64)>> {
        let _ = dir;
        Box::pin(async { None })
    }
}

impl<T: Gh> Gh for Arc<T> {
    fn head_oid<'a>(&'a self, url: &'a str) -> Read<'a, Result<String, HeadError>> { (**self).head_oid(url) }
    fn diff<'a>(&'a self, url: &'a str) -> Read<'a, Result<PrDiff, DiffError>> { (**self).diff(url) }
    fn viewed_files<'a>(&'a self, pr: &'a PullRequest) -> Read<'a, Result<Marks, String>> { (**self).viewed_files(pr) }
    fn pr_graphql<'a>(&'a self, host: &'a str, owner: &'a str, name: &'a str, number: u64, query: &'a str) -> Read<'a, Result<Vec<u8>, GhFailure>> { (**self).pr_graphql(host, owner, name, number, query) }
    fn view<'a>(&'a self, url: &'a str) -> Read<'a, Option<Value>> { (**self).view(url) }
    fn search_issues<'a>(&'a self, owners: &'a [String]) -> Read<'a, Result<Value, String>> { (**self).search_issues(owners) }
    fn issue_view<'a>(&'a self, repo: &'a str, number: u64) -> Read<'a, Result<Value, String>> { (**self).issue_view(repo, number) }
    fn authored_open_prs(&self) -> Read<'_, Result<Vec<AuthoredPr>, GhFailure>> { (**self).authored_open_prs() }
    fn head_ref<'a>(&'a self, url: &'a str) -> Read<'a, Result<String, GhFailure>> { (**self).head_ref(url) }
    fn branch_pr<'a>(&'a self, dir: &'a Path) -> Read<'a, Option<(String, String, u64)>> { (**self).branch_pr(dir) }
}

#[cfg(test)]
#[path = "github_tests.rs"]
mod tests;
