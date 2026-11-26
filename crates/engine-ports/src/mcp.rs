//! What the MCP servers screen and the cards that wait for a server speak in: the checks and
//! deletions this daemon ran, what Connect, Reconnect and Sign out act on, and why a server is not
//! deleted. The engine runs them; the server only reads and words them.
use alondra_domain_model::{InteractionId, MCP_SERVER_PREFIX};
use alondra_foundation::McpOrigin;
use alondra_foundation::locks::Poisonable as _;
use alondra_view_model::{LiveMcpProbe, LiveMcpRemoval};
use std::collections::BTreeMap;
use std::sync::Mutex;

/// What the MCP servers screen knows beyond the domain while this daemon runs: each check, and the
/// servers the user deleted (`mcp_remove`).
#[derive(Default)]
pub struct McpProbes {
    /// A holder that panics leaves it whole, so it is recovered: a live view of each project's
    /// check that the next probe writes again.
    probes: Mutex<BTreeMap<String, LiveMcpProbe>>,
    /// A holder that panics leaves it whole, so it is recovered: a retain and a push of removal
    /// notes for the live screen.
    removed: Mutex<Vec<LiveMcpRemoval>>,
}

impl McpProbes {
    pub fn snapshot(&self) -> BTreeMap<String, LiveMcpProbe> {
        self.probes.lock_or_recover("the MCP probes").clone()
    }

    pub fn removals(&self) -> Vec<LiveMcpRemoval> {
        self.removed.lock_or_recover("the MCP removals").clone()
    }

    /// Keeps that the user deleted a server: of `project`'s own settings, or of their own for every
    /// project. The checks that found it there no longer list it.
    pub fn removed(&self, removal: LiveMcpRemoval, project: Option<&str>) {
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
    pub fn begin(&self, project: &str) -> bool {
        let mut probes = self.probes.lock_or_recover("the MCP probes");
        if matches!(probes.get(project), Some(LiveMcpProbe::Checking)) {
            return false;
        }
        probes.insert(project.to_owned(), LiveMcpProbe::Checking);
        true
    }

    pub fn finish(&self, project: &str, probe: LiveMcpProbe) {
        self.probes.lock_or_recover("the MCP probes").insert(project.to_owned(), probe);
    }
}

/// What Connect hands the user.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Opened {
    /// The user signs in at `url`; with a `port` the browser then comes back to
    /// `http://localhost:<port>/callback`, Claude's listener on the Mac, and on another device the
    /// client listens on that port of its own or the user pastes where it came back to.
    Browser { server: String, url: String, port: Option<u16> },
    /// Claude's stored sign-in works: nothing to do in a browser.
    Connected { server: String },
}

/// What Connect, Reconnect and Sign out act on: the card the tasks that wait for a server wait
/// on, or a server the MCP servers screen names by its target.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Target {
    Card(InteractionId),
    Server(String),
}

impl Target {
    /// A server key names a server; anything else names a card, which must be one of Alondra's
    /// MCP waits for anything to act on it.
    pub fn of(id: &str) -> Option<Self> {
        Some(if id.starts_with(MCP_SERVER_PREFIX) { Self::Server(id.to_owned()) } else { Self::Card(InteractionId::from(id)) })
    }
}

/// Why a server is not deleted.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Refused {
    /// Nothing reports it any more, or its project is no longer listed.
    Gone,
    /// It is configured where the user deletes it, not in their own settings.
    Kept(McpOrigin),
    /// Claude could not delete it.
    Failed(String),
}

impl Refused {
    pub fn message(&self, name: &str, lang: alondra_view_model::Lang) -> String {
        let es = lang == alondra_view_model::Lang::Es;
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

