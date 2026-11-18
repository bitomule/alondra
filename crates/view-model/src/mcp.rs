/// The key of the MCP servers screen.
pub const SERVERS_KEY: &str = "mcp_servers";
pub const PROJECT_PREFIX: &str = "mcp-project-";

/// The opaque id the screen names a project by in its actions.
pub fn project_target(project: &str) -> String {
    use sha2::{Digest, Sha256};
    let digest = Sha256::digest(project.as_bytes());
    let hex: String = digest[..8].iter().map(|byte| format!("{byte:02x}")).collect();
    format!("{PROJECT_PREFIX}{hex}")
}
