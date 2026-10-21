//! How Claude Code's MCP servers read in Alondra's neutral terms: whose each one is, where Claude
//! found it and how the user connects it.
use alondra_foundation::{McpConnect, McpOrigin, McpScope, McpTraits};

/// The traits of a server Claude lists with `source` (its `scope` in `mcpServerStatus`), its
/// transport (`config.type`) and `status`. Init lists no transport: a server that needs a sign-in
/// there is one Claude signs in to.
pub fn mcp_traits(source: Option<&str>, transport: Option<&str>, status: &str) -> McpTraits {
    let connector = source == Some("claudeai") || transport == Some("claudeai-proxy");
    let (scope, origin) = match source {
        _ if connector => (McpScope::User, McpOrigin::Connector),
        Some("user") => (McpScope::User, McpOrigin::UserSettings),
        Some("project") => (McpScope::Project, McpOrigin::ProjectSettings),
        Some("local") => (McpScope::Project, McpOrigin::LocalSettings),
        Some("plugin") => (McpScope::User, McpOrigin::Plugin),
        Some("managed" | "enterprise") => (McpScope::User, McpOrigin::Managed),
        _ => (McpScope::Project, McpOrigin::Unknown),
    };
    let remote = matches!(transport, Some("http" | "sse"));
    let connect = if connector {
        McpConnect::Connector
    } else if remote || (transport.is_none() && status == "needs-auth") {
        McpConnect::SignIn
    } else {
        McpConnect::None
    };
    McpTraits { scope, origin, connect, reconnect: true, sign_out: remote && !connector }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn claude_s_sources_and_transports_read_as_whose_a_server_is_and_how_it_connects() {
        let traits = |source, transport, status| mcp_traits(source, transport, status);
        assert_eq!(traits(Some("user"), Some("http"), "needs-auth"),
            McpTraits { scope: McpScope::User, origin: McpOrigin::UserSettings, connect: McpConnect::SignIn, reconnect: true, sign_out: true });
        assert_eq!(traits(Some("project"), Some("stdio"), "connected"),
            McpTraits { scope: McpScope::Project, origin: McpOrigin::ProjectSettings, connect: McpConnect::None, reconnect: true, sign_out: false });
        assert_eq!(traits(Some("claudeai"), Some("claudeai-proxy"), "needs-auth"),
            McpTraits { scope: McpScope::User, origin: McpOrigin::Connector, connect: McpConnect::Connector, reconnect: true, sign_out: false },
            "a claude.ai connector is the user's and is connected on claude.ai");
        assert_eq!(traits(Some("local"), Some("sse"), "connected").scope, McpScope::Project);
        assert_eq!(traits(Some("plugin"), None, "connected").scope, McpScope::User);
        assert_eq!(traits(Some("user"), None, "needs-auth").connect, McpConnect::SignIn, "init names no transport");
        assert_eq!(traits(None, None, "connected"), McpTraits { reconnect: true, ..McpTraits::default() });
    }
}
