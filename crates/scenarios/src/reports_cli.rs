//! The `alondra` CLI a scenario's daemon runs for a crash report held on the Mac: it does to the
//! report's `sending.json` what the real one does when GitHub takes it (`report send`) or when the
//! user keeps it (`report keep`). Any other command it is asked for is a leak; every command that
//! is not the CLI goes on to the edge it wraps.
use alondra_agent_runtime::owned_host::{HostLauncher, HostSpawn, Launcher, ProbeOutput, ProbeRun};
use serde_json::{Value, json};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

pub const SCENARIO_CLI: &str = "/scenario/alondra";

pub struct ReportsCli {
    pub inner: Launcher,
    pub leaks: Arc<Mutex<Vec<String>>>,
}

impl HostLauncher for ReportsCli {
    fn spawn(&self, spawn: HostSpawn<'_>) -> std::io::Result<std::process::Child> {
        self.inner.get().spawn(spawn)
    }

    fn run(&self, run: ProbeRun) -> std::io::Result<ProbeOutput> {
        if run.command.get_program() != SCENARIO_CLI {
            return self.inner.get().run(run);
        }
        let args: Vec<String> = run.command.get_args().map(|arg| arg.to_string_lossy().into_owned()).collect();
        let home = run.command.get_envs().find(|(key, _)| *key == "ALONDRA_HOME").and_then(|(_, value)| value).map(PathBuf::from);
        let args: Vec<&str> = args.iter().map(String::as_str).collect();
        let done = match (args.as_slice(), home) {
            (["report", verb @ ("send" | "keep"), id], Some(home)) => apply(&home.join("reports").join(id).join("sending.json"), verb),
            _ => None,
        };
        match done {
            Some(()) => Ok(ProbeOutput { success: true, ..ProbeOutput::default() }),
            None => {
                let what = format!("process alondra {}", args.join(" "));
                self.leaks.lock().unwrap_or_else(std::sync::PoisonError::into_inner).push(what.clone());
                Err(std::io::Error::other(format!("a scenario runs no {what}")))
            }
        }
    }
}

fn apply(path: &std::path::Path, verb: &str) -> Option<()> {
    let mut sending: Value = serde_json::from_slice(&std::fs::read(path).ok()?).ok()?;
    let sending_map = sending.as_object_mut()?;
    let held = sending_map.remove("held");
    if verb == "send" {
        let current = held?.get("current")?.clone();
        sending_map.insert("repo".into(), current);
        sending_map.insert("state".into(), json!("sent"));
    } else {
        sending_map.insert("state".into(), json!("kept"));
    }
    std::fs::write(path, sending.to_string()).ok()
}
