//! Scripted end-to-end journeys against a sandbox started with `FAKES=1`: a phone acts as the user,
//! the fakes act as Claude and GitHub, and every case writes what it saw to the run's report.
mod local;
mod reopen;
mod report;
mod sdk;
pub mod seed;
mod session;
mod steps;
mod task_cycle;
mod user;

use std::{path::PathBuf, time::Duration};

use anyhow::{Result, bail};

use crate::{
    fake::{self, Fakes},
    sandbox::Sandbox,
};
pub(crate) use local::pair_terminal;
pub use report::Report;
pub use user::User;

/// How long a step waits for what another process applies on its own.
pub const PATIENCE: Duration = Duration::from_secs(60);

pub const NAMES: [&str; 4] = ["task-cycle", "session", "local", "reopen"];
/// Journeys that need a sandbox started for them, which `all` does not run.
pub const OPT_IN: [&str; 1] = ["sdk"];

pub fn run(sandbox: &Sandbox, names: &[String]) -> Result<()> {
    if let Some(unknown) = names.iter().find(|name| *name != "all" && !NAMES.contains(&name.as_str()) && !OPT_IN.contains(&name.as_str())) {
        bail!("unknown journey {unknown}: {}, {} or all", NAMES.join(", "), OPT_IN.join(", "));
    }
    let out = std::env::var_os("TEST_UNDECLARED_OUTPUTS_DIR").map(PathBuf::from).unwrap_or_else(|| sandbox.dir.join("journeys"));
    let fakes = Fakes::open(&sandbox.dir)?;
    let mut report = Report::new(out)?;
    let all = names.is_empty() || names.iter().any(|name| name == "all");
    for name in NAMES.into_iter().chain(OPT_IN) {
        let opted = OPT_IN.contains(&name);
        if !names.iter().any(|wanted| wanted == name) && (opted || !all) {
            continue;
        }
        report.setup(&format!("Before {name}: the tasks earlier journeys left open are cancelled and no node works"), |report| {
            clear_fleet(report, sandbox, &fakes)
        });
        match name {
            "task-cycle" => task_cycle::run(&mut report, sandbox, &fakes),
            "local" => local::run(&mut report, sandbox, &fakes),
            "session" => session::run(&mut report, sandbox, &fakes),
            "reopen" => reopen::run(&mut report, sandbox, &fakes),
            "sdk" => sdk::run(&mut report, sandbox, &fakes),
            _ => unreachable!(),
        }
    }
    report.finish(&fakes, sandbox)
}

/// Cancels on every Mac each scripted task still open and waits until no node works, so a journey
/// starts with every slot free whatever the ones before it left behind.
fn clear_fleet(report: &mut Report, sandbox: &Sandbox, fakes: &Fakes) -> Result<()> {
    let tasks: Vec<String> = fakes.request_scripts()?.into_iter().flat_map(|script| script.tasks).map(|task| task.task_id).collect();
    for number in 1..=sandbox.mac_count() {
        let user = report.step(&format!("a phone pairs directly with Mac {number} to tidy it"), || User::paired(sandbox, "tidy", number, true))?;
        for task in &tasks {
            let Ok(view) = user.view(&format!("task:{task}")) else { continue };
            if user.action(&view, "cancel:").is_some() {
                steps::press(report, &user, &view, "cancel:", None, None, &format!("Mac {number} cancels {task}"))?;
            }
        }
    }
    steps::wait(report, "no node works", || {
        let working: Vec<String> = fakes.all_nodes()?.into_iter().filter(|node| node.state == fake::node::State::Running).map(|node| node.short).collect();
        Ok((working.is_empty().then_some(()), format!("still running: {working:?}")))
    })
}
