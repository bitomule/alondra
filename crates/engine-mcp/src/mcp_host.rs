//! What a check or a deletion of MCP servers starts: the Agent SDK host and the native agent the
//! nodes run, resolved for one start by whoever keeps the daemon's SDK backend.
use std::collections::BTreeMap;
use std::path::PathBuf;

/// The programs and environments of one start.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct McpHost {
    /// A Seatbelt profile the host runs under, the native agent included.
    pub confinement: Option<PathBuf>,
    pub node: PathBuf,
    pub host: PathBuf,
    pub native: PathBuf,
    pub host_environment: BTreeMap<String, String>,
    pub native_environment: BTreeMap<String, String>,
}

/// The daemon's SDK backend, as the MCP checks and deletions see it.
pub trait HostBackend: Send + 'static {
    /// The host one start runs, or why it cannot start. It may run the native agent to check it.
    fn start(&self) -> Result<McpHost, String>;
}

#[cfg(test)]
impl HostBackend for McpHost {
    fn start(&self) -> Result<McpHost, String> {
        Ok(self.clone())
    }
}
