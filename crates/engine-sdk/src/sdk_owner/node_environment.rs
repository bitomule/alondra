//! What a node's agent gets of the daemon's environment beyond the built-in allowlist: the
//! variables its MCP servers name (`${VAR}` or `${VAR:-default}` in Claude's configuration, or
//! `env_vars`, `bearer_token_env_var` and `env_http_headers` in Codex's) and the ones the user
//! added with `node_environment`. The configuration is read as JSON or TOML, never as free text,
//! and only for the node that will load it.
use alondra_core::accounts::Provider;
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

/// Claude and Codex sign in as the user: a key in the environment would replace that sign-in.
const WITHHELD: [&str; 3] = ["ANTHROPIC_API_KEY", "GH_TOKEN", "GITHUB_TOKEN"];
const WITHHELD_PREFIX: &str = "CLAUDE_CODE_";

fn withheld(name: &str) -> bool {
    WITHHELD.contains(&name) || name.starts_with(WITHHELD_PREFIX)
}

pub(super) fn granted(alondra_home: &Path, user_home: &Path, directory: &Path, provider: Provider, daemon: &dyn Fn(&str) -> Option<String>) -> BTreeMap<String, String> {
    let mut names = added_by_the_user(alondra_home);
    names.extend(match provider {
        Provider::Claude => claude_references(user_home, directory),
        Provider::Codex => codex_references(user_home, directory),
    });
    names.into_iter()
        .filter(|name| alondra_core::config::is_variable_name(name) && !withheld(name))
        .filter_map(|name| daemon(&name).map(|value| (name, value)))
        .collect()
}

fn added_by_the_user(alondra_home: &Path) -> BTreeSet<String> {
    std::fs::read_to_string(alondra_core::config::path_in(alondra_home)).ok()
        .and_then(|text| alondra_core::config::FileConfig::parse(&text).ok())
        .and_then(|file| file.node_environment)
        .unwrap_or_default().into_iter().collect()
}

fn claude_references(user_home: &Path, directory: &Path) -> BTreeSet<String> {
    let mut names = BTreeSet::new();
    let mut servers = Vec::new();
    for folder in directory.ancestors() {
        servers.extend(mcp_servers(&json(&folder.join(".mcp.json"))));
    }
    let user = json(&user_home.join(".claude.json"));
    servers.extend(mcp_servers(&user));
    for folder in directory.ancestors() {
        servers.extend(mcp_servers(&user["projects"][folder.to_string_lossy().as_ref()]));
    }
    let settings = json(&user_home.join(".claude/settings.json"));
    servers.extend(mcp_servers(&settings));
    for install in enabled_plugins(user_home, &settings) {
        servers.extend(mcp_servers(&json(&install.join(".mcp.json"))));
    }
    for server in servers {
        collect_references(&server, &mut names);
    }
    names
}

fn json(path: &Path) -> serde_json::Value {
    std::fs::read_to_string(path).ok().and_then(|text| serde_json::from_str(&text).ok()).unwrap_or(serde_json::Value::Null)
}

/// The servers of a configuration: under `mcpServers`, or, in a plugin's `.mcp.json`, the object itself.
fn mcp_servers(configuration: &serde_json::Value) -> Vec<serde_json::Value> {
    let servers = configuration.get("mcpServers").unwrap_or(configuration);
    servers.as_object().map(|servers| servers.values().filter(|server| server.is_object()).cloned().collect()).unwrap_or_default()
}

fn enabled_plugins(user_home: &Path, settings: &serde_json::Value) -> Vec<PathBuf> {
    let installed = json(&user_home.join(".claude/plugins/installed_plugins.json"));
    let Some(plugins) = installed["plugins"].as_object() else { return Vec::new() };
    plugins.iter()
        .filter(|(name, _)| settings["enabledPlugins"][name.as_str()].as_bool() == Some(true))
        .flat_map(|(_, installs)| installs.as_array().into_iter().flatten())
        .filter_map(|install| install["installPath"].as_str().map(PathBuf::from))
        .collect()
}

