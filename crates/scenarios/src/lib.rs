//! Conversation scenarios: the real daemon, with every loop it runs, assembled over fake edges
//! (GitHub, the clock, the push transport, the Mac, the provider's process) and driven the way its
//! users drive it. A scenario passes through checkpoints at which what the apps would show and the
//! pushes sent are compared with what a reviewer accepted.

pub mod checkpoint;
pub mod claude_host;
pub mod claude_peer;
pub mod codex_peer;
pub mod error;
pub mod format;
pub mod from_report;
pub mod http;
pub mod runner;

pub use error::ScenarioError;
pub use format::Scenario;
pub use runner::{Outcome, run, run_partial, run_with};

#[cfg(test)]
mod harness_tests;

use std::path::{Path, PathBuf};

/// The folders of `cases_root` that hold a case, by name.
pub fn cases(cases_root: &Path) -> std::io::Result<Vec<PathBuf>> {
    let mut found: Vec<PathBuf> = std::fs::read_dir(cases_root)?
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .filter(|path| path.join("scenario.toml").is_file())
        .collect();
    found.sort();
    Ok(found)
}

pub fn load(case: &Path) -> Result<Scenario, ScenarioError> {
    let text = std::fs::read_to_string(case.join("scenario.toml"))
        .map_err(|error| ScenarioError::Invalid(format!("{}: {error}", case.join("scenario.toml").display())))?;
    let mut scenario = Scenario::parse(&text)?;
    scenario.case = Some(case.to_path_buf());
    Ok(scenario)
}

/// Runs the case and compares every checkpoint with the files accepted in its folder.
pub fn check(case: &Path) -> Result<Outcome, ScenarioError> {
    run(&load(case)?, Some(case))
}

/// Runs the case and writes what its checkpoints read as the accepted files.
pub fn accept(case: &Path) -> Result<Outcome, ScenarioError> {
    let outcome = run(&load(case)?, None)?;
    for (name, files) in &outcome.checkpoints {
        checkpoint::write(case, name, files).map_err(|error| ScenarioError::Invalid(error.to_string()))?;
    }
    Ok(outcome)
}

/// Runs the case of a bug not fixed yet and writes the checkpoints it passed before the failure
/// that shows the bug, which is returned. The fix accepts the rest.
pub fn accept_until_failure(case: &Path) -> Result<Option<ScenarioError>, ScenarioError> {
    let (outcome, failure) = run_partial(&load(case)?);
    for (name, files) in &outcome.checkpoints {
        checkpoint::write(case, name, files).map_err(|error| ScenarioError::Invalid(error.to_string()))?;
    }
    Ok(failure)
}
