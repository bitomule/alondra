//! `gh` itself: running it, reading what it answers, and observing a pull request with it.
use alondra_core::clock;
use alondra_core::pr::CheckState;
use alondra_domain_rules::{MergeMethod, PrChecks, PrMergeable, PrState, ProviderLocator, ProviderPrKey};
use alondra_engine_ports::github::{Closed, GhFailure, HttpAnswer, Located, Observed, PrFacts, PrObserver, ReadIdentity, pr_url};
use serde_json::Value;
use std::path::PathBuf;
use std::time::Duration;

/// How long a delivery waits for GitHub to name one branch's open PR.
const BRANCH_PR_LIMIT: Duration = Duration::from_secs(3);

const OBSERVED_FIELDS: &str = "state,headRefOid,baseRefName,mergeable,mergeStateStatus,statusCheckRollup,fullDatabaseId,id,url,headRefName";

/// The fields of a PR that say which one it is, asked for in every read that already reads it.
pub const IDENTITY_FIELDS: &str = "fullDatabaseId,id,url,headRefName";

/// Finds a pull request by its node id, wherever its repository lives now.
const RELOCATE_QUERY: &str = "query($id:ID!){node(id:$id){... on PullRequest{fullDatabaseId id url headRefName}}}";
const ADDRESS_QUERY: &str = "query($owner:String!,$name:String!,$number:Int!){repository(owner:$owner,name:$name){pullRequest(number:$number){id}}}";

/// GitHub through the Mac's `gh`, and a signed image link through a request without credentials.
#[derive(Clone)]
pub struct GhCli {
    program: PathBuf,
    pub(crate) branch_pr_limit: Duration,
    /// Whether a call waits for `gh` however long it asks to, as a test scripts a `gh` that only
    /// the Mac's load decides how fast it starts.
    unbounded: bool,
    pub(crate) web: std::sync::OnceLock<reqwest::Client>,
}

impl Default for GhCli {
    fn default() -> Self {
        Self::new()
    }
}

impl GhCli {
    pub fn new() -> Self {
        Self::at("gh".into())
    }
    /// A `gh` at `program`, as a test scripts it.
    pub fn at(program: PathBuf) -> Self {
        Self { program, branch_pr_limit: BRANCH_PR_LIMIT, unbounded: false, web: std::sync::OnceLock::new() }
    }
    /// A `gh` no call gives up on: a test of what `gh` answers never races the time the Mac takes to
    /// start it. A test of the limits itself keeps the `gh` of [`GhCli::at`].
    pub fn without_limits(mut self) -> Self {
        self.unbounded = true;
        self
    }
    pub fn program(&self) -> &std::path::Path {
        &self.program
    }
}

impl PrObserver for GhCli {
    fn observe<'a>(&'a self, host: &'a str, repository: &'a str, number: u64) -> Observed<'a> {
        Box::pin(async move {
            let url = format!("https://{}/{repository}/pull/{number}", alondra_domain_rules::normalize_pr_host(host));
            let run = tokio::process::Command::new(&self.program)
                .args(["pr", "view", &url, "--json", OBSERVED_FIELDS])
                .kill_on_drop(true)
                .output();
            let output = match self.within(Duration::from_secs(30), run).await {
                Ok(Ok(output)) if output.status.success() => output,
                Ok(Ok(output)) => {
                    let failure = GhFailure::Exit(output.status.code(), String::from_utf8_lossy(&output.stderr).trim().to_owned());
                    return Err(self.view_failure(host, repository, number, failure).await);
                }
                Ok(Err(error)) => return Err(GhFailure::Launch(error.kind(), error.to_string())),
                Err(_) => return Err(GhFailure::TimedOut("gh pr view did not finish within 30 s".into())),
            };
            let value: Value = serde_json::from_slice(&output.stdout).map_err(GhFailure::unreadable)?;
            let mut facts = parse(&value).map_err(GhFailure::Unreadable)?;
            facts.allowed_methods = self.repository_methods(host, repository).await;
            Ok(facts)
        })
    }
    /// `gh pr close` cannot check which pull request it closes, so the key is the one the
    /// observer read at this address just before, in the same round.
    fn close<'a>(&'a self, host: &'a str, repository: &'a str, number: u64, _expected: &'a ProviderPrKey, comment: &'a str) -> Closed<'a> {
        Box::pin(async move {
            let url = pr_url(host, repository, number);
            self.run(&["pr", "close", &url, "--comment", comment], None, Duration::from_secs(30)).await.map(drop)
        })
    }
    fn relocate<'a>(&'a self, host: &'a str, locator: &'a ProviderLocator) -> Located<'a> {
        Box::pin(async move {
            let host = alondra_domain_rules::normalize_pr_host(host);
            let (query, id) = (format!("query={RELOCATE_QUERY}"), format!("id={}", locator.0));
            let answer = self.api(&["graphql", "--hostname", &host, "-f", &query, "-f", &id], Duration::from_secs(30)).await?;
            located(&answer)
        })
    }
}