fn collect_references(value: &serde_json::Value, names: &mut BTreeSet<String>) {
    match value {
        serde_json::Value::String(text) => names.extend(referenced_in(text)),
        serde_json::Value::Array(items) => items.iter().for_each(|item| collect_references(item, names)),
        serde_json::Value::Object(fields) => fields.values().for_each(|item| collect_references(item, names)),
        _ => {}
    }
}

fn referenced_in(text: &str) -> Vec<String> {
    text.split("${").skip(1)
        .filter_map(|rest| rest.split_once('}').map(|(inside, _)| inside))
        .map(|inside| inside.split_once(":-").map_or(inside, |(name, _)| name).to_owned())
        .collect()
}

fn codex_references(user_home: &Path, directory: &Path) -> BTreeSet<String> {
    let mut names = BTreeSet::new();
    let mut files = vec![user_home.join(".codex/config.toml")];
    files.extend(directory.ancestors().map(|folder| folder.join(".codex/config.toml")));
    for file in files {
        let Some(configuration) = std::fs::read_to_string(&file).ok().and_then(|text| text.parse::<toml::Table>().ok()) else { continue };
        let Some(servers) = configuration.get("mcp_servers").and_then(toml::Value::as_table) else { continue };
        for server in servers.values().filter_map(toml::Value::as_table) {
            names.extend(server.get("env_vars").and_then(toml::Value::as_array).into_iter().flatten().filter_map(toml::Value::as_str).map(str::to_owned));
            names.extend(server.get("bearer_token_env_var").and_then(toml::Value::as_str).map(str::to_owned));
            names.extend(server.get("env_http_headers").and_then(toml::Value::as_table).into_iter().flat_map(toml::Table::values).filter_map(toml::Value::as_str).map(str::to_owned));
            collect_toml_references(&toml::Value::Table(server.clone()), &mut names);
        }
    }
    names
}

