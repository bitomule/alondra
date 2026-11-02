//! Deletes an MCP server from the agent's settings, as the user asked on the MCP servers screen.
//! The provider says which servers are in the user's own settings and how they are deleted;
//! Alondra runs that in the project's folder and touches no configuration file itself. A server a
//! repository, a plugin, the user's account or their organization configures is refused: it is
//! deleted there. Nodes that had it loaded keep it until their agent starts again, and the screen
//! stops listing it for them meanwhile.
use crate::mcp_probe::McpProbes;
use crate::sdk_owner::SdkBackend;
use alondra_core::domain::{McpNodeReading, McpServerKey, McpServerState};
use alondra_core::options::ProjectEntry;
use alondra_core::views::mcp_servers::project_target;
use alondra_core::views::{LiveMcpProbe, LiveMcpRemoval};
use alondra_foundation::{McpOrigin, McpTraits};
use std::collections::BTreeMap;
use std::io::Read;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

/// Claude reads and writes its settings well within this.
const DEADLINE: Duration = Duration::from_secs(30);
const MAX_OUTPUT: u64 = 64 * 1024;

/// What deleting a server the screen names takes.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Removal {
    pub(crate) key: McpServerKey,
    pub(crate) traits: McpTraits,
    pub(crate) arguments: Vec<String>,
    /// The project whose own settings hold it; none for the user's settings of every project.
    pub(crate) project: Option<ProjectEntry>,
    /// Each node that has it loaded now, by agent, with its execution.
    pub(crate) loaded: BTreeMap<String, u64>,
}

/// Why a server is not deleted.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Refused {
    /// Nothing reports it any more, or its project is no longer listed.
    Gone,
    /// It is configured where the user deletes it, not in their own settings.
    Kept(McpOrigin),
    /// Claude could not delete it.
    Failed(String),
}

impl Refused {
    pub(crate) fn message(&self, name: &str, lang: alondra_core::views::Lang) -> String {
        let es = lang == alondra_core::views::Lang::Es;
        match self {
            Self::Gone if es => "Este servidor MCP ya no aparece".into(),
            Self::Gone => "This MCP server is no longer listed".into(),
            Self::Kept(McpOrigin::ProjectSettings) if es => format!("{name} está en la configuración compartida del proyecto, en su repositorio: bórralo allí"),
            Self::Kept(McpOrigin::ProjectSettings) => format!("{name} is in the project's shared settings, in its repository: delete it there"),
            Self::Kept(McpOrigin::Plugin) if es => format!("{name} lo aporta un plugin: desinstala el plugin para quitarlo"),
            Self::Kept(McpOrigin::Plugin) => format!("{name} comes from a plugin: uninstall the plugin to remove it"),
            Self::Kept(McpOrigin::Connector) if es => format!("{name} es un conector de tu cuenta: quítalo en la web del proveedor"),
            Self::Kept(McpOrigin::Connector) => format!("{name} is a connector of your account: remove it on the provider's website"),
            Self::Kept(McpOrigin::Managed) if es => format!("{name} lo configura tu organización"),
            Self::Kept(McpOrigin::Managed) => format!("{name} is configured by your organization"),
            Self::Kept(_) if es => format!("Alondra no sabe dónde está configurado {name}"),
            Self::Kept(_) => format!("Alondra does not know where {name} is configured"),
            Self::Failed(why) if es => format!("Claude Code no pudo borrar {name}: {why}"),
            Self::Failed(why) => format!("Claude Code could not delete {name}: {why}"),
        }
    }
}

/// The server `subject` names (its target, and its project's after a dot) as the open tasks'
/// `nodes` and the checks report it, and what deleting it takes. `Err` carries the server's name
/// when it is known.
pub(crate) fn resolve(
    nodes: &[McpNodeReading],
    probes: &BTreeMap<String, LiveMcpProbe>,
    projects: &[ProjectEntry],
    subject: &str,
) -> Result<(String, Removal), (Option<String>, Refused)> {
    let (target, project) = match subject.split_once('.') {
        Some((target, project)) => (target, Some(project)),
        None => (subject, None),
    };
    let probed = probes.values().flat_map(|probe| match probe {
        LiveMcpProbe::Checked { servers, .. } => servers.as_slice(),
        _ => &[],
    });
    let named: Vec<&McpServerState> = nodes.iter().flat_map(|node| &node.servers).chain(probed)
        .filter(|server| McpServerKey::of(server).target() == target).collect();
    let Some(first) = named.first() else { return Err((None, Refused::Gone)) };
    let name = first.name.clone();
    let Some(server) = named.iter().find(|server| server.traits.remove) else {
        return Err((Some(name), Refused::Kept(first.traits.origin)));
    };
    let project = match (server.traits.origin, project) {
        (McpOrigin::UserSettings, _) => None,
        (McpOrigin::LocalSettings, Some(project)) => match projects.iter().find(|entry| project_target(&entry.name) == project) {
            Some(entry) => Some(entry.clone()),
            None => return Err((Some(name), Refused::Gone)),
        },
        _ => return Err((Some(name), Refused::Gone)),
    };
    let Some(arguments) = alondra_provider_claude::remove_arguments(&server.name, server.traits) else {
        return Err((Some(name), Refused::Kept(server.traits.origin)));
    };
    let key = McpServerKey::of(server);
    let loaded = nodes.iter()
        .filter(|node| node.servers.iter().any(|reported| key.names(reported) && reported.traits.origin == server.traits.origin))
        .map(|node| (node.agent.0.clone(), node.generation))
        .collect();
    Ok((name, Removal { key, traits: server.traits, arguments, project, loaded }))
}