/// GitHub's `NOT_FOUND` in a GraphQL answer, the one signal that says for certain nothing is
/// there for this account; a limit, an outage or an answer with data says nothing about it.
pub fn missing(answer: &HttpAnswer) -> Option<String> {
    if !answer.success() {
        return None;
    }
    let value: Value = serde_json::from_slice(&answer.body).ok()?;
    let errors = value["errors"].as_array()?;
    let found = errors.iter().find(|error| error["type"].as_str() == Some("NOT_FOUND"))?;
    Some(found["message"].as_str().unwrap_or("GitHub has nothing at this address").to_owned())
}

/// GitHub's API limit is spent, as a GraphQL answer says it: by its status and headers, or by
/// its error type.
pub fn limited(answer: &HttpAnswer) -> bool {
    answer.rate_limited() || answer.graphql_rate_limited()
}

/// What GitHub answered to `node(id:)`: the pull request it found, or for certain that none is
/// there any more.
pub fn located(answer: &HttpAnswer) -> Result<ReadIdentity, GhFailure> {
    if !answer.success() {
        return Err(answer.failure());
    }
    let value: Value = serde_json::from_slice(&answer.body).map_err(GhFailure::unreadable)?;
    let node = &value["data"]["node"];
    if node.is_null() {
        return Err(match missing(answer) {
            Some(message) => GhFailure::NotFound(message),
            None if answer.graphql_rate_limited() =>
                GhFailure::RateLimited(value["errors"][0]["message"].as_str().unwrap_or("GitHub's API rate limit is spent").to_owned()),
            None => GhFailure::Unreadable(value["errors"][0]["message"].as_str().unwrap_or("GitHub did not answer which pull request it is").to_owned()),
        });
    }
    identity(node).ok_or_else(|| GhFailure::Unreadable("GitHub's node is not a pull request".into()))
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
    (repository.split('/').count() == 2).then(|| (alondra_domain_rules::normalize_pr_host(host), alondra_domain_rules::normalize_pr_repository(repository), number))
}

impl GhCli {
    /// The merge methods the repository settings allow, squash first; empty when GitHub does not answer.
    async fn repository_methods(&self, host: &str, repository: &str) -> Vec<MergeMethod> {
        let url = format!("https://{}/{repository}", alondra_domain_rules::normalize_pr_host(host));
        let args = ["repo", "view", url.as_str(), "--json", "mergeCommitAllowed,squashMergeAllowed,rebaseMergeAllowed"];
        let Ok(out) = self.run(&args, None, Duration::from_secs(30)).await else { return vec![] };
        let Ok(value) = serde_json::from_slice::<Value>(&out) else { return vec![] };
        parse_methods(&value)
    }

    pub async fn run(&self, args: &[&str], dir: Option<&std::path::Path>, limit: Duration) -> Result<Vec<u8>, GhFailure> {
        let output = self.output(args, dir, limit).await?;
        if output.status.success() {
            return Ok(output.stdout);
        }
        Err(GhFailure::Exit(output.status.code(), String::from_utf8_lossy(&output.stderr).trim().to_owned()))
    }

    /// One `gh api` call with `--include`: GitHub's HTTP status, headers and body, whatever the status.
    /// A failure is only `gh` failing before GitHub answered.
    pub async fn api(&self, args: &[&str], limit: Duration) -> Result<HttpAnswer, GhFailure> {
        let args: Vec<&str> = ["api", "--include"].into_iter().chain(args.iter().copied()).collect();
        let output = self.output(&args, None, limit).await?;
        match HttpAnswer::parse(&output.stdout) {
            Some(answer) => Ok(answer),
            None if output.status.success() => Err(GhFailure::Unreadable("gh api answered without an HTTP status".into())),
            None => Err(GhFailure::Exit(output.status.code(), String::from_utf8_lossy(&output.stderr).trim().to_owned())),
        }
    }

    async fn within<F: std::future::Future>(&self, limit: Duration, call: F) -> Result<F::Output, clock::Elapsed> {
        if self.unbounded {
            return Ok(call.await);
        }
        clock::timeout(limit, call).await
    }

