//! Checks the MCP servers of a project no node runs in. The Agent SDK host starts Claude in the
//! project and only asks it how its servers are: no message reaches Claude, so the model is never
//! called, and Claude keeps no session of it. Each check is the user's: nothing checks on its own.
use crate::locks::Poisonable as _;
use crate::sdk_owner::SdkBackend;
use alondra_core::domain::{McpServerState, McpStatus};
use alondra_core::views::{LiveMcpProbe, LiveMcpRemoval};
use alondra_foundation::McpOrigin;
use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::Mutex;
use std::time::Duration;

/// The host's flag for a check (`agent-host/probe.ts`).
const FLAG: &str = "--mcp-probe";
/// Claude starts, connects its servers and answers well within this.
const DEADLINE: Duration = Duration::from_secs(60);
const MAX_OUTPUT: u64 = 512 * 1024;

/// What the MCP servers screen knows beyond the domain while this daemon runs: each check, and the
/// servers the user deleted (`mcp_remove`).
#[derive(Default)]
pub(crate) struct McpProbes {
    /// A holder that panics leaves it whole, so it is recovered: a live view of each project's
    /// check that the next probe writes again.
    probes: Mutex<BTreeMap<String, LiveMcpProbe>>,
    /// A holder that panics leaves it whole, so it is recovered: a retain and a push of removal
    /// notes for the live screen.
    removed: Mutex<Vec<LiveMcpRemoval>>,
}

impl McpProbes {
    pub(crate) fn snapshot(&self) -> BTreeMap<String, LiveMcpProbe> {
        self.probes.lock_or_recover("the MCP probes").clone()
    }

    pub(crate) fn removals(&self) -> Vec<LiveMcpRemoval> {
        self.removed.lock_or_recover("the MCP removals").clone()
    }

    /// Keeps that the user deleted a server: of `project`'s own settings, or of their own for every
    /// project. The checks that found it there no longer list it.
    pub(crate) fn removed(&self, removal: LiveMcpRemoval, project: Option<&str>) {
        for (checked, probe) in self.probes.lock_or_recover("the MCP probes").iter_mut() {
            let origin = match project {
                None => McpOrigin::UserSettings,
                Some(project) if project == checked => McpOrigin::LocalSettings,
                Some(_) => continue,
            };
            if let LiveMcpProbe::Checked { servers, .. } = probe {
                servers.retain(|server| !removal.key.names(server) || server.traits.origin != origin);
            }
        }
        let mut removed = self.removed.lock_or_recover("the MCP removals");
        removed.retain(|kept| kept.key != removal.key);
        removed.push(removal);
    }

    /// Marks the project as being checked, unless it already is.
    fn begin(&self, project: &str) -> bool {
        let mut probes = self.probes.lock_or_recover("the MCP probes");
        if matches!(probes.get(project), Some(LiveMcpProbe::Checking)) {
            return false;
        }
        probes.insert(project.to_owned(), LiveMcpProbe::Checking);
        true
    }

    pub(crate) fn finish(&self, project: &str, probe: LiveMcpProbe) {
        self.probes.lock_or_recover("the MCP probes").insert(project.to_owned(), probe);
    }
}

/// What the host wrote: the servers, or why it could not list them.
fn parse(output: &str) -> Result<Vec<McpServerState>, String> {
    let value: serde_json::Value = serde_json::from_str(output.lines().next().unwrap_or_default())
        .map_err(|_| "Claude gave no answer".to_owned())?;
    if let Some(error) = value["error"].as_str() {
        return Err(error.chars().take(240).collect());
    }
    let servers = value["servers"].as_array().ok_or_else(|| "Claude gave no answer".to_owned())?;
    let text = |server: &serde_json::Value, field: &str| server[field].as_str().map(str::to_owned);
    Ok(servers.iter().filter_map(|server| {
        let status = server["status"].as_str()?;
        let (source, transport) = (text(server, "source"), text(server, "transport"));
        Some(McpServerState {
            name: text(server, "name")?,
            status: McpStatus::of(status),
            error: text(server, "error"),
            traits: alondra_provider_claude::mcp_traits(source.as_deref(), transport.as_deref(), status),
            source,
            transport,
            url: text(server, "url"),
        })
    }).take(alondra_core::domain::MAX_MCP_SERVERS).collect())
}