fn collect_toml_references(value: &toml::Value, names: &mut BTreeSet<String>) {
    match value {
        toml::Value::String(text) => names.extend(referenced_in(text)),
        toml::Value::Array(items) => items.iter().for_each(|item| collect_toml_references(item, names)),
        toml::Value::Table(fields) => fields.values().for_each(|item| collect_toml_references(item, names)),
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Fixture { root: tempfile::TempDir }

    impl Fixture {
        fn new() -> Self {
            let fixture = Self { root: tempfile::tempdir().unwrap() };
            std::fs::create_dir_all(fixture.alondra()).unwrap();
            std::fs::create_dir_all(fixture.repo()).unwrap();
            fixture
        }
        fn alondra(&self) -> PathBuf { self.root.path().join("alondra") }
        fn user(&self) -> PathBuf { self.root.path().join("user") }
        fn repo(&self) -> PathBuf { self.root.path().join("repo") }
        fn write(&self, path: PathBuf, text: &str) {
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, text).unwrap();
        }
        fn granted(&self, provider: Provider) -> BTreeMap<String, String> {
            let daemon = |name: &str| Some(format!("value-of-{name}"));
            granted(&self.alondra(), &self.user(), &self.repo(), provider, &daemon)
        }
    }

    #[test]
    fn a_variable_the_repos_mcp_configuration_references_reaches_the_node_and_an_unreferenced_one_does_not() {
        let fixture = Fixture::new();
        fixture.write(fixture.repo().join(".mcp.json"), r#"{"mcpServers": {"firecrawl": {"command": "npx", "env": {"KEY": "${FIRECRAWL_API_KEY}"}}, "revenuecat": {"url": "https://x", "headers": {"Authorization": "Bearer ${REVENUECAT_MCP_BEARER_TOKEN:-none}"}}}}"#);
        let granted = fixture.granted(Provider::Claude);
        assert_eq!(granted.keys().map(String::as_str).collect::<Vec<_>>(), ["FIRECRAWL_API_KEY", "REVENUECAT_MCP_BEARER_TOKEN"]);
        assert!(!granted.contains_key("UNRELATED_SECRET"));
    }

    #[test]
    fn the_users_claude_configuration_and_enabled_plugins_are_read_too_and_disabled_plugins_are_not() {
        let fixture = Fixture::new();
        let user = fixture.user();
        fixture.write(user.join(".claude.json"), &format!(r#"{{"mcpServers": {{"a": {{"args": ["${{FROM_USER}}"]}}}}, "projects": {{"{}": {{"mcpServers": {{"b": {{"env": {{"K": "${{FROM_PROJECT}}"}}}}}}}}}}}}"#, fixture.repo().display()));
        fixture.write(user.join(".claude/settings.json"), r#"{"mcpServers": {"c": {"env": {"K": "${FROM_SETTINGS}"}}}, "enabledPlugins": {"on@market": true, "off@market": false}}"#);
        let plugin = |name: &str| user.join("plugins-data").join(name);
        fixture.write(user.join(".claude/plugins/installed_plugins.json"), &format!(r#"{{"version": 2, "plugins": {{"on@market": [{{"installPath": "{}"}}], "off@market": [{{"installPath": "{}"}}]}}}}"#, plugin("on").display(), plugin("off").display()));
        fixture.write(plugin("on").join(".mcp.json"), r#"{"server": {"env": {"K": "${FROM_ENABLED_PLUGIN}"}}}"#);
        fixture.write(plugin("off").join(".mcp.json"), r#"{"mcpServers": {"server": {"env": {"K": "${FROM_DISABLED_PLUGIN}"}}}}"#);
        let granted = fixture.granted(Provider::Claude);
        assert_eq!(granted.keys().map(String::as_str).collect::<Vec<_>>(), ["FROM_ENABLED_PLUGIN", "FROM_PROJECT", "FROM_SETTINGS", "FROM_USER"]);
    }

    #[test]
    fn codex_servers_name_their_variables_in_the_users_and_the_repos_toml() {
        let fixture = Fixture::new();
        fixture.write(fixture.user().join(".codex/config.toml"), "[mcp_servers.a]\nenv_vars = [\"CODEX_LISTED\"]\nbearer_token_env_var = \"CODEX_BEARER\"\n[mcp_servers.a.env_http_headers]\nX-Key = \"CODEX_HEADER\"\n[mcp_servers.b]\nargs = [\"${CODEX_ARGUMENT}\"]\n");
        fixture.write(fixture.repo().join(".mcp.json"), r#"{"mcpServers": {"c": {"env": {"K": "${ONLY_CLAUDE_READS_THIS}"}}}}"#);
        let granted = fixture.granted(Provider::Codex);
        assert_eq!(granted.keys().map(String::as_str).collect::<Vec<_>>(), ["CODEX_ARGUMENT", "CODEX_BEARER", "CODEX_HEADER", "CODEX_LISTED"]);
    }

    #[test]
    fn the_users_setting_adds_names_and_the_sign_in_variables_stay_out_even_when_referenced_or_added() {
        let fixture = Fixture::new();
        fixture.write(fixture.alondra().join("config.toml"), "node_environment = [\"GOPATH\", \"GH_TOKEN\"]\n");
        fixture.write(fixture.repo().join(".mcp.json"), r#"{"mcpServers": {"a": {"env": {"A": "${ANTHROPIC_API_KEY}", "B": "${GITHUB_TOKEN}", "C": "${CLAUDE_CODE_MESSAGING_TOKEN}", "D": "${GH_TOKEN}"}}}}"#);
        assert_eq!(fixture.granted(Provider::Claude).keys().map(String::as_str).collect::<Vec<_>>(), ["GOPATH"]);
    }

    #[test]
    fn a_variable_the_daemon_does_not_have_is_not_invented() {
        let fixture = Fixture::new();
        fixture.write(fixture.repo().join(".mcp.json"), r#"{"mcpServers": {"a": {"env": {"A": "${MISSING}"}}}}"#);
        assert!(granted(&fixture.alondra(), &fixture.user(), &fixture.repo(), Provider::Claude, &|_| None).is_empty());
    }
}
