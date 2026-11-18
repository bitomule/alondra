//! What Alondra tells about a repository Claude Code does not trust. Reading and writing that trust
//! is the Mac's (`Accounts` in `engine-ports`).
use std::path::Path;

/// Why a session cannot start in `repository`, for the orchestrator and the nodes.
pub fn instructions(repository: &Path) -> String {
    format!(
        "Claude Code does not trust {repo}, so Alondra cannot start a session there. The user trusts it from the card Alondra shows them, or on the Mac by running `claude` in {repo} and accepting the trust prompt.",
        repo = repository.display()
    )
}