    async fn output(&self, args: &[&str], dir: Option<&std::path::Path>, limit: Duration) -> Result<std::process::Output, GhFailure> {
        let mut command = tokio::process::Command::new(&self.program);
        command.args(args).kill_on_drop(true);
        if let Some(dir) = dir {
            command.current_dir(dir);
        }
        match self.within(limit, command.output()).await {
            Ok(Ok(output)) => Ok(output),
            Ok(Err(error)) => Err(GhFailure::Launch(error.kind(), error.to_string())),
            Err(_) => Err(GhFailure::TimedOut(format!("gh {} did not finish within {} s", args.first().copied().unwrap_or_default(), limit.as_secs()))),
        }
    }

    /// Closes the PR at this address with `comment` only after a read of it there answers with
    /// `expected`: a moved repository whose old name another one took never gets its pull
    /// request closed. One more read, only when the user closes from a card.
    pub async fn close_checked(&self, host: &str, repository: &str, number: u64, expected: &ProviderPrKey, comment: &str) -> Result<(), GhFailure> {
        let url = pr_url(host, repository, number);
        let out = match self.run(&["pr", "view", &url, "--json", IDENTITY_FIELDS], None, Duration::from_secs(30)).await {
            Ok(out) => out,
            Err(failure) => return Err(self.view_failure(host, repository, number, failure).await),
        };
        let value: Value = serde_json::from_slice(&out).map_err(GhFailure::unreadable)?;
        let read = identity(&value).ok_or_else(|| GhFailure::Unreadable("GitHub did not say which pull request it is".into()))?;
        if read.key != *expected {
            return Err(GhFailure::IdentityMismatch(format!("{repository}#{number} is another pull request now")));
        }
        PrObserver::close(self, host, repository, number, expected, comment).await
    }

    /// What a failed `gh pr view` means for the pull request: `gh` prints GitHub's answer only as
    /// text, so the same lookup goes to GraphQL, whose `NOT_FOUND` says for certain nothing is
    /// there and whose status, headers and error type say GitHub's API limit is spent. Anything
    /// else keeps the failure as it was. One more call, only after a failed read.
    async fn view_failure(&self, host: &str, repository: &str, number: u64, failure: GhFailure) -> GhFailure {
        let GhFailure::Exit(..) = &failure else { return failure };
        match self.address_answer(host, repository, number).await {
            Some(answer) if limited(&answer) => GhFailure::RateLimited(failure.to_string()),
            Some(answer) => missing(&answer).map_or(failure, GhFailure::NotFound),
            None => failure,
        }
    }

    /// A failed read is GitHub's spent API limit when the same lookup through GraphQL says so;
    /// otherwise the failure stays as it was.
    pub(crate) async fn limit_failure(&self, host: &str, repository: &str, number: u64, failure: GhFailure) -> GhFailure {
        let GhFailure::Exit(..) = &failure else { return failure };
        match self.address_answer(host, repository, number).await {
            Some(answer) if limited(&answer) => GhFailure::RateLimited(failure.to_string()),
            _ => failure,
        }
    }

    async fn address_answer(&self, host: &str, repository: &str, number: u64) -> Option<HttpAnswer> {
        let (owner, name) = repository.split_once('/')?;
        let host = alondra_domain_rules::normalize_pr_host(host);
        let (query, owner, name, number) = (format!("query={ADDRESS_QUERY}"), format!("owner={owner}"), format!("name={name}"), format!("number={number}"));
        self.api(&["graphql", "--hostname", &host, "-f", &query, "-f", &owner, "-f", &name, "-F", &number], Duration::from_secs(30)).await.ok()
    }

    /// The open PR GitHub has for the branch checked out in `dir`, as host, `owner/repo` and number.
    pub async fn branch_pr_within(&self, dir: &std::path::Path, limit: Duration) -> Option<(String, String, u64)> {
        let out = self.run(&["pr", "view", "--json", "url,state"], Some(dir), limit).await.ok()?;
        let value: Value = serde_json::from_slice(&out).ok()?;
        if value["state"].as_str() != Some("OPEN") {
            return None;
        }
        address(value["url"].as_str()?)
    }
}


fn parse_methods(value: &Value) -> Vec<MergeMethod> {
    [
        ("squashMergeAllowed", MergeMethod::Squash),
        ("mergeCommitAllowed", MergeMethod::Merge),
        ("rebaseMergeAllowed", MergeMethod::Rebase),
    ]
    .into_iter()
    .filter(|(key, _)| value[*key].as_bool() == Some(true))
    .map(|(_, method)| method)
    .collect()
}

pub fn parse(value: &Value) -> Result<PrFacts, String> {
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
    let entries = value["statusCheckRollup"].as_array().map(|rollup| crate::prs::latest_entries(rollup)).unwrap_or_default();
    let (mut failed, mut approval, mut running, mut any) = (vec![], vec![], vec![], false);
    for entry in entries {
        let Some(check) = crate::prs::check(entry) else { continue };
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

#[cfg(test)]
#[path = "cli_tests.rs"]
mod tests;
