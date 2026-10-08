//! Keeps each open PR card in step with GitHub through `gh`: title, body, checks, what can be
//! merged and how.

use std::collections::{HashMap, HashSet};
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use alondra_core::automerge::{AutoMerge, Obstacle};
use alondra_core::pr::{Check, CheckState, MergeMethod, PrState, PullRequest};
use alondra_core::stack::Retarget;
use alondra_core::{Event, MessageFrom, View};
use serde_json::Value;

use crate::hub::Hub;

const EVERY: Duration = Duration::from_secs(20);
/// What a merge that did not finish in time says.
pub const TIMED_OUT: &str = "gh pr merge did not finish within 90 s";
const REFRESH: Duration = Duration::from_secs(120);
/// How often a PR Alondra merges on its own is asked about while its checks run.
const AUTO_MERGE_REFRESH: Duration = Duration::from_secs(40);
const FIELDS: &str = "title,body,baseRefName,headRefName,additions,deletions,changedFiles,statusCheckRollup,mergeable,mergeStateStatus,state,headRefOid,isDraft";

pub async fn run(hub: Arc<Hub>) {
    let mut fetched: HashMap<u64, Instant> = HashMap::new();
    let mut merges: HashMap<String, usize> = HashMap::new();
    let mut base_moved: HashMap<String, Instant> = HashMap::new();
    let mut merging = AutoMerging::default();
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
        auto_merge(&hub, &mut merging, &GhCli).await;
    }
}

/// A merge GitHub refused is tried again after this long, unless a push moved the PR first; the
/// wait doubles with each refusal of the same head, up to `MERGE_RETRY_MAX`.
pub const MERGE_RETRY: Duration = Duration::from_secs(300);
pub const MERGE_RETRY_MAX: Duration = Duration::from_secs(2 * 3600);

/// What auto-merge remembers between rounds: what it said, once per head, and when a merge of a
/// head last failed and how many times in a row.
pub struct AutoMerging {
    told: HashSet<(u64, String, String)>,
    failed: HashMap<(u64, String), (Instant, u32)>,
    retry_after: Duration,
    retry_max: Duration,
}

impl Default for AutoMerging {
    fn default() -> Self {
        AutoMerging { told: HashSet::new(), failed: HashMap::new(), retry_after: MERGE_RETRY, retry_max: MERGE_RETRY_MAX }
    }
}

impl AutoMerging {
    /// How long a head GitHub refused `times` times in a row waits before the next try.
    fn wait(&self, times: u32) -> Duration {
        self.retry_after.saturating_mul(1 << times.saturating_sub(1).min(16)).min(self.retry_max)
    }
}

/// Merges the PRs of auto-merge tasks that GitHub says can go in, through the same path as
/// David's Merge. A node hears once per push what only it can fix; a merge GitHub refused for
/// another reason is tried again after `MERGE_RETRY`, then less and less often while GitHub
/// keeps refusing that head, and the orchestrator hears why once.
pub async fn auto_merge(hub: &Arc<Hub>, merging: &mut AutoMerging, gh: &impl Gh) {
    let now = alondra_core::now_millis();
    let head = |pr: &PullRequest| pr.head_sha.clone().unwrap_or_default();
    let (ready, blocked, heads) = hub.read(|v| {
        let (mut ready, mut blocked) = (vec![], vec![]);
        for p in open(v) {
            match v.auto_merge(p, now) {
                Some(AutoMerge::Ready(method)) => ready.push((p.clone(), method)),
                Some(AutoMerge::Blocked(obstacle)) => blocked.push((p.clone(), obstacle)),
                Some(AutoMerge::Waiting(_)) | None => {}
            }
        }
        (ready, blocked, open(v).map(|p| (p.id, head(p))).collect::<HashSet<_>>())
    });
    merging.failed.retain(|key, _| heads.contains(key));
    for (pr, method) in ready {
        let key = (pr.id, head(&pr));
        let times = match merging.failed.get(&key) {
            Some((at, times)) if at.elapsed() < merging.wait(*times) => continue,
            Some((_, times)) => *times,
            None => 0,
        };
        let Err(refusal) = crate::http::merge(hub, pr.id, method, crate::http::Merger::Alondra, gh).await else { continue };
        if hub.read(|v| v.prs.get(&pr.id).map(head)).as_ref() != Some(&key.1) {
            continue;
        }
        merging.failed.insert(key.clone(), (Instant::now(), times + 1));
        let (first, most) = (merging.wait(1).as_secs() / 60, merging.retry_max.as_secs() / 60);
        if let Some(text) = refusal.for_node(&pr.url)
            && merging.told.insert((pr.id, key.1.clone(), "merge".to_string()))
        {
            log!("{} not merged by Alondra: {text}", pr.url);
            if let Err(e) = crate::http::tell_about_pr(hub, &pr, text, MessageFrom::Alondra, None, vec![]) {
                log!("nobody to tell why {} was not merged: {e}", pr.url);
            }
        }
        if let Some(why) = refusal.for_orchestrator()
            && merging.told.insert((pr.id, key.1.clone(), "refused".to_string()))
        {
            log!("{} not merged by Alondra, tried again in {first} min: {why}", pr.url);
            crate::orchestrate::tell_orchestrator(
                hub,
                format!("[Alondra] Alondra could not merge {} (auto-merge): {why}. It tries again in {first} min, then less and less often while GitHub refuses it (at most every {most} min), and at once after a push; its node was not told, since it cannot fix this.", pr.url),
            );
        }
    }
    for (pr, obstacle) in blocked {
        if merging.told.insert((pr.id, head(&pr), format!("{obstacle:?}"))) {
            let _ = crate::http::tell_about_pr(hub, &pr, obstacle_text(&pr, &obstacle), MessageFrom::Alondra, None, vec![]);
        }
    }
}

