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
    let remove = matches!(origin, McpOrigin::UserSettings | McpOrigin::LocalSettings);
    McpTraits { scope, origin, connect, reconnect: true, sign_out: remote && !connector, remove }
}

/// What deletes the server `name` Claude lists with `traits` from the settings it is in: the
/// arguments of `claude`, run in the project's folder. `None` for one the user deletes elsewhere.
pub fn remove_arguments(name: &str, traits: McpTraits) -> Option<Vec<String>> {
    let scope = match traits.origin {
        McpOrigin::UserSettings if traits.remove => "user",
        McpOrigin::LocalSettings if traits.remove => "local",
        _ => return None,
    };
    Some(["mcp", "remove", "--scope", scope, "--", name].map(str::to_owned).to_vec())
}

/// Whether what `claude mcp remove` wrote says the server was not in those settings any more.
pub fn removal_found_nothing(output: &str) -> bool {
    output.contains("No MCP server named")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn claude_s_sources_and_transports_read_as_whose_a_server_is_and_how_it_connects() {
        let traits = |source, transport, status| mcp_traits(source, transport, status);
        assert_eq!(traits(Some("user"), Some("http"), "needs-auth"),
            McpTraits { scope: McpScope::User, origin: McpOrigin::UserSettings, connect: McpConnect::SignIn, reconnect: true, sign_out: true, remove: true });
        assert_eq!(traits(Some("project"), Some("stdio"), "connected"),
            McpTraits { scope: McpScope::Project, origin: McpOrigin::ProjectSettings, connect: McpConnect::None, reconnect: true, sign_out: false, remove: false });
        assert_eq!(traits(Some("claudeai"), Some("claudeai-proxy"), "needs-auth"),
            McpTraits { scope: McpScope::User, origin: McpOrigin::Connector, connect: McpConnect::Connector, reconnect: true, sign_out: false, remove: false },
            "a claude.ai connector is the user's and is connected on claude.ai");
        assert_eq!(traits(Some("local"), Some("sse"), "connected").scope, McpScope::Project);
        assert_eq!(traits(Some("plugin"), None, "connected").scope, McpScope::User);
        assert_eq!(traits(Some("user"), None, "needs-auth").connect, McpConnect::SignIn, "init names no transport");
        assert_eq!(traits(None, None, "connected"), McpTraits { reconnect: true, ..McpTraits::default() });
    }

    #[test]
    fn only_a_server_in_the_user_s_own_settings_is_deleted_from_alondra() {
        let removal = |source| remove_arguments("linear", mcp_traits(Some(source), Some("http"), "connected"));
        assert_eq!(removal("user"), Some(["mcp", "remove", "--scope", "user", "--", "linear"].map(str::to_owned).to_vec()));
        assert_eq!(removal("local"), Some(["mcp", "remove", "--scope", "local", "--", "linear"].map(str::to_owned).to_vec()));
        for kept in ["project", "plugin", "claudeai", "managed", "a-later-source"] {
            assert_eq!(removal(kept), None, "{kept} is deleted where it is configured, not from Alondra");
            assert!(!mcp_traits(Some(kept), Some("http"), "connected").remove, "{kept}");
        }
        assert_eq!(remove_arguments("-x", mcp_traits(Some("user"), None, "connected")).unwrap().last().map(String::as_str), Some("-x"),
            "a name that looks like an option is still the name");
        assert!(removal_found_nothing("No MCP server named \"linear\" in user scope"));
        assert!(!removal_found_nothing("Removed MCP server linear from user config"));
    }
}
