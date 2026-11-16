//! The Codex of a scenario. The app-server is started by the Codex host, which speaks to the
//! daemon in the same protocol as the Claude host and passes the app-server's JSON-RPC messages on
//! as the payload of its events, so the stand-in takes the host's place on its socket and says what
//! the app-server said: the notifications of a turn (`turn/started`, `item/agentMessage/delta`,
//! `turn/completed`) as Codex writes them, beside the `alondra/*` notices of the host.
//!
//! One transcript per host the daemon starts: the first plays the turn that is cut, the next the
//! host that resumes the thread.

use crate::claude_peer::PeerLauncher;
use crate::error::ScenarioError;
use std::path::Path;

/// What the Codex host announces in its hello, and the daemon requires of it for a task.
const CAPABILITIES: &[&str] = &[
    "streaming_input", "owner_attach", "commit_ack", "exact_decisions", "native_account_policy", "causal_child_continuations",
    "interrupt", "force_stop", "set_model", "set_permission_mode", "mcp_status", "mcp_sign_in",
];

pub fn launcher(case: &Path, names: &[String]) -> Result<PeerLauncher, ScenarioError> {
    Ok(PeerLauncher::declaring(CAPABILITIES, crate::claude_peer::transcripts(case, names, "codex")?).with_app_server())
}
