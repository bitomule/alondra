//! Closing a session the way `claude rm` does, which keeps a worktree with work not yet saved
//! elsewhere, and refusing while the session still has something pending. An interactive
//! session is not a job `claude rm` knows: it only leaves the fleet and its terminal stays open.
//! Shared by the close route and by Alondra closing the node of a finished task.
use crate::domain_runtime::DomainRuntime;
use alondra_core::domain::AgentId;
use axum::http::StatusCode;
use serde::Serialize;
use serde_json::Value;
use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;
use std::time::Duration;

pub(crate) type Answer<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// What closing a session asks of the machine, so a test can stand in for Claude Code and git.
pub(crate) trait Sessions: Send + Sync + 'static {
    fn claude<'a>(&'a self, args: Vec<String>) -> Answer<'a, Result<String, String>>;
    fn leftovers<'a>(&'a self, session_id: &'a str) -> Answer<'a, Option<String>>;
}

pub(crate) struct Machine;

impl Sessions for Machine {
    fn claude<'a>(&'a self, args: Vec<String>) -> Answer<'a, Result<String, String>> {
        Box::pin(async move {
            let run = tokio::process::Command::new("claude").args(&args).kill_on_drop(true).output();
            match tokio::time::timeout(Duration::from_secs(60), run).await {
                Ok(Ok(out)) => Ok(format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr)).trim().to_string()),
                Ok(Err(e)) => Err(e.to_string()),
                Err(_) => Err(format!("claude {} did not answer in 60 s", args.join(" "))),
            }
        })
    }

    fn leftovers<'a>(&'a self, session_id: &'a str) -> Answer<'a, Option<String>> {
        Box::pin(crate::worktree::leftovers(session_id))
    }
}

/// A machine-readable reason a close was refused, so each client can show it in the user's
/// language instead of the English `error` string.
#[derive(Serialize, Debug, PartialEq)]
pub(crate) struct Reason {
    pub(crate) kind: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) detail: Option<String>,
}

/// Real risks a close must not paper over: the session still doing something (busy or a live
/// permission hold), or work left uncommitted or unpushed in its worktree.
fn reasons(busy: bool, waiting_permission: bool, worktree_leftovers: Option<String>) -> Vec<Reason> {
    if busy {
        return vec![Reason { kind: "busy", detail: None }];
    }
    let mut reasons = vec![];
    if waiting_permission {
        reasons.push(Reason { kind: "permission", detail: None });
    }
    if let Some(left) = worktree_leftovers {
        reasons.push(Reason { kind: "worktree", detail: Some(left) });
    }
    reasons
}

pub(crate) fn listed(agents_json: &str, short: &str) -> bool {
    serde_json::from_str::<Value>(agents_json)
        .ok()
        .and_then(|v| v.as_array().cloned())
        .is_some_and(|a| a.iter().any(|x| x.get("id").and_then(Value::as_str) == Some(short)))
}

pub(crate) enum CloseOutcome {
    /// `claude rm` removed it and `claude agents` no longer lists it.
    Closed { output: String },
    /// `claude agents` does not list it: an interactive session, or one already deleted.
    NotListed,
    /// Something real stops the close: busy, a pending permission, unsaved work.
    Refused { reasons: Vec<Reason>, message: String },
    /// `claude` failed or its answer was not the expected one.
    Failed { status: StatusCode, message: String },
}

fn failed(status: StatusCode, message: impl Into<String>) -> CloseOutcome {
    CloseOutcome::Failed { status, message: message.into() }
}

/// Closes the session of `agent`. A session `claude agents` does not list is left alone, and
/// leaves the fleet only when `leave_if_not_listed` says so.
pub(crate) async fn close_node(
    domain: &DomainRuntime,
    sessions: &dyn Sessions,
    agent: &AgentId,
    leave_if_not_listed: bool,
) -> anyhow::Result<CloseOutcome> {
    let agent_id = agent.0.as_str();
    let node = domain.live.node(agent_id);
    let short: String = node.as_ref().map_or_else(|| agent_id.chars().take(8).collect(), |node| node.short_id.clone());
    let (busy, waiting_permission) = node.as_ref().map_or((false, false), |node| (node.busy, node.waiting_permission));
    let listing = match sessions.claude(vec!["agents".into(), "--json".into(), "--all".into()]).await {
        Ok(listing) => listing,
        Err(e) => return Ok(failed(StatusCode::BAD_GATEWAY, e)),
    };
    if !listed(&listing, &short) {
        if leave_if_not_listed {
            crate::node_revival::user_closed(domain, agent_id)?;
        }
        return Ok(CloseOutcome::NotListed);
    }
    let leftovers = sessions.leftovers(agent_id).await;
    let reasons = reasons(busy, waiting_permission, leftovers);
    if !reasons.is_empty() {
        let message = reasons.iter().map(|r| r.detail.clone().unwrap_or_else(|| r.kind.to_string())).collect::<Vec<_>>().join("; ");
        return Ok(CloseOutcome::Refused { message: format!("{short} still has something pending, so it was not closed: {message}"), reasons });
    }
    let watched = domain.live.begin_watch(agent_id);
    let removed = remove(domain, sessions, agent_id, &short).await;
    if watched {
        domain.live.end_watch(agent_id);
    }
    if !matches!(removed, Ok(CloseOutcome::Closed { .. })) && !domain.live.is_closed(agent_id) {
        domain.live.losses.report(agent_id);
    }
    removed
}

