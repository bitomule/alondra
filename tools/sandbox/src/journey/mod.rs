//! Scripted end-to-end journeys against a sandbox started with `FAKES=1`: a phone acts as the user,
//! the fakes act as Claude and GitHub, and every case writes what it saw to the run's report.
mod local;
mod reopen;
mod report;
pub mod seed;
mod session;
mod steps;
mod task_cycle;
mod user;

use std::{path::PathBuf, time::Duration};

use anyhow::{Result, bail};

use crate::{fake::Fakes, sandbox::Sandbox};
pub(crate) use local::pair_terminal;
pub use report::Report;
pub use user::User;

/// How long a step waits for what another process applies on its own.
pub const PATIENCE: Duration = Duration::from_secs(60);

pub const NAMES: [&str; 4] = ["task-cycle", "session", "local", "reopen"];

pub fn run(sandbox: &Sandbox, names: &[String]) -> Result<()> {
    if let Some(unknown) = names.iter().find(|name| *name != "all" && !NAMES.contains(&name.as_str())) {
        bail!("unknown journey {unknown}: {} or all", NAMES.join(", "));
    }
    let out = std::env::var_os("TEST_UNDECLARED_OUTPUTS_DIR").map(PathBuf::from).unwrap_or_else(|| sandbox.dir.join("journeys"));
    let fakes = Fakes::open(&sandbox.dir)?;
    let mut report = Report::new(out)?;
    let all = names.is_empty() || names.iter().any(|name| name == "all");
    for name in NAMES {
        if !all && !names.iter().any(|wanted| wanted == name) {
            continue;
        }
        match name {
            "task-cycle" => task_cycle::run(&mut report, sandbox, &fakes),
            "local" => local::run(&mut report, sandbox, &fakes),
            "session" => session::run(&mut report, sandbox, &fakes),
            "reopen" => reopen::run(&mut report, sandbox, &fakes),
            _ => unreachable!(),
        }
    }
    report.finish(&fakes, sandbox)
}