/// Starts the host in `directory` as a check and reads what it answers, within `DEADLINE`.
fn run(backend: &SdkBackend, directory: &Path) -> Result<Vec<McpServerState>, String> {
    let backend = backend.for_start().map_err(|error| error.to_string())?;
    let mut command = match &backend.confinement {
        Some(profile) => {
            let mut command = Command::new("/usr/bin/sandbox-exec");
            command.arg("-f").arg(profile).arg(&backend.node);
            command
        }
        None => Command::new(&backend.node),
    };
    let mut child = command.arg(&backend.host).arg(FLAG).current_dir(directory).env_clear().envs(&backend.host_environment)
        .stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::null()).process_group(0)
        .spawn().map_err(|error| format!("could not start the Agent SDK host: {error}"))?;
    let config = serde_json::json!({"executable": backend.native, "cwd": directory, "env": backend.native_environment});
    let mut stdin = child.stdin.take().expect("piped stdin");
    let written = stdin.write_all(config.to_string().as_bytes());
    drop(stdin);
    let mut stdout = child.stdout.take().expect("piped stdout");
    let (sender, receiver) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut output = String::new();
        let _ = (&mut stdout).take(MAX_OUTPUT).read_to_string(&mut output);
        let _ = sender.send(output);
    });
    let output = receiver.recv_timeout(DEADLINE);
    if output.is_err() {
        // SAFETY: the host leads its own process group, Claude included.
        unsafe { libc::killpg(child.id() as libc::pid_t, libc::SIGKILL) };
    }
    let _ = child.wait();
    written.map_err(|error| format!("could not reach the Agent SDK host: {error}"))?;
    let output = output.map_err(|_| format!("Claude did not answer within {} s", DEADLINE.as_secs()))?;
    parse(&output)
}

/// Checks `directory`, the project `project`, in the background; false when it is already being
/// checked. `done` runs once the result is in `probes`.
pub(crate) fn check(
    probes: std::sync::Arc<McpProbes>,
    backend: SdkBackend,
    project: String,
    directory: std::path::PathBuf,
    done: impl FnOnce() + Send + 'static,
) -> bool {
    if !probes.begin(&project) {
        return false;
    }
    std::thread::spawn(move || {
        let at = crate::log::local_iso(alondra_core::now_millis().div_euclid(1000) as libc::time_t, 0);
        let probe = match run(&backend, &directory) {
            Ok(servers) => LiveMcpProbe::Checked { at, servers },
            Err(error) => {
                log!("could not check the MCP servers of {project}: {error}");
                LiveMcpProbe::Failed { at, error }
            }
        };
        probes.finish(&project, probe);
        done();
    });
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_host_s_answer_is_the_servers_or_why_not() {
        let servers = parse(r#"{"servers":[{"name":"linear","status":"needs-auth","transport":"http","url":"https://mcp.linear.app/mcp"},{"name":"files","status":"a-later-status"},{"status":"connected"}]}"#).unwrap();
        assert_eq!(servers.iter().map(|server| (server.name.as_str(), server.status)).collect::<Vec<_>>(),
            [("linear", McpStatus::NeedsAuth), ("files", McpStatus::Unknown)], "a server without a name is left out");
        assert_eq!(servers[0].url.as_deref(), Some("https://mcp.linear.app/mcp"));
        assert_eq!(parse(r#"{"error":"Claude Code is not installed"}"#), Err("Claude Code is not installed".into()));
        assert_eq!(parse(""), Err("Claude gave no answer".into()));
    }
}