/// Runs the provider's deletion in `directory` with the native agent the nodes run, within
/// `DEADLINE`. When Claude fails, the server counts as deleted only if its settings, read again,
/// no longer have it.
pub(crate) fn run(backend: &SdkBackend, removal: &Removal, directory: &Path) -> Result<(), String> {
    let backend = backend.for_start().map_err(|error| error.to_string())?;
    let mut command = match &backend.confinement {
        Some(profile) => {
            let mut command = Command::new("/usr/bin/sandbox-exec");
            command.arg("-f").arg(profile).arg(&backend.native);
            command
        }
        None => Command::new(&backend.native),
    };
    let mut child = command.args(&removal.arguments).current_dir(directory).env_clear().envs(&backend.native_environment)
        .stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped()).process_group(0)
        .spawn().map_err(|error| format!("could not start Claude Code: {error}"))?;
    let read = |mut pipe: Box<dyn Read + Send>| {
        let (sender, receiver) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let mut output = String::new();
            let _ = (&mut pipe).take(MAX_OUTPUT).read_to_string(&mut output);
            let _ = sender.send(output);
        });
        receiver
    };
    let stdout = read(Box::new(child.stdout.take().expect("piped stdout")));
    let stderr = read(Box::new(child.stderr.take().expect("piped stderr")));
    let started = std::time::Instant::now();
    let output = stderr.recv_timeout(DEADLINE).and_then(|errors| {
        let left = DEADLINE.saturating_sub(started.elapsed());
        stdout.recv_timeout(left).map(|output| format!("{output}{errors}"))
    });
    let Ok(output) = output else {
        // SAFETY: Claude leads its own process group.
        unsafe { libc::killpg(child.id() as libc::pid_t, libc::SIGKILL) };
        let _ = child.wait();
        return Err(format!("it did not answer within {} s", DEADLINE.as_secs()));
    };
    let status = child.wait().map_err(|error| error.to_string())?;
    if status.success() || still_configured(&backend.native_environment, &removal.key.name, removal.traits, directory) == Some(false) {
        return Ok(());
    }
    let why = output.lines().map(str::trim).find(|line| !line.is_empty()).unwrap_or("it gave no reason");
    Err(why.chars().take(240).collect())
}

/// Whether Claude's settings, read with the environment it runs in, still have the server. `None`
/// when they cannot be read.
fn still_configured(environment: &BTreeMap<String, String>, name: &str, traits: McpTraits, directory: &Path) -> Option<bool> {
    let profile = alondra_core::claude_account::configured_profile(&|key: &str| environment.get(key).cloned());
    let home = PathBuf::from(environment.get("HOME")?);
    let settings = std::fs::read(alondra_provider_claude::settings_file(profile.as_deref(), &home)).ok()?;
    alondra_provider_claude::still_configured(&serde_json::from_slice(&settings).ok()?, name, traits, directory)
}

/// Deletes the server `subject` names and has the screen stop listing it. `Ok` is its name.
pub(crate) fn remove(
    nodes: &[McpNodeReading],
    probes: &McpProbes,
    projects: &[ProjectEntry],
    backend: &SdkBackend,
    subject: &str,
) -> Result<String, (Option<String>, Refused)> {
    let (name, removal) = resolve(nodes, &probes.snapshot(), projects, subject)?;
    let directory = match &removal.project {
        Some(project) => project.path.clone(),
        None => backend.native_environment.get("HOME").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("/")),
    };
    run(backend, &removal, &directory).map_err(|why| (Some(name.clone()), Refused::Failed(why)))?;
    log!("deleted the MCP server {name} from the agent's settings");
    probes.removed(LiveMcpRemoval { key: removal.key, loaded: removal.loaded }, removal.project.as_ref().map(|project| project.name.as_str()));
    Ok(name)
}

#[cfg(test)]
#[path = "mcp_remove_tests.rs"]
mod tests;
