use super::*;
use alondra_core::domain::{AgentId, McpStatus, TaskId};
use alondra_core::views::Lang;
use std::os::unix::fs::PermissionsExt;

const LINEAR: &str = "https://mcp.linear.app/mcp";

fn server(name: &str, source: &str, url: Option<&str>) -> McpServerState {
    let transport = if url.is_some() { "http" } else { "stdio" };
    McpServerState {
        name: name.into(), status: McpStatus::Connected, error: None, source: Some(source.into()), transport: Some(transport.into()),
        url: url.map(Into::into), traits: alondra_provider_claude::mcp_traits(Some(source), Some(transport), "connected"),
    }
}

fn node(agent: &str, generation: u64, servers: Vec<McpServerState>) -> McpNodeReading {
    McpNodeReading { task: TaskId::from(format!("t-{agent}").as_str()), agent: AgentId::from(agent), generation, servers, signing_in: vec![] }
}

fn target(name: &str, url: Option<&str>) -> String {
    McpServerKey { name: name.into(), url: url.map(Into::into) }.target()
}

/// A home with a fake `claude` that keeps each call's folder and arguments, and answers as Claude
/// Code does: removed, not there (`missing`), or unable to write its settings (`broken`).
struct Fixture {
    home: crate::test_support::Directory,
    backend: SdkBackend,
    projects: Vec<ProjectEntry>,
}

