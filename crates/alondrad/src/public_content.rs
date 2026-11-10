//! Checks configured private names before a node delivers a public pull request.
use std::path::Path;
use std::time::Duration;

use alondra_core::config::{FileConfig, path_in};
use alondra_core::domain::{
    Command, DeliveryTarget, DomainError, DomainState, InteractionId, PrState, PullRequest, TaskId,
};
use serde::Deserialize;
use serde_json::Value;

use crate::domain_github::GhCli;

const QUERY: &str = "query($owner:String!,$name:String!,$number:Int!){repository(owner:$owner,name:$name){visibility pullRequest(number:$number){title body headRefOid baseRefOid baseRefName changedFiles}}}";
const LIMIT: Duration = Duration::from_secs(30);
const MAX_FINDINGS: usize = 20;

#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "UPPERCASE")]
enum Visibility {
    Public,
    Private,
    Internal,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
struct Snapshot {
    title: String,
    body: String,
    head_ref_oid: String,
    base_ref_oid: String,
    base_ref_name: String,
    changed_files: usize,
}

#[derive(Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
struct Repository {
    visibility: Visibility,
    pull_request: Snapshot,
}

fn unavailable(reason: &str) -> DomainError {
    DomainError::new(
        "public_content_review_unavailable",
        format!(
            "Public content review could not finish: {reason}. Fix the read and deliver again."
        ),
    )
}

async fn snapshot(gh: &GhCli, pr: &PullRequest) -> Result<Repository, DomainError> {
    let (owner, name) = pr
        .repository
        .split_once('/')
        .ok_or_else(|| unavailable("invalid repository"))?;
    let host = alondra_core::domain::normalize_pr_host(&pr.host);
    let args = [
        format!("query={QUERY}"),
        format!("owner={owner}"),
        format!("name={name}"),
        format!("number={}", pr.number),
    ];
    let bytes = gh
        .run(
            &[
                "api",
                "graphql",
                "--hostname",
                &host,
                "-f",
                &args[0],
                "-f",
                &args[1],
                "-f",
                &args[2],
                "-F",
                &args[3],
            ],
            None,
            LIMIT,
        )
        .await
        .map_err(|_| unavailable("GitHub did not return the repository and PR"))?;
    let value: Value =
        serde_json::from_slice(&bytes).map_err(|_| unavailable("invalid GitHub response"))?;
    if value
        .get("errors")
        .is_some_and(|errors| !errors.as_array().is_some_and(Vec::is_empty))
    {
        return Err(unavailable("GitHub could not read the repository and PR"));
    }
    serde_json::from_value(value["data"]["repository"].clone())
        .map_err(|_| unavailable("missing visibility or PR content"))
}

/// Literal names and their locations stay in the node's refusal, never in a GitHub comment.
fn find(names: &[String], text: &str, location: &str, findings: &mut Vec<String>) {
    let lower = text.to_lowercase();
    for name in names {
        if findings.len() < MAX_FINDINGS && lower.contains(&name.trim().to_lowercase()) {
            findings.push(format!("{name:?} in {location}"));
        }
    }
}

/// Git quotes non-ASCII path bytes as octal escapes even though the configured name is Unicode.
fn decoded_path(line: &str) -> Result<String, DomainError> {
    let mut bytes = line.bytes();
    let mut decoded = Vec::new();
    while let Some(byte) = bytes.next() {
        if byte != b'\\' {
            decoded.push(byte);
            continue;
        }
        let escaped = bytes
            .next()
            .ok_or_else(|| unavailable("incomplete quoted path"))?;
        match escaped {
            b'0'..=b'3' => {
                let second = bytes.next().filter(|b| matches!(*b, b'0'..=b'7'));
                let third = bytes.next().filter(|b| matches!(*b, b'0'..=b'7'));
                let (Some(second), Some(third)) = (second, third) else {
                    return Err(unavailable("invalid quoted path"));
                };
                decoded.push((escaped - b'0') * 64 + (second - b'0') * 8 + (third - b'0'));
            }
            b'n' => decoded.push(b'\n'),
            b't' => decoded.push(b'\t'),
            b'r' => decoded.push(b'\r'),
            b'b' => decoded.push(8),
            b'f' => decoded.push(12),
            b'v' => decoded.push(11),
            b'a' => decoded.push(7),
            b'\\' | b'"' => decoded.push(escaped),
            _ => return Err(unavailable("invalid quoted path escape")),
        }
    }
    String::from_utf8(decoded).map_err(|_| unavailable("a path is not UTF-8"))
}

fn inspect_diff(
    names: &[String],
    diff: &str,
    changed_files: usize,
    findings: &mut Vec<String>,
) -> Result<(), DomainError> {
    let mut files = 0;
    let mut in_hunk = false;
    let mut path = String::new();
    for (index, line) in diff.lines().enumerate() {
        if let Some(header) = line.strip_prefix("diff --git ") {
            files += 1;
            in_hunk = false;
            path = decoded_path(header)?;
            find(names, header, &format!("diff paths: {path}"), findings);
            find(names, &path, &format!("diff paths: {path}"), findings);
        } else if line.starts_with("@@ ") {
            in_hunk = true;
        } else if in_hunk {
            if let Some(added) = line.strip_prefix('+') {
                find(
                    names,
                    added,
                    &format!("{path}, diff line {}", index + 1),
                    findings,
                );
            }
        } else if let Some(new_path) = line.strip_prefix("+++ ") {
            path = decoded_path(new_path)?;
        }
    }
    if files != changed_files {
        return Err(unavailable("the diff does not contain every changed file"));
    }
    Ok(())
}

async fn review_pr(gh: &GhCli, pr: &PullRequest, names: &[String]) -> Result<(), DomainError> {
    let before = snapshot(gh, pr).await?;
    if before.visibility != Visibility::Public {
        return Ok(());
    }
    if before.pull_request.head_ref_oid != pr.head || before.pull_request.base_ref_name != pr.base {
        return Err(DomainError::new(
            "stale_head",
            "PR revision changed before public content review; refresh it and deliver again",
        ));
    }
    let url = format!(
        "https://{}/{}/pull/{}",
        alondra_core::domain::normalize_pr_host(&pr.host),
        pr.repository,
        pr.number
    );
    let (bytes, truncated) = crate::domain_api::bounded_diff(gh, &url)
        .await
        .map_err(|_| unavailable("could not read the PR diff"))?;
    if truncated {
        return Err(unavailable("the PR diff exceeds the review limit"));
    }
    let diff = std::str::from_utf8(&bytes).map_err(|_| unavailable("the diff is not UTF-8"))?;
    let mut findings = Vec::new();
    find(names, &before.pull_request.title, "PR title", &mut findings);
    for (index, line) in before.pull_request.body.lines().enumerate() {
        find(
            names,
            line,
            &format!("PR description line {}", index + 1),
            &mut findings,
        );
    }
    inspect_diff(
        names,
        diff,
        before.pull_request.changed_files,
        &mut findings,
    )?;
    if snapshot(gh, pr).await? != before {
        return Err(DomainError::new(
            "public_content_review_changed",
            "PR title, description, visibility or revision changed during public content review; deliver again",
        ));
    }
    if !findings.is_empty() {
        let mut error = DomainError::new(
            "private_names_in_public_content",
            format!(
                "Remove configured private names from public PR #{} before delivering:\n{}\nUp to {MAX_FINDINGS} matches are shown; review again after correcting them.",
                pr.number,
                findings.join("\n")
            ),
        );
        error.subject = Some(pr.id.0.clone());
        return Err(error);
    }
    Ok(())
}

/// PRs read from GitHub at once, so a delivery of many does not wait for each one in turn.
const CONCURRENT_REVIEWS: usize = 4;

/// Reviews every PR at once, at most `CONCURRENT_REVIEWS` together; the refusal of the first PR
/// in delivery order that has one is the one given.
async fn review_concurrently(
    gh: &GhCli,
    prs: Vec<PullRequest>,
    names: Vec<String>,
) -> Result<(), DomainError> {
    let permits = std::sync::Arc::new(tokio::sync::Semaphore::new(CONCURRENT_REVIEWS));
    let names = std::sync::Arc::new(names);
    let mut reviews = tokio::task::JoinSet::new();
    for (index, pr) in prs.into_iter().enumerate() {
        let (gh, permits, names) = (gh.clone(), permits.clone(), names.clone());
        reviews.spawn(async move {
            let _permit = permits.acquire().await.expect("the semaphore is never closed");
            (index, review_pr(&gh, &pr, &names).await)
        });
    }
    let mut refusals = Vec::new();
    while let Some(joined) = reviews.join_next().await {
        let (index, outcome) =
            joined.map_err(|_| unavailable("a pull request review stopped early"))?;
        if let Err(error) = outcome {
            refusals.push((index, error));
        }
    }
    match refusals.into_iter().min_by_key(|(index, _)| *index) {
        Some((_, error)) => Err(error),
        None => Ok(()),
    }
}

#[derive(Clone)]
pub(crate) struct Reviewed {
    task: TaskId,
    interaction: InteractionId,
    targets: Vec<DeliveryTarget>,
    prs: Vec<PullRequest>,
}

impl Reviewed {
    /// Recheck under the domain writer's lock without changing the command's receipt identity.
    pub(crate) fn verify(&self, state: &DomainState) -> Result<(), DomainError> {
        if state.interactions.contains_key(&self.interaction) {
            return Ok(());
        }
        let same = state.with_open_prs(&self.task, &self.targets) == self.targets
            && self
                .prs
                .iter()
                .all(|pr| state.pull_requests.get(&pr.id) == Some(pr));
        if same {
            Ok(())
        } else {
            Err(DomainError::new(
                "public_content_review_changed",
                "The task's PRs changed after public content review; deliver again",
            ))
        }
    }
}

pub(crate) async fn check(
    home: &Path,
    state: &DomainState,
    command: &Command,
    gh: &GhCli,
) -> Result<Option<Reviewed>, DomainError> {
    let Command::PresentResult {
        task_id,
        interaction_id,
        targets,
        is_final: true,
        ..
    } = command
    else {
        return Ok(None);
    };
    if state.interactions.contains_key(interaction_id) {
        return Ok(None);
    }
    let all = state.with_open_prs(task_id, targets);
    let prs: Vec<_> = all
        .iter()
        .filter_map(|target| match target {
            DeliveryTarget::PullRequest { pr_id, head } => state
                .pull_requests
                .get(pr_id)
                .filter(|pr| pr.state == PrState::Open)
                .map(|pr| (pr, head)),
            _ => None,
        })
        .collect();
    if prs.is_empty() {
        return Ok(None);
    }
    let file = match std::fs::read_to_string(path_in(home)) {
        Ok(text) => FileConfig::parse(&text).map_err(|_| unavailable("config.toml is invalid"))?,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => FileConfig::default(),
        Err(_) => return Err(unavailable("config.toml could not be read")),
    };
    let names = file.private_names.unwrap_or_default();
    if names.is_empty() {
        return Ok(None);
    }
    for (pr, head) in &prs {
        if head.as_str() != pr.head {
            return Err(DomainError::new(
                "stale_head",
                "Presented PR head does not match its registered head",
            ));
        }
    }
    review_concurrently(gh, prs.iter().map(|(pr, _)| (*pr).clone()).collect(), names).await?;
    Ok(Some(Reviewed {
        task: task_id.clone(),
        interaction: interaction_id.clone(),
        targets: all.clone(),
        prs: prs.into_iter().map(|(pr, _)| pr.clone()).collect(),
    }))
}

#[cfg(test)]
mod tests;
