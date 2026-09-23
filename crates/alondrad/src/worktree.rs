use std::path::PathBuf;

use serde_json::Value;
use tokio::process::Command;

/// What a node would leave behind in its worktree: uncommitted files or commits no remote has.
/// None when the node has no worktree of its own or it is clean.
pub async fn leftovers(session_id: &str) -> Option<String> {
    let short = session_id.get(..8)?;
    let job = std::fs::read_to_string(crate::watcher::claude_home().join("jobs").join(short).join("state.json")).ok()?;
    let dir = serde_json::from_str::<Value>(&job).ok()?.get("worktreePath")?.as_str().map(PathBuf::from)?;
    if !dir.exists() {
        return None;
    }
    let git = |args: &'static [&'static str]| {
        let dir = dir.clone();
        async move {
            let out = Command::new("git").arg("-C").arg(&dir).args(args).output().await.ok()?;
            out.status.success().then(|| String::from_utf8_lossy(&out.stdout).trim().to_string())
        }
    };
    let changed = git(&["status", "--porcelain"]).await?.lines().count();
    let unpushed: usize = git(&["rev-list", "--count", "HEAD", "--not", "--remotes"]).await?.parse().ok()?;
    let mut parts = vec![];
    if changed > 0 {
        parts.push(format!("{changed} uncommitted file(s)"));
    }
    if unpushed > 0 {
        parts.push(format!("{unpushed} commit(s) not pushed"));
    }
    (!parts.is_empty()).then(|| format!("{} in {}", parts.join(" and "), dir.display()))
}