impl Fixture {
    fn new() -> Self {
        let home = crate::test_support::Directory::new();
        let project = home.join("Projects/quill");
        std::fs::create_dir_all(&project).unwrap();
        let native = home.join("claude");
        std::fs::write(&native, r#"#!/bin/sh
printf '%s|%s\n' "$PWD" "$*" >> "$HOME/calls"
if [ -e "$HOME/missing" ]; then echo "No MCP server named \"$6\" in user scope" >&2; exit 1; fi
if [ -e "$HOME/broken" ]; then echo "Error: could not write the settings" >&2; exit 1; fi
echo "Removed MCP server $6"
"#).unwrap();
        std::fs::set_permissions(&native, std::fs::Permissions::from_mode(0o755)).unwrap();
        let backend = SdkBackend {
            node: PathBuf::new(), node_sha256: String::new(), host: PathBuf::new(), host_sha256: String::new(),
            native, native_sha256: String::new(), host_environment: BTreeMap::new(),
            native_environment: BTreeMap::from([("HOME".into(), home.display().to_string()), ("PATH".into(), "/usr/bin:/bin".into())]),
            confinement: None, state_root: home.join("sdk"), io_timeout_seconds: 1, installed_native: false,
        };
        let projects = vec![ProjectEntry { name: "quill".into(), path: project, last_used: None, nodes: 0 }];
        Self { home, backend, projects }
    }

    fn calls(&self) -> Vec<String> {
        std::fs::read_to_string(self.home.join("calls")).map(|calls| calls.lines().map(str::to_owned).collect()).unwrap_or_default()
    }
}

// The user's own server is deleted from their settings for every project, and a project's own from
// its settings, in its folder. Nodes that had it loaded stop listing it on the screen; a check that
// found it no longer lists it either.
#[test]
fn a_server_in_the_user_s_own_settings_is_deleted_through_claude_code() {
    let fixture = Fixture::new();
    let nodes = [
        node("zeta", 3, vec![server("linear", "user", Some(LINEAR)), server("db", "local", None)]),
        node("queued", 1, vec![server("linear", "user", Some(LINEAR))]),
    ];
    let probes = McpProbes::default();
    probes.finish("lumo", LiveMcpProbe::Checked { at: "2026-10-24T09:00:00".into(), servers: vec![server("linear", "user", Some(LINEAR))] });

    assert_eq!(remove(&nodes, &probes, &fixture.projects, &fixture.backend, &target("linear", Some(LINEAR))), Ok("linear".into()));
    let quill = fixture.projects[0].path.display().to_string();
    let subject = format!("{}.{}", target("db", None), project_target("quill"));
    assert_eq!(remove(&nodes, &probes, &fixture.projects, &fixture.backend, &subject), Ok("db".into()));
    assert_eq!(fixture.calls(), [
        format!("{}|mcp remove --scope user -- linear", fixture.home.display()),
        format!("{quill}|mcp remove --scope local -- db"),
    ]);

    let removals = probes.removals();
    assert_eq!(removals[0].loaded, BTreeMap::from([("queued".to_owned(), 1), ("zeta".to_owned(), 3)]), "each node that has it loaded, by its execution");
    assert_eq!(removals[1].loaded, BTreeMap::from([("zeta".to_owned(), 3)]));
    assert!(matches!(&probes.snapshot()["lumo"], LiveMcpProbe::Checked { servers, .. } if servers.is_empty()), "a check that found it no longer lists it");
}

#[test]
fn a_server_no_longer_in_the_settings_counts_as_deleted_and_a_failure_says_why() {
    let fixture = Fixture::new();
    let nodes = [node("zeta", 1, vec![server("linear", "user", Some(LINEAR))])];
    let probes = McpProbes::default();
    std::fs::write(fixture.home.join("missing"), "").unwrap();
    assert_eq!(remove(&nodes, &probes, &fixture.projects, &fixture.backend, &target("linear", Some(LINEAR))), Ok("linear".into()),
        "deleted elsewhere since: it is gone, which is what the user asked");
    std::fs::remove_file(fixture.home.join("missing")).unwrap();
    std::fs::write(fixture.home.join("broken"), "").unwrap();
    let probes = McpProbes::default();
    let refused = remove(&nodes, &probes, &fixture.projects, &fixture.backend, &target("linear", Some(LINEAR))).unwrap_err();
    assert_eq!(refused, (Some("linear".into()), Refused::Failed("Error: could not write the settings".into())));
    assert_eq!(refused.1.message("linear", Lang::Es), "Claude Code no pudo borrar linear: Error: could not write the settings");
    assert!(probes.removals().is_empty(), "a server Claude could not delete stays on the screen");
}

// A repository's shared settings, a plugin, the account's connector and the organization's server
// are deleted where they are configured: Alondra refuses them and says where, without running
// Claude Code.
#[test]
fn a_server_configured_elsewhere_is_refused_with_where_to_delete_it() {
    let fixture = Fixture::new();
    let nodes = [node("zeta", 1, vec![
        server("files", "project", None),
        server("telegram", "plugin", None),
        server("claude.ai Gmail", "claudeai", None),
        server("vault", "managed", Some("https://vault.example.com/mcp")),
    ])];
    let probes = McpProbes::default();
    let refused = |name: &str, url: Option<&str>| remove(&nodes, &probes, &fixture.projects, &fixture.backend, &target(name, url)).unwrap_err();
    assert_eq!(refused("files", None), (Some("files".into()), Refused::Kept(McpOrigin::ProjectSettings)));
    assert_eq!(refused("telegram", None), (Some("telegram".into()), Refused::Kept(McpOrigin::Plugin)));
    assert_eq!(refused("claude.ai Gmail", None).1, Refused::Kept(McpOrigin::Connector));
    assert_eq!(refused("vault", Some("https://vault.example.com/mcp")).1, Refused::Kept(McpOrigin::Managed));
    assert_eq!(Refused::Kept(McpOrigin::Plugin).message("telegram", Lang::Es), "telegram lo aporta un plugin: desinstala el plugin para quitarlo");
    assert_eq!(Refused::Kept(McpOrigin::ProjectSettings).message("files", Lang::En),
        "files is in the project's shared settings, in its repository: delete it there");
    assert!(fixture.calls().is_empty(), "Claude Code never ran");
    assert!(probes.removals().is_empty());
}

#[test]
fn a_server_or_project_no_longer_listed_is_gone() {
    let fixture = Fixture::new();
    let nodes = [node("zeta", 1, vec![server("db", "local", None)])];
    let probes = McpProbes::default();
    let gone = |subject: &str| remove(&nodes, &probes, &fixture.projects, &fixture.backend, subject).unwrap_err().1;
    assert_eq!(gone(&target("linear", Some(LINEAR))), Refused::Gone);
    assert_eq!(gone(&format!("{}.{}", target("db", None), project_target("blurcamera"))), Refused::Gone, "a project no longer listed");
    assert_eq!(gone(&target("db", None)), Refused::Gone, "a project's own server without its project has no folder to run in");
    assert!(fixture.calls().is_empty());
}