/// `claude rm` until `claude agents` no longer lists the session. Its connection drops on the way,
/// so the caller holds the session's watch: the revival must not restart what is being closed.
async fn remove(domain: &DomainRuntime, sessions: &dyn Sessions, agent_id: &str, short: &str) -> anyhow::Result<CloseOutcome> {
    let short = short.to_owned();
    let job = std::fs::read_to_string(crate::watcher::claude_home().join("jobs").join(&short).join("state.json"))
        .ok()
        .and_then(|t| serde_json::from_str::<Value>(&t).ok());
    let worktree = job.as_ref().and_then(|v| v.get("worktreePath").and_then(Value::as_str).map(PathBuf::from));
    let branch = job.as_ref().and_then(|v| v.get("worktreeBranch").and_then(Value::as_str).map(str::to_string));
    let mut cleared_broken = false;
    let mut said = String::new();
    for _ in 0..3 {
        match sessions.claude(vec!["rm".into(), short.clone()]).await {
            Ok(text) if text.contains(&format!("removed {short}")) => said = text,
            Ok(text) if !cleared_broken && text.contains("Git couldn't remove it") => {
                let (Some(dir), Some(branch)) = (worktree.as_ref(), branch.as_deref()) else {
                    return Ok(failed(StatusCode::CONFLICT, text));
                };
                if let Err(why) = discard_if_identical(dir, branch).await {
                    return Ok(failed(StatusCode::CONFLICT, format!("{text}\n\nAlondra did not delete it: {why}")));
                }
                cleared_broken = true;
                continue;
            }
            Ok(text) => return Ok(failed(StatusCode::CONFLICT, text)),
            Err(e) => return Ok(failed(StatusCode::BAD_GATEWAY, e)),
        }
        match sessions.claude(vec!["agents".into(), "--json".into(), "--all".into()]).await {
            Ok(list) if !listed(&list, &short) => {
                if let Some(dir) = worktree.as_ref().filter(|d| d.exists()) {
                    return Ok(failed(StatusCode::CONFLICT, format!("{short} is gone from claude agents but its worktree is still at {}", dir.display())));
                }
                crate::node_revival::user_closed(domain, agent_id)?;
                return Ok(CloseOutcome::Closed { output: said });
            }
            Ok(_) => continue,
            Err(e) => return Ok(failed(StatusCode::BAD_GATEWAY, e)),
        }
    }
    Ok(failed(StatusCode::CONFLICT, format!("claude rm said '{said}' but {short} is still listed by claude agents")))
}

/// A worktree whose git link is broken can only be deleted by hand. It is deleted only when
/// its files are exactly its branch, it has no new files and the branch is fully pushed.
async fn discard_if_identical(dir: &std::path::Path, branch: &str) -> Result<(), String> {
    let link = std::fs::read_to_string(dir.join(".git")).map_err(|e| format!("no .git link: {e}"))?;
    let gitdir = link.trim().strip_prefix("gitdir: ").ok_or("unexpected .git link")?;
    let repo_git = PathBuf::from(gitdir).parent().and_then(|p| p.parent()).map(PathBuf::from).ok_or("unexpected gitdir")?;
    let index = scratch_index();
    let git = |args: Vec<String>| {
        let repo_git = repo_git.clone();
        let index = index.clone();
        async move {
            let out = tokio::process::Command::new("git")
                .env("GIT_INDEX_FILE", &index)
                .arg(format!("--git-dir={}", repo_git.display()))
                .args(&args)
                .output()
                .await
                .map_err(|e| e.to_string())?;
            if !out.status.success() {
                return Err(String::from_utf8_lossy(&out.stderr).trim().to_string());
            }
            Ok::<String, String>(String::from_utf8_lossy(&out.stdout).trim().to_string())
        }
    };
    let tree = format!("--work-tree={}", dir.display());
    git(vec!["read-tree".into(), branch.into()]).await?;
    let differs = git(vec![tree.clone(), "diff".into(), "--stat".into()]).await;
    let _ = std::fs::remove_file(&index);
    if !differs?.is_empty() {
        return Err(format!("its files differ from the branch {branch}"));
    }
    git(vec!["read-tree".into(), branch.into()]).await?;
    let untracked = git(vec![tree, "ls-files".into(), "--others".into(), "--exclude-standard".into()]).await;
    let _ = std::fs::remove_file(&index);
    if !untracked?.is_empty() {
        return Err("it has files that are not in git".into());
    }
    if git(vec!["rev-list".into(), "--count".into(), format!("origin/{branch}..{branch}")]).await? != "0" {
        return Err(format!("the branch {branch} has commits that are not pushed"));
    }
    tokio::fs::remove_dir_all(dir).await.map_err(|e| e.to_string())
}

/// A private index per call: two closes in the same millisecond must not share one.
fn scratch_index() -> PathBuf {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let n = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    std::env::temp_dir().join(format!("alondra-index-{}-{}-{n}", std::process::id(), alondra_core::now_millis()))
}
