//! Writes the secret the user types on a node's card to the file the node named, in the node's
//! worktree and in the repository's main checkout, so the value never enters the domain.

use std::path::{Path, PathBuf};
use std::process::Command;

use alondra_core::domain::{Author, DomainError, DomainState, Interaction, Scope, TaskId};
use alondra_core::secret_destination::SecretDestination;

fn git(dir: &Path, args: &[&str]) -> Option<PathBuf> {
    let out = Command::new("git").arg("-C").arg(dir).args(args).output().ok()?;
    let text = String::from_utf8_lossy(&out.stdout).trim().to_owned();
    (out.status.success() && !text.is_empty()).then(|| PathBuf::from(text))
}

/// The node's worktree and its repository's main checkout, the same folder once when the node
/// works in the main checkout.
pub(crate) fn checkouts_of(worktree: &Path) -> Result<Vec<PathBuf>, String> {
    let unknown = || "Alondra cannot tell which repository this task works in".to_owned();
    let own = git(worktree, &["rev-parse", "--show-toplevel"]).and_then(|dir| std::fs::canonicalize(dir).ok()).ok_or_else(unknown)?;
    let common = git(worktree, &["rev-parse", "--path-format=absolute", "--git-common-dir"])
        .and_then(|dir| std::fs::canonicalize(dir).ok()).ok_or_else(unknown)?;
    let main = common.parent().filter(|_| common.file_name().is_some_and(|name| name == ".git")).ok_or_else(unknown)?.to_path_buf();
    Ok(if main == own { vec![own] } else { vec![own, main] })
}

fn checkouts(state: &DomainState, session_id: &str, task: &TaskId) -> Result<Vec<PathBuf>, String> {
    let worktree = crate::worktree::node_worktree(state, session_id, task)
        .ok_or("Alondra cannot tell which repository this task works in")?;
    checkouts_of(&worktree)
}

/// Why a node cannot ask for a secret written to `destination`: it is not a git-ignored file
/// inside both its worktree and the main checkout.
pub fn ask_refusal(state: &DomainState, session_id: &str, task: &TaskId, destination: &SecretDestination) -> Option<DomainError> {
    let problem = checkouts(state, session_id, task).and_then(|roots| {
        roots.iter().find_map(|root| destination.place_problem(root)).map_or(Ok(()), Err)
    }).err()?;
    let mut refusal = DomainError::new("invalid_input", problem);
    refusal.subject = Some("destination".into());
    Some(refusal)
}

/// Writes `value` where the open card `interaction` says, and returns what the node is told
/// instead: nothing is written unless every checkout can take it.
pub fn write(state: &DomainState, interaction: &Interaction, value: &str) -> Result<String, String> {
    let destination = interaction.card.secret_destination.as_ref().ok_or("This card does not take a secret")?;
    let (Author::Agent { agent_id }, Scope::Task { task_id }) = (&interaction.author, &interaction.scope) else {
        return Err("Only a task's node asks for a secret".into());
    };
    let roots = checkouts(state, &agent_id.0, task_id)?;
    if let Some(problem) = roots.iter().find_map(|root| destination.place_problem(root)) {
        return Err(problem);
    }
    for root in &roots {
        destination.write(root, value)?;
    }
    Ok(destination.written())
}
