//! How Claude Code's MCP servers read in Alondra's neutral terms: whose each one is, where Claude
//! found it and how the user connects it.
use alondra_foundation::{McpConnect, McpOrigin, McpScope, McpTraits};
use std::path::{Path, PathBuf};

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

/// The file Claude Code keeps the user's own MCP servers in, and each project's private ones:
/// `.claude.json` in its configuration folder (`CLAUDE_CONFIG_DIR`), or else in HOME.
pub fn settings_file(profile: Option<&Path>, home: &Path) -> PathBuf {
    profile.unwrap_or(home).join(".claude.json")
}

/// Whether `settings`, that file read as JSON, still has the server `name` where `traits` say it
/// was: the top-level `mcpServers` for the user's settings, those of `project`'s entry for its
/// private ones. `None` for a server Alondra does not delete.
pub fn still_configured(settings: &serde_json::Value, name: &str, traits: McpTraits, project: &Path) -> Option<bool> {
    let holds = |entry: &serde_json::Value| entry.get("mcpServers").and_then(|servers| servers.get(name)).is_some();
    match traits.origin {
        McpOrigin::UserSettings if traits.remove => Some(holds(settings)),
        McpOrigin::LocalSettings if traits.remove => {
            let project = project.canonicalize().unwrap_or_else(|_| project.to_path_buf());
            let projects = settings.get("projects").and_then(serde_json::Value::as_object);
            Some(projects.into_iter().flatten()
                .filter(|(key, _)| Path::new(key).canonicalize().unwrap_or_else(|_| PathBuf::from(key)) == project)
                .any(|(_, entry)| holds(entry)))
        }
        _ => None,
    }
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
    }

    #[test]
    fn a_server_is_still_configured_only_while_its_scope_in_claude_json_names_it() {
        let user = mcp_traits(Some("user"), Some("http"), "connected");
        let local = mcp_traits(Some("local"), None, "connected");
        let settings = serde_json::json!({
            "mcpServers": {"linear": {"type": "http"}},
            "projects": {"/work/quill": {"mcpServers": {"db": {}}}, "/work/lumo": {"mcpServers": {"linear": {}}}},
        });
        let quill = Path::new("/work/quill");
        assert_eq!(still_configured(&settings, "linear", user, quill), Some(true));
        assert_eq!(still_configured(&settings, "db", user, quill), Some(false), "a project's server is not the user's");
        assert_eq!(still_configured(&settings, "db", local, quill), Some(true));
        assert_eq!(still_configured(&settings, "linear", local, quill), Some(false), "another project's server is not this one's");
        assert_eq!(still_configured(&serde_json::json!({}), "linear", user, quill), Some(false));
        assert_eq!(still_configured(&settings, "linear", mcp_traits(Some("project"), None, "connected"), quill), None);
        assert_eq!(settings_file(None, Path::new("/Users/me")), Path::new("/Users/me/.claude.json"));
        assert_eq!(settings_file(Some(Path::new("/accounts/work")), Path::new("/Users/me")), Path::new("/accounts/work/.claude.json"));
    }
}
