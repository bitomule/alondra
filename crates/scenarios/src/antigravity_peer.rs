//! The Antigravity of a scenario. `agy acp` is started by the Antigravity host, which speaks to the
//! daemon in the same protocol as the other hosts and passes agy's `session/update` notifications
//! on as the payload of its events, beside its own `alondra/*` notices of the session and of each
//! prompt. The stand-in takes the host's place on its socket and says what the host said.
//!
//! One transcript per host the daemon starts, as for Codex.

use crate::claude_peer::PeerLauncher;
use crate::error::ScenarioError;
use alondra_scenarios_transcript::Transcript;
use std::path::Path;

/// What the Antigravity host announces in its hello (`ANTIGRAVITY_CAPABILITIES`).
const CAPABILITIES: &[&str] = &["streaming_input", "exact_decisions", "owner_attach", "commit_ack", "interrupt", "force_stop", "set_model", "set_permission_mode"];

pub fn launcher(case: &Path, names: &[String]) -> Result<PeerLauncher, ScenarioError> {
    let mut transcripts = Vec::new();
    for name in names {
        let path = case.join("transcripts").join(name);
        let text = std::fs::read_to_string(&path).map_err(|error| ScenarioError::Invalid(format!("{}: {error}", path.display())))?;
        let transcript = Transcript::parse(&text).map_err(|error| ScenarioError::Invalid(format!("{}: {error}", path.display())))?;
        if transcript.header.provider != "antigravity" {
            return Err(ScenarioError::Invalid(format!("{}: an Antigravity transcript names provider `antigravity`, not `{}`", path.display(), transcript.header.provider)));
        }
        transcripts.push(transcript);
    }
    Ok(PeerLauncher::declaring(CAPABILITIES, transcripts).with_app_server())
}
