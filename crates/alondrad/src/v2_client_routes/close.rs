//! Closing a session the way `claude rm` does, which keeps a worktree with work not yet saved
//! elsewhere, and refusing while the session still has something pending. An interactive
//! session is not a job `claude rm` knows: it only leaves the fleet and its terminal stays open.
use super::{Client, Reply};
use alondra_core::domain_v2::{AgentId, DomainError};
use axum::http::StatusCode;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
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

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Body {
    #[serde(default)]
    #[allow(dead_code)]
    force: bool,
}

/// A machine-readable reason a close was refused, so each client can show it in the user's
/// language instead of the English `error` string.
#[derive(Serialize, Debug, PartialEq)]
struct Reason {
    kind: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    detail: Option<String>,
}

fn refused(message: impl Into<String>, reasons: Vec<Reason>) -> Reply {
    Reply { status: StatusCode::CONFLICT, body: json!({"error": message.into(), "reasons": reasons}) }
}

fn failed(status: StatusCode, message: impl Into<String>) -> Reply {
    Reply { status, body: json!({"error": message.into()}) }
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

fn listed(agents_json: &str, short: &str) -> bool {
    serde_json::from_str::<Value>(agents_json)
        .ok()
        .and_then(|v| v.as_array().cloned())
        .is_some_and(|a| a.iter().any(|x| x.get("id").and_then(Value::as_str) == Some(short)))
}

impl Client {
    pub(super) async fn close_session(&self, agent_id: &str, body: &Value) -> anyhow::Result<Reply> {
        if serde_json::from_value::<Body>(body.clone()).is_err() {
            return Ok(Reply::error(DomainError::new("invalid_input", "invalid close request")));
        }
        let agent = AgentId(agent_id.to_owned());
        let Some(known) = self.domain.state()?.agents.get(&agent).cloned() else {
            return Ok(Reply::error(DomainError::new("not_found", "agent does not exist")));
        };
        let sessions = self.sessions.lock().expect("session control poisoned").clone();
        let node = self.domain.live.node(agent_id);
        let short = node.as_ref().map_or_else(|| agent_id.chars().take(8).collect(), |node| node.short_id.clone());
        let name = node.as_ref().and_then(|node| node.name.clone()).unwrap_or(known.name);
        let (busy, waiting_permission) = node.as_ref().map_or((false, false), |node| (node.busy, node.waiting_permission));
        let listing = match sessions.claude(vec!["agents".into(), "--json".into(), "--all".into()]).await {
            Ok(listing) => listing,
            Err(e) => return Ok(failed(StatusCode::BAD_GATEWAY, e)),
        };
        if !listed(&listing, &short) {
            self.leave_fleet(&agent)?;
            return Ok(Reply::ok(json!({"ok": true, "interactive": true, "output": format!(
                "{name} is an interactive session, not a background job: Alondra removed it from the fleet and left the terminal session itself open. Close it there with /exit when you are done."
            )})));
        }
        let leftovers = sessions.leftovers(agent_id).await;
        let reasons = reasons(busy, waiting_permission, leftovers);
        if !reasons.is_empty() {
            let message = reasons.iter().map(|r| r.detail.clone().unwrap_or_else(|| r.kind.to_string())).collect::<Vec<_>>().join("; ");
            return Ok(refused(format!("{short} still has something pending, so it was not closed: {message}"), reasons));
        }
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
                    self.leave_fleet(&agent)?;
                    return Ok(Reply::ok(json!({"ok": true, "interactive": false, "output": said})));
                }
                Ok(_) => continue,
                Err(e) => return Ok(failed(StatusCode::BAD_GATEWAY, e)),
            }
        }
        Ok(failed(StatusCode::CONFLICT, format!("claude rm said '{said}' but {short} is still listed by claude agents")))
    }

    /// The session is gone from the fleet: the domain sees it disconnected, its capability is
    /// revoked, the tasks it had open wait for a new node and what the daemon observed of it is
    /// forgotten, as when its session ends.
    fn leave_fleet(&self, agent: &AgentId) -> anyhow::Result<()> {
        crate::node_revival::user_closed(&self.domain, &agent.0)
    }
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