/// What the node of an auto-merge task hears when only it can move its PR on.
pub fn obstacle_text(pr: &PullRequest, obstacle: &Obstacle) -> String {
    match obstacle {
        Obstacle::ChecksFailed(names) => format!("[Alondra] The checks of {} failed ({}). Fix them and push: Alondra merges it once they pass.", pr.url, names.join(", ")),
        Obstacle::Conflicts => {
            let base = pr.base.as_deref().unwrap_or("its base");
            format!("[Alondra] {} conflicts with {base}: bring {base} into the branch, resolve the conflicts and push. Alondra merges it once GitHub can.", pr.url)
        }
        Obstacle::Draft => format!("[Alondra] {} is a draft. Mark it ready (gh pr ready) when it is: Alondra merges it once its checks pass.", pr.url),
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

/// Watched cards to ask GitHub about now: never fetched, not fetched for `REFRESH`, still waiting
/// for GitHub to work out whether they merge, or in a repo where a PR merged since their last
/// fetch, which may have left them in conflict.
fn due(v: &View, fetched: &HashMap<u64, Instant>, base_moved: &HashMap<String, Instant>, now: Instant) -> Vec<PullRequest> {
    watched(v)
        .filter(|p| {
            let Some(last) = fetched.get(&p.id).filter(|_| p.fetched) else { return true };
            let checks_running = p.auto_merge && p.checks.iter().any(|c| c.state == CheckState::Running);
            now.duration_since(*last) >= REFRESH
                || p.mergeability_unknown()
                || base_moved.get(&p.repo).is_some_and(|moved| moved > last)
                || (checks_running && now.duration_since(*last) >= AUTO_MERGE_REFRESH)
        })
        .cloned()
        .collect()
}

pub fn open(v: &View) -> impl Iterator<Item = &PullRequest> {
    v.prs.values().filter(|p| p.state == PrState::Open)
}

/// Cards kept in step with GitHub: the open ones, and a set-aside one while an open card sits on
/// it, so merging or closing it on GitHub releases the card above.
pub fn watched(v: &View) -> impl Iterator<Item = &PullRequest> {
    v.prs.values().filter(|p| p.state == PrState::Open || (p.state == PrState::Dismissed && open(v).any(|up| v.stack_below(up).is_some_and(|b| b.id == p.id))))
}

/// Open cards that want the user now. One waiting for the node's next push stays out, and so
/// does one the user let its node merge on its own.
pub fn in_inbox(v: &View) -> impl Iterator<Item = &PullRequest> {
    open(v).filter(|p| !p.awaiting_push() && !p.auto_merge && !v.left_out.contains(&p.session_id))
}

async fn gh_json(args: &[&str]) -> Option<Value> {
    let out = tokio::time::timeout(Duration::from_secs(30), tokio::process::Command::new("gh").args(args).kill_on_drop(true).output()).await.ok()?.ok()?;
    out.status.success().then(|| serde_json::from_slice(&out.stdout).ok()).flatten()
}

/// What Alondra asks of GitHub about a PR, so the train steps can run against a fake.
pub trait Gh: Sync {
    fn edit_base(&self, url: &str, base: &str) -> impl Future<Output = Result<(), String>> + Send;
    fn view(&self, url: &str) -> impl Future<Output = Option<Value>> + Send;
    /// Whether GitHub knows the PR.
    fn exists(&self, url: &str) -> impl Future<Output = bool> + Send;

    /// Merges the PR, only while its head is still `head` when one is given; what GitHub said
    /// when it would not.
    fn merge(&self, url: &str, method: MergeMethod, head: Option<&str>) -> impl Future<Output = Result<(), String>> + Send {
        let _ = (url, method, head);
        async { Err("merging is not available here".to_string()) }
    }

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

/// `gh pr merge`, pinned to the head whose checks passed when there is one: GitHub refuses the
/// merge if a push moved the branch since (cli.github.com/manual/gh_pr_merge).
fn merge_args(url: &str, method: MergeMethod, head: Option<&str>) -> Vec<String> {
    let mut args: Vec<String> = ["pr", "merge", url, method.flag()].map(String::from).into();
    if let Some(head) = head {
        args.extend(["--match-head-commit".to_string(), head.to_string()]);
    }
    args
}

impl Gh for GhCli {
    async fn edit_base(&self, url: &str, base: &str) -> Result<(), String> {
        let edit = tokio::process::Command::new("gh").args(["pr", "edit", url, "--base", base]).kill_on_drop(true).output();
        match tokio::time::timeout(Duration::from_secs(30), edit).await {
            Ok(Ok(out)) if out.status.success() => Ok(()),
            Ok(Ok(out)) => Err(String::from_utf8_lossy(&out.stderr).trim().to_string()),
            Ok(Err(e)) => Err(e.to_string()),
            Err(_) => Err("gh pr edit did not finish within 30 s".into()),
        }
    }

    async fn view(&self, url: &str) -> Option<Value> {
        gh_json(&["pr", "view", url, "--json", FIELDS]).await
    }

    async fn exists(&self, url: &str) -> bool {
        gh_json(&["pr", "view", url, "--json", "url,state,headRefName"]).await.is_some()
    }

    async fn merge(&self, url: &str, method: MergeMethod, head: Option<&str>) -> Result<(), String> {
        let merge = tokio::process::Command::new("gh").args(merge_args(url, method, head)).kill_on_drop(true).output();
        match tokio::time::timeout(Duration::from_secs(90), merge).await {
            Ok(Ok(out)) if out.status.success() => Ok(()),
            Ok(Ok(out)) => Err(String::from_utf8_lossy(&out.stderr).trim().to_string()),
            Ok(Err(e)) => Err(e.to_string()),
            Err(_) => Err(TIMED_OUT.into()),
        }
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

type Found<'a> = Pin<Box<dyn Future<Output = bool> + Send + 'a>>;

/// `Gh::exists` behind a pointer, so the hub can hold the real `gh` or a test's fake.
pub trait FindsPrs: Send + Sync {
    fn finds<'a>(&'a self, url: &'a str) -> Found<'a>;
}

impl<G: Gh + Send> FindsPrs for G {
    fn finds<'a>(&'a self, url: &'a str) -> Found<'a> {
        Box::pin(self.exists(url))
    }
}

const MISSING_FOR: Duration = Duration::from_secs(600);

/// Asks GitHub whether a PR exists before it gets a card, remembering for `MISSING_FOR` the ones
/// it did not know, so a node repeating a fixture URL does not send Alondra back to `gh`.
pub struct Lookup {
    gh: Box<dyn FindsPrs>,
    missing: Mutex<HashMap<String, Instant>>,
}

impl Lookup {
    pub fn new(gh: impl FindsPrs + 'static) -> Self {
        Lookup { gh: Box::new(gh), missing: Mutex::new(HashMap::new()) }
    }

    fn missing_lately(&self, url: &str) -> bool {
        let mut missing = self.missing.lock().expect("missing lock poisoned");
        missing.retain(|_, at| at.elapsed() < MISSING_FOR);
        missing.contains_key(url)
    }

    async fn finds(&self, url: &str) -> bool {
        let found = self.gh.finds(url).await;
        if !found {
            self.missing.lock().expect("missing lock poisoned").insert(url.to_string(), Instant::now());
        }
        found
    }
}

/// Opens a card for each PR in `urls` that GitHub confirms, for the task given, which the caller
/// decided when it saw the PR. One GitHub does not know (a fixture URL in test output, a typo)
/// gets no card. 24 Sep, seq 29139-29143: a/b#7, #24 and #54, fixtures of Alondra's own tests,
/// reached David's inbox as cards.
pub async fn open_confirmed(hub: &Hub, session_id: &str, urls: Vec<String>, task: Option<String>) {
    for url in urls {
        let Some((repo, number)) = alondra_core::pr::parse_url(&url) else { continue };
        let url = alondra_core::pr::canonical(&repo, number);
        if !hub.read(|v| v.prs.values().any(|p| p.repo == repo && p.number == number)) {
            if hub.pr_lookup.missing_lately(&url) {
                continue;
            }
            if !hub.pr_lookup.finds(&url).await {
                let node = hub.read(|v| v.nodes.get(session_id).and_then(|n| n.name.clone())).unwrap_or_else(|| session_id.to_string());
                log!("PR {url} from {node}: not found on GitHub, no card");
                continue;
            }
        }
        if let Err(e) = hub.append(Event::PrOpened { url, session_id: session_id.to_string(), task: task.clone() }) {
            log!("could not record a PR: {e}");
        }
    }
}

/// Whether a command of the shell line runs `gh pr create`, rather than only mentioning it as a
/// grep or an echo does. 24 Sep, seq 29139: a grep for 'gh pr create' counted as one.
pub fn runs_pr_create(line: &str) -> bool {
    commands(line).iter().any(|c| {
        let mut words = c.split_whitespace().skip_while(|w| is_assignment(w) || *w == "command");
        (words.next(), words.next(), words.next()) == (Some("gh"), Some("pr"), Some("create"))
    })
}

fn is_assignment(word: &str) -> bool {
    word.split_once('=').is_some_and(|(name, _)| {
        name.starts_with(|c: char| c.is_ascii_alphabetic() || c == '_') && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
    })
}

/// The commands of a shell line, split at `&&`, `||`, `;`, `|` and newlines outside quotes.
fn commands(line: &str) -> Vec<String> {
    let mut out = vec![];
    let mut current = String::new();
    let mut quote: Option<char> = None;
    let mut chars = line.chars().peekable();
    while let Some(c) = chars.next() {
        match (quote, c) {
            (Some(q), c) if c == q => quote = None,
            (None | Some('"'), '\\') => {
                current.push(c);
                current.extend(chars.next());
                continue;
            }
            (None, '\'' | '"') => quote = Some(c),
            (None, ';' | '|' | '\n') => {
                out.push(std::mem::take(&mut current));
                continue;
            }
            (None, '&') if chars.peek() == Some(&'&') => {
                chars.next();
                out.push(std::mem::take(&mut current));
                continue;
            }
            _ => {}
        }
        current.push(c);
    }
    out.push(current);
    out
}

/// The PR `gh pr create` made: gh prints its URL alone on the last line of its output. Any other
/// PR URL the output mentions is not it.
pub fn created_pr(response: &Value) -> Option<String> {
    let out = response.get("stdout").and_then(Value::as_str).or_else(|| response.as_str())?;
    let last = out.lines().map(str::trim).rfind(|l| !l.is_empty())?;
    let (repo, number) = alondra_core::pr::parse_url(last)?;
    let url = alondra_core::pr::canonical(&repo, number);
    (url == last).then_some(url)
}

/// Moves each PR of the plan onto its new base and returns the ones GitHub accepted. GitHub
/// only does this itself when the merged branch is deleted, and Alondra's merge keeps it.
pub async fn retarget(hub: &Hub, plan: &[Retarget], gh: &impl Gh) -> Vec<Retarget> {
    let mut moved = vec![];
    for r in plan {
        match gh.edit_base(&r.url, &r.base).await {
            Ok(()) => {
                log!("{} now goes into {}", r.url, r.base);
                let _ = hub.append_with(|v| v.prs.get(&r.id).map(|p| Event::PrUpdated(PullRequest { base: Some(r.base.clone()), mergeable: None, ..p.clone() })));
                moved.push(r.clone());
            }
            Err(e) => log!("could not move {} onto {}: {e}", r.url, r.base),
        }
    }
    moved
}

/// Refreshes each moved card until GitHub has worked out whether it still merges cleanly, and
/// asks the node that opened it to rebase when it no longer does.
pub async fn settle(hub: &Hub, moved: &[Retarget], gh: &impl Gh, pause: Duration, tries: usize) {
    for r in moved {
        for _ in 0..tries {
            tokio::time::sleep(pause).await;
            let Some(pr) = hub.read(|v| v.prs.get(&r.id).cloned()) else { break };
            let Some(view) = gh.view(&pr.url).await else { continue };
            let updated = refreshed(&pr, &view, None);
            if updated != pr {
                let _ = hub.append(Event::PrUpdated(updated.clone()));
            }
            if let Some(text) = rebase_request(&updated, &r.base) {
                if let Err(e) = crate::http::tell_about_pr(hub, &updated, text, MessageFrom::Alondra, None, vec![]) {
                    log!("{} conflicts after moving onto {}: {e}", updated.url, r.base);
                }
                break;
            }
            if updated.mergeable.as_deref().is_some_and(|m| m != "UNKNOWN") {
                break;
            }
        }
    }
}

/// What the node hears when its PR, moved onto `base`, no longer merges cleanly.
pub fn rebase_request(pr: &PullRequest, base: &str) -> Option<String> {
    (pr.state == PrState::Open && pr.conflicts()).then(|| {
        let branch = pr.head.as_deref().unwrap_or("your branch");
        format!("[Alondra] The PR under {} was merged, so {} now goes into {base} and conflicts with it. Rebase {branch} onto origin/{base}, resolve the conflicts and push.", pr.url, pr.url)
    })
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
        draft: view.get("isDraft").and_then(Value::as_bool).unwrap_or(false),
        fetched: true,
        ..pr.clone()
    }
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

    #[derive(Default)]
    struct FakeGh {
        refuses: bool,
        edits: std::sync::Mutex<Vec<(String, String)>>,
        mergeable: std::sync::Mutex<std::collections::VecDeque<&'static str>>,
        merged: std::sync::Mutex<Vec<String>>,
        /// The head each merge was pinned to.
        heads: std::sync::Mutex<Vec<Option<String>>>,
        /// What GitHub says to the next merges, one each, before it takes one.
        refusals: std::sync::Mutex<std::collections::VecDeque<&'static str>>,
    }

    impl Gh for FakeGh {
        async fn edit_base(&self, url: &str, base: &str) -> Result<(), String> {
            self.edits.lock().unwrap().push((url.into(), base.into()));
            if self.refuses { Err("no permission".into()) } else { Ok(()) }
        }

        async fn view(&self, _url: &str) -> Option<Value> {
            let m = self.mergeable.lock().unwrap().pop_front()?;
            Some(json!({"title": "Upper", "state": "OPEN", "baseRefName": "main", "headRefName": "y", "mergeable": m}))
        }

        async fn exists(&self, _url: &str) -> bool {
            true
        }

        async fn merge(&self, url: &str, _method: MergeMethod, head: Option<&str>) -> Result<(), String> {
            self.heads.lock().unwrap().push(head.map(str::to_string));
            if let Some(said) = self.refusals.lock().unwrap().pop_front() {
                return Err(said.into());
            }
            self.merged.lock().unwrap().push(url.into());
            Ok(())
        }
    }

    fn hub(_tag: &str) -> Hub {
        let directory = crate::test_support::Directory::new();
        let home = directory.to_path_buf();
        let _ = std::fs::remove_dir_all(&home);
        let config = alondra_core::config::Config::resolve(home, &Default::default(), |_| None, || Some("David".into()));
        Hub::open(config).unwrap().with_test_directory(directory)
    }

    #[tokio::test]
    async fn merging_the_lower_pr_moves_the_next_one_and_asks_for_a_rebase_only_on_conflicts() {
        let cases: &[(&str, bool, &[&'static str], &str, bool)] = &[
            ("merges cleanly", false, &["UNKNOWN", "MERGEABLE", "MERGEABLE"], "main", false),
            ("conflicts", false, &["UNKNOWN", "CONFLICTING"], "main", true),
            ("gh refuses the move", true, &["CONFLICTING"], "x", false),
        ];
        for (i, &(what, refuses, mergeable, base, told)) in cases.iter().enumerate() {
            let hub = hub(&i.to_string());
            hub.append(Event::Hook(alondra_core::HookEvent { session_id: "s".into(), event: "UserPromptSubmit".into(), ..Default::default() })).unwrap();
            for (n, base, head) in [(1, "main", "x"), (2, "x", "y")] {
                hub.append(Event::PrOpened { url: format!("https://github.com/a/b/pull/{n}"), session_id: "s".into(), task: None }).unwrap();
                let mut pr = hub.read(|v| v.prs[&n].clone());
                (pr.base, pr.head) = (Some(base.into()), Some(head.into()));
                hub.append(Event::PrUpdated(pr)).unwrap();
            }
            let gh = FakeGh { refuses, mergeable: std::sync::Mutex::new(mergeable.iter().copied().collect()), ..Default::default() };
            let plan = hub.read(|v| v.retarget_plan(1));
            hub.append(Event::PrMerged { id: 1, method: MergeMethod::Merge }).unwrap();
            let moved = retarget(&hub, &plan, &gh).await;
            settle(&hub, &moved, &gh, Duration::ZERO, 5).await;
            assert_eq!(*gh.edits.lock().unwrap(), [("https://github.com/a/b/pull/2".to_string(), "main".to_string())], "{what}");
            assert_eq!(hub.read(|v| v.prs[&2].base.clone()).as_deref(), Some(base), "{what}");
            let asked = hub.read(|v| v.messages.values().any(|m| m.session_id == "s" && m.text.contains("Rebase y onto origin/main")));
            assert_eq!(asked, told, "{what}");
            assert!(!refuses || gh.mergeable.lock().unwrap().len() == mergeable.len(), "{what}: nothing refreshed after a refused move");
        }
    }

    #[tokio::test]
    async fn alondra_merges_an_auto_merge_pr_once_github_lets_it_and_tells_the_node_what_only_it_can_fix() {
        use alondra_core::tasks::TaskState;
        let hub = Arc::new(hub("auto-merge"));
        hub.append(Event::Launched { name: "app/fix".into(), request_id: None, auto_merge: true, task: None, title: None, session: None }).unwrap();
        hub.append(Event::DaemonSeen(alondra_core::DaemonObservation { session_id: "s".into(), short_id: "s".into(), name: Some("app/fix".into()), pid: Some(1), ..Default::default() })).unwrap();
        hub.append(Event::PrOpened { url: "https://github.com/a/b/pull/7".into(), session_id: "s".into(), task: None }).unwrap();
        let check = |state: CheckState| vec![Check { name: "lint".into(), state, url: None }];
        let github = |head: &str, checks: Vec<Check>| {
            let mut pr = hub.read(|v| v.prs[&1].clone());
            (pr.fetched, pr.head_sha, pr.checks, pr.mergeable) = (true, Some(head.into()), checks, Some("MERGEABLE".into()));
            hub.append(Event::PrUpdated(pr)).unwrap();
        };
        let (gh, mut told) = (FakeGh::default(), AutoMerging::default());
        let task = |hub: &Hub| hub.read(|v| v.session_task("s").map(|t| (t.state, v.auto_merge_pending(t, alondra_core::now_millis()))));
        let said = |hub: &Hub| hub.read(|v| v.messages.values().filter(|m| m.session_id == "s").map(|m| m.text.clone()).collect::<Vec<_>>());
        assert!(hub.read(|v| v.prs[&1].auto_merge), "the task's option reaches its PR");

        github("a1", check(CheckState::Running));
        auto_merge(&hub, &mut told, &gh).await;
        assert!(gh.merged.lock().unwrap().is_empty());
        assert_eq!(task(&hub), Some((TaskState::Running, true)), "waiting on the checks is not the node's work");

        github("a1", check(CheckState::Failed));
        auto_merge(&hub, &mut told, &gh).await;
        auto_merge(&hub, &mut told, &gh).await;
        assert_eq!(said(&hub).iter().filter(|t| t.contains("checks of https://github.com/a/b/pull/7 failed (lint)")).count(), 1, "told once per push");
        assert_eq!(task(&hub), Some((TaskState::Running, false)), "only the node can move it on");

        github("a2", check(CheckState::Passed));
        auto_merge(&hub, &mut told, &gh).await;
        assert_eq!(*gh.merged.lock().unwrap(), ["https://github.com/a/b/pull/7"]);
        assert_eq!(hub.read(|v| v.prs[&1].state), PrState::Merged);
        assert_eq!(task(&hub).map(|t| t.0), Some(TaskState::Closing));
        assert!(said(&hub).iter().any(|t| t.contains("Alondra merged https://github.com/a/b/pull/7")), "the node gets the merge notice: {:?}", said(&hub));
    }

    /// An auto-merge node with PR #7 open, and an orchestrator.
    fn auto_merge_fleet(tag: &str) -> Arc<Hub> {
        let hub = Arc::new(hub(tag));
        hub.append(Event::MarkedOrchestrator { session_id: "o".into() }).unwrap();
        hub.append(Event::Hook(alondra_core::HookEvent { session_id: "o".into(), event: "SessionStart".into(), ..Default::default() })).unwrap();
        hub.append(Event::Launched { name: "app/fix".into(), request_id: None, auto_merge: true, task: None, title: None, session: None }).unwrap();
        hub.append(Event::DaemonSeen(alondra_core::DaemonObservation { session_id: "s".into(), short_id: "s".into(), name: Some("app/fix".into()), pid: Some(1), ..Default::default() })).unwrap();
        hub.append(Event::PrOpened { url: "https://github.com/a/b/pull/7".into(), session_id: "s".into(), task: None }).unwrap();
        hub
    }

    fn checks_passed_at(hub: &Hub, head: &str) {
        let mut pr = hub.read(|v| v.prs[&1].clone());
        (pr.fetched, pr.head_sha, pr.mergeable) = (true, Some(head.into()), Some("MERGEABLE".into()));
        pr.checks = vec![Check { name: "lint".into(), state: CheckState::Passed, url: None }];
        hub.append(Event::PrUpdated(pr)).unwrap();
    }

    fn said_to(hub: &Hub, session: &str) -> Vec<String> {
        hub.read(|v| v.messages.values().filter(|m| m.session_id == session).map(|m| m.text.clone()).collect())
    }

    /// Alondra merged from a card up to two minutes old with a bare `gh pr merge`: a push in
    /// between went in with checks nobody saw pass.
    #[tokio::test]
    async fn alondra_merges_only_the_head_whose_checks_it_saw_pass() {
        let hub = auto_merge_fleet("pinned");
        let gh = FakeGh::default();
        checks_passed_at(&hub, "a2");
        auto_merge(&hub, &mut AutoMerging::default(), &gh).await;
        assert_eq!(*gh.heads.lock().unwrap(), [Some("a2".to_string())]);
        assert_eq!(merge_args("https://github.com/a/b/pull/7", MergeMethod::Squash, Some("a2"))[4..], ["--match-head-commit", "a2"]);
        assert_eq!(merge_args("https://github.com/a/b/pull/7", MergeMethod::Squash, None).len(), 4, "David's merge from the card is not pinned");
    }

    /// Any merge GitHub refused went into `told` and was never tried again until a push, while
    /// the node was told to fix what it could not.
    #[tokio::test]
    async fn a_merge_github_refused_is_tried_again_later_and_only_a_conflict_reaches_the_node() {
        let hub = auto_merge_fleet("retry");
        let gh = FakeGh { refusals: std::sync::Mutex::new(["GraphQL: Base branch policy prohibits the merge"].into()), ..Default::default() };
        let mut merging = AutoMerging { retry_after: Duration::from_millis(150), ..AutoMerging::default() };
        checks_passed_at(&hub, "a1");
        auto_merge(&hub, &mut merging, &gh).await;
        assert!(gh.merged.lock().unwrap().is_empty());
        assert!(!said_to(&hub, "s").iter().any(|t| t.contains("could not merge")), "the node cannot fix a branch policy: {:?}", said_to(&hub, "s"));
        let told = |hub: &Hub| said_to(hub, "o").into_iter().filter(|t| t.contains("Base branch policy prohibits the merge")).count();
        assert_eq!(told(&hub), 1);
        auto_merge(&hub, &mut merging, &gh).await;
        assert_eq!(gh.heads.lock().unwrap().len(), 1, "not tried again right away");
        tokio::time::sleep(Duration::from_millis(200)).await;
        auto_merge(&hub, &mut merging, &gh).await;
        assert_eq!(*gh.merged.lock().unwrap(), ["https://github.com/a/b/pull/7"], "tried again once the wait is over");
        assert_eq!(told(&hub), 1, "the orchestrator heard it once");
    }

    /// Round 2: a merge GitHub refuses for good (a branch policy) was tried every 5 min forever,
    /// each try a `gh pr merge` and a `gh pr view`.
    #[tokio::test]
    async fn a_merge_github_keeps_refusing_is_tried_less_and_less_often_and_at_once_after_a_push() {
        let hub = auto_merge_fleet("backoff");
        let gh = FakeGh { refusals: std::sync::Mutex::new(["GraphQL: Base branch policy prohibits the merge"; 5].into()), ..Default::default() };
        let mut merging = AutoMerging::default();
        let tries = |gh: &FakeGh| gh.heads.lock().unwrap().len();
        let age = |merging: &mut AutoMerging, by: Duration| {
            for (at, _) in merging.failed.values_mut() {
                *at = at.checked_sub(by).expect("the machine has been up longer than that");
            }
        };
        checks_passed_at(&hub, "a1");
        auto_merge(&hub, &mut merging, &gh).await;
        assert_eq!(tries(&gh), 1);
        age(&mut merging, MERGE_RETRY + Duration::from_secs(1));
        auto_merge(&hub, &mut merging, &gh).await;
        assert_eq!(tries(&gh), 2, "the first wait is 5 min");
        age(&mut merging, MERGE_RETRY + Duration::from_secs(1));
        auto_merge(&hub, &mut merging, &gh).await;
        assert_eq!(tries(&gh), 2, "refused twice, it waits twice as long");
        age(&mut merging, MERGE_RETRY);
        auto_merge(&hub, &mut merging, &gh).await;
        assert_eq!(tries(&gh), 3);
        assert_eq!((merging.wait(3), merging.wait(9), merging.wait(40)), (4 * MERGE_RETRY, MERGE_RETRY_MAX, MERGE_RETRY_MAX), "doubling up to every 2 h");

        checks_passed_at(&hub, "a2");
        auto_merge(&hub, &mut merging, &gh).await;
        assert_eq!(tries(&gh), 4, "a push is tried at once");
        let told: Vec<String> = said_to(&hub, "o").into_iter().filter(|t| t.contains("Base branch policy prohibits the merge")).collect();
        assert_eq!(told.len(), 2, "the orchestrator heard it once per head: {told:?}");
        assert!(told[0].contains("It tries again in 5 min, then less and less often while GitHub refuses it (at most every 120 min)"), "{}", told[0]);
    }

    #[test]
    fn a_set_aside_pr_under_an_open_one_is_still_followed_until_github_merges_it() {
        let mut v = View::default();
        for (n, base, head, state) in [(1, "main", "x", PrState::Dismissed), (2, "x", "y", PrState::Open), (3, "main", "z", PrState::Dismissed)] {
            let mut pr = PullRequest::opened(n, &format!("https://github.com/a/b/pull/{n}"), "s", None, 0).unwrap();
            (pr.base, pr.head, pr.state) = (Some(base.into()), Some(head.into()), state);
            v.prs.insert(n, pr);
        }
        assert_eq!(watched(&v).map(|p| p.id).collect::<Vec<_>>(), [1, 2], "the set-aside PR holding up another, not the one alone");
        let still_open = refreshed(&v.prs[&1], &json!({"state": "OPEN", "baseRefName": "main", "headRefName": "x"}), None);
        v.apply(1, 1, &Event::PrUpdated(still_open));
        assert_eq!(v.merge_blocked_by(2).map(|p| p.id), Some(1), "still open on GitHub");
        let merged = refreshed(&v.prs[&1], &json!({"state": "MERGED", "baseRefName": "main", "headRefName": "x"}), None);
        v.apply(2, 2, &Event::PrUpdated(merged));
        assert_eq!(v.merge_blocked_by(2), None, "merged on GitHub");
        assert_eq!(watched(&v).map(|p| p.id).collect::<Vec<_>>(), [2]);
    }

    #[test]
    fn a_command_counts_as_making_a_pr_only_when_it_runs_gh_pr_create() {
        for (command, runs) in [
            ("sed -n 680,715p crates/alondrad/src/http.rs; grep -rn 'fn pr_url\\|pull/\\|gh pr create' crates/", false),
            ("grep -rn \"a|gh pr create\" .", false),
            ("echo gh pr create", false),
            ("git log --grep 'gh pr create'", false),
            ("gh pr create --title x --body \"see https://github.com/a/b/pull/7\"", true),
            ("cd x && gh pr create --fill", true),
            ("git push -u origin b\ngh pr create --fill | tee out", true),
            ("false || GH_TOKEN=t command gh pr create --fill", true),
            ("gh pr view 12", false),
        ] {
            assert_eq!(runs_pr_create(command), runs, "{command}");
        }
    }

    #[test]
    fn the_pr_made_is_the_url_gh_prints_last_on_its_own_line() {
        for (response, pr) in [
            (json!({"stdout": "https://github.com/o/r/pull/12\n"}), Some("https://github.com/o/r/pull/12")),
            (json!({"stdout": "Warning: 1 uncommitted change\nhttps://github.com/o/r/pull/12\n\n"}), Some("https://github.com/o/r/pull/12")),
            (json!({"stdout": "https://github.com/o/r/pull/12\ndone\n"}), None),
            (json!({"stdout": "see https://github.com/a/b/pull/7\n"}), None),
            (json!({"stdout": "", "stderr": "https://github.com/o/r/pull/12"}), None),
            (json!("https://github.com/o/r/pull/12"), Some("https://github.com/o/r/pull/12")),
        ] {
            assert_eq!(created_pr(&response).as_deref(), pr, "{response}");
        }
    }

    #[test]
    fn only_a_conflicting_open_pr_is_asked_to_rebase() {
        let mut pr = PullRequest::opened(2, "https://github.com/a/b/pull/2", "s", None, 0).unwrap();
        pr.head = Some("y".into());
        for (mergeable, state, asked) in [("CONFLICTING", PrState::Open, true), ("MERGEABLE", PrState::Open, false), ("UNKNOWN", PrState::Open, false), ("CONFLICTING", PrState::Merged, false)] {
            (pr.mergeable, pr.state) = (Some(mergeable.into()), state);
            assert_eq!(rebase_request(&pr, "main").is_some(), asked, "{mergeable} {state:?}");
        }
    }
}
