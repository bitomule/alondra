//! The MCP servers screen: per project, each server its tasks' nodes use, how it is, where it is
//! configured and which tasks use it, with what can be done about it now. A project no node runs
//! in is checked on demand, by starting the agent there without asking it anything. A server in the
//! user's own settings can be deleted from them; the rest are deleted where they are configured.
use super::action_id::Verb;
use super::text::{t, tf};
use super::{common, Action, Icon, Item, Label, Lang, Link, LocalLink, RenderContext, Role, Style, Tone, View};
use crate::domain::{McpServerKey, McpServerState, McpStatus, TaskId};
use alondra_foundation::{McpConnect, McpOrigin, McpScope};
use std::collections::BTreeMap;

pub const KEY: &str = "mcp_servers";
const PROJECT_PREFIX: &str = "mcp-project-";

/// The opaque id the screen names a project by in its actions.
pub fn project_target(project: &str) -> String {
    use sha2::{Digest, Sha256};
    let digest = Sha256::digest(project.as_bytes());
    let hex: String = digest[..8].iter().map(|byte| format!("{byte:02x}")).collect();
    format!("{PROJECT_PREFIX}{hex}")
}

/// One server of a project, as the nodes of its open tasks reported it.
struct Used<'a> {
    key: McpServerKey,
    reported: Vec<(&'a TaskId, &'a McpServerState, bool)>,
}

impl Used<'_> {
    fn first(&self) -> &McpServerState {
        self.reported[0].1
    }
    fn signing_in(&self) -> bool {
        self.reported.iter().any(|(_, _, open)| *open)
    }
    /// What the user can act on first: a node that needs a sign-in, then one that fails, then one
    /// still connecting; connected only when every node is.
    fn status(&self) -> McpStatus {
        [McpStatus::NeedsAuth, McpStatus::Failed, McpStatus::Unknown, McpStatus::Pending, McpStatus::Connected]
            .into_iter()
            .find(|status| self.reported.iter().any(|(_, server, _)| server.status == *status))
            .unwrap_or(McpStatus::Disabled)
    }
    fn error(&self) -> Option<&str> {
        self.reported.iter().find(|(_, server, _)| server.status == McpStatus::Failed).and_then(|(_, server, _)| server.error.as_deref())
    }
}

/// How a server is, in a few words. A sign-in is open only while the server waits for it: one whose
/// last attempt failed reads as failed. `signs_in` marks a server the user connects by signing in.
fn status_label(status: McpStatus, signing_in: bool, signs_in: bool, lang: Lang) -> Label {
    let (text, tone) = match status {
        McpStatus::NeedsAuth | McpStatus::Pending if signing_in => (t!(lang, "Signing in", "Iniciando sesión"), Tone::Info),
        McpStatus::Connected => (t!(lang, "Connected", "Conectado"), Tone::Success),
        McpStatus::NeedsAuth => (t!(lang, "Needs sign-in", "Necesita iniciar sesión"), Tone::YourTurn),
        McpStatus::Failed if signs_in => (t!(lang, "Sign-in failed", "No se pudo iniciar sesión"), Tone::Critical),
        McpStatus::Failed => (t!(lang, "Failed", "Falla"), Tone::Critical),
        McpStatus::Pending => (t!(lang, "Connecting", "Conectando"), Tone::Neutral),
        McpStatus::Disabled => (t!(lang, "Turned off", "Desactivado"), Tone::Neutral),
        McpStatus::Unknown => (t!(lang, "A status Alondra does not know", "Un estado que Alondra no conoce"), Tone::Neutral),
    };
    Label { text: text.into(), tone, elapsed: None }
}

/// Where the provider found a server, in a few words.
fn origin_text(origin: McpOrigin, lang: Lang) -> Option<&'static str> {
    Some(match origin {
        McpOrigin::UserSettings => t!(lang, "Your settings", "Tu configuración"),
        McpOrigin::ProjectSettings => t!(lang, "Project settings", "Configuración del proyecto"),
        McpOrigin::LocalSettings => t!(lang, "Your settings for this project", "Tu configuración de este proyecto"),
        McpOrigin::Plugin => t!(lang, "Plugin", "Plugin"),
        McpOrigin::Connector => t!(lang, "Account connector", "Conector de tu cuenta"),
        McpOrigin::Managed => t!(lang, "Your organization", "Tu organización"),
        McpOrigin::Unknown => return None,
    })
}

fn quoted(titles: &[&str]) -> String {
    titles.iter().map(|title| format!("«{title}»")).collect::<Vec<_>>().join(", ")
}

fn local_button(label: &str, local: LocalLink) -> Action {
    let mut action = common::view_link("", label);
    action.link = Some(Link::Local { local });
    action
}

/// Delete, for a server in the user's own settings: of every project, or of `project`'s.
fn remove_action(server: &McpServerState, project: Option<&str>, context: &RenderContext<'_>) -> Option<Action> {
    let lang = context.lang;
    if !server.traits.remove {
        return None;
    }
    let target = McpServerKey::of(server).target();
    let (subject, message) = match (server.traits.origin, project) {
        (McpOrigin::UserSettings, _) => (target, t!(lang,
            "Alondra deletes it from your Claude Code settings, for every project. Tasks that already use it keep it until they start again.",
            "Alondra lo borra de tu configuración de Claude Code, en todos tus proyectos. Las tareas que ya lo usan lo mantienen hasta que vuelvan a arrancar.").to_owned()),
        (McpOrigin::LocalSettings, Some(project)) => (format!("{target}.{}", project_target(project)), tf!(lang,
            "Alondra deletes it from your Claude Code settings for {project}. Tasks that already use it keep it until they start again.",
            "Alondra lo borra de tu configuración de Claude Code para {project}. Las tareas que ya lo usan lo mantienen hasta que vuelvan a arrancar.")),
        _ => return None,
    };
    let mut remove = common::action(Verb::McpRemove, &subject, context.domain.revision, t!(lang, "Delete", "Borrar"), Style::Destructive)?;
    let name = &server.name;
    remove.confirm = Some(super::Confirm {
        title: tf!(lang, "Delete {name}?", "¿Borrar {name}?"),
        message,
        button: t!(lang, "Delete", "Borrar").into(),
    });
    remove.stamp = Some(t!(lang, "Deleted", "Borrado").into());
    remove.resolves_card = true;
    Some(remove)
}

fn server_row(used: &Used<'_>, waiting: usize, project: Option<&str>, context: &RenderContext<'_>) -> Item {
    let lang = context.lang;
    let server = used.first();
    let status = used.status();
    let target = used.key.target();
    let titles: Vec<&str> = used.reported.iter().filter_map(|(task, _, _)| context.domain.tasks.get(*task)).map(|task| task.title.as_str()).collect();
    let mut about: Vec<String> = origin_text(server.traits.origin, lang).map(str::to_owned).into_iter().collect();
    about.extend(used.error().map(str::to_owned));
    let mut usage = vec![match titles.len() {
        1 => tf!(lang, "Used by {tasks}", "Lo usa {tasks}", tasks = quoted(&titles)),
        2 => tf!(lang, "Used by {tasks}", "Lo usan {tasks}", tasks = quoted(&titles)),
        count => tf!(lang, "Used by {count} tasks", "Lo usan {count} tareas"),
    }];
    if waiting > 0 {
        usage.push(if waiting == 1 { t!(lang, "1 waits for it", "1 lo espera").into() } else {
            tf!(lang, "{waiting} wait for it", "{waiting} lo esperan")
        });
    }
    let subtitle = [about.join(" · "), usage.join(" · ")].into_iter().filter(|line| !line.is_empty()).collect::<Vec<_>>().join("\n");
    let mut row = common::row(&target, KEY, Icon::Machine, server.name.clone(), Some(subtitle));
    row.link = None;
    let connector = used.reported.iter().any(|(_, server, _)| server.traits.connect == McpConnect::Connector);
    let signable = used.reported.iter().any(|(_, server, _)| server.traits.connect != McpConnect::None);
    let reconnects = used.reported.iter().any(|(_, server, _)| server.traits.reconnect);
    row.badges.push(status_label(status, used.signing_in(), signable, lang));
    match status {
        McpStatus::NeedsAuth | McpStatus::Pending if used.signing_in() => {}
        McpStatus::NeedsAuth => connect(&mut row, &target, &server.name, connector, lang),
        McpStatus::Failed if signable => connect(&mut row, &target, &server.name, connector, lang),
        McpStatus::Failed => {
            if reconnects {
                row.actions.push(local_button(t!(lang, "Reconnect", "Reconectar"), LocalLink::McpReconnect { card: target.clone() }));
            }
        }
        McpStatus::Connected => {
            if reconnects {
                row.actions.push(local_button(t!(lang, "Reconnect", "Reconectar"), LocalLink::McpReconnect { card: target.clone() }));
            }
            if used.reported.iter().any(|(_, server, _)| server.signs_in())
                && let Some(mut sign_out) = common::action(Verb::McpSignOut, &target, context.domain.revision, t!(lang, "Sign out", "Cerrar sesión"), Style::Destructive)
            {
                let name = &server.name;
                sign_out.confirm = Some(super::Confirm {
                    title: tf!(lang, "Sign out of {name}?", "¿Cerrar sesión en {name}?"),
                    message: tf!(lang,
                        "The agent forgets and revokes its sign-in to {name}. Every task that uses it loses it until you connect again.",
                        "El agente olvida y revoca su sesión en {name}. Las tareas que lo usan lo pierden hasta que vuelvas a conectar."),
                    button: t!(lang, "Sign out", "Cerrar sesión").into(),
                });
                sign_out.stamp = Some(t!(lang, "Signed out", "Sesión cerrada").into());
                row.actions.push(sign_out);
            }
        }
        McpStatus::Pending | McpStatus::Disabled | McpStatus::Unknown => {}
    }
    row.actions.extend(remove_action(server, project, context));
    Item::Row(row)
}

/// Connect, the one thing to do for a server that needs a sign-in or whose last one failed.
fn connect(row: &mut super::Row, target: &str, server: &str, connector: bool, lang: Lang) {
    let mut connect = local_button(t!(lang, "Connect", "Conectar"), LocalLink::McpSignIn { card: target.to_owned(), server: server.to_owned() });
    connect.style = Style::Primary;
    connect.role = Some(Role::Advance);
    if connector {
        connect.description = Some(t!(lang, "Opens the provider's website to connect it there", "Abre la web del proveedor para conectarlo allí").into());
    }
    row.actions.push(connect);
}

/// A server a check of a project found, with no node to act through.
/// With `idle`, a server of the user's that no task uses: nothing can act on it from here, and the
/// row says so.
fn probed_row(server: &McpServerState, idle: bool, context: &RenderContext<'_>) -> super::Row {
    let lang = context.lang;
    let mut facts = vec![];
    if let Some(origin) = origin_text(server.traits.origin, lang) {
        facts.push(origin.to_owned());
    }
    if let Some(error) = server.error.as_deref().filter(|_| server.status == McpStatus::Failed) {
        facts.push(error.to_owned());
    }
    let mut lines = vec![facts.join(" · ")];
    if idle && server.status != McpStatus::Connected {
        lines.push(t!(lang, "No task uses it now: you can connect it once one does", "Ahora no lo usa ninguna tarea: podrás conectarlo cuando alguna lo use").into());
    }
    let subtitle = lines.into_iter().filter(|line| !line.is_empty()).collect::<Vec<_>>().join("\n");
    let key = McpServerKey::of(server);
    let mut row = common::row(&format!("probe-{}", key.target()), KEY, Icon::Machine, server.name.clone(),
        (!subtitle.is_empty()).then_some(subtitle));
    row.link = None;
    row.badges.push(status_label(server.status, false, server.traits.connect != McpConnect::None, lang));
    row
}

/// A server that is the user's own wherever the agent runs, as its provider says; every other one is
/// a project's.
fn personal(server: &McpServerState) -> bool {
    server.traits.scope == McpScope::User
}

fn project_of(task: &TaskId, context: &RenderContext<'_>) -> Option<String> {
    context.domain.tasks.get(task)?.project.clone()
}

fn check_action(project: &str, again: bool, context: &RenderContext<'_>) -> Option<Action> {
    let lang = context.lang;
    let label = if again { t!(lang, "Check again", "Comprobar otra vez") } else { t!(lang, "Check", "Comprobar") };
    let mut check = common::action(Verb::McpCheck, &project_target(project), context.domain.revision, label, Style::Secondary)?;
    check.description = Some(t!(lang, "Starts the agent in the project without asking it anything", "Arranca el agente en el proyecto sin pedirle nada").into());
    Some(check)
}

fn list(mut section: super::Section) -> super::Section {
    section.style = Some(super::SectionStyle::List);
    section
}

pub(super) fn build(context: &RenderContext<'_>) -> View {
    let lang = context.lang;
    let mut view = common::view(KEY, KEY, Some(t!(lang, "MCP servers", "Servidores MCP").into()), context, vec![]);
    let other = t!(lang, "No project", "Sin proyecto").to_owned();
    let mut yours: Vec<Used<'_>> = vec![];
    let mut projects: BTreeMap<String, Vec<Used<'_>>> = BTreeMap::new();
    for node in context.index.mcp_nodes() {
        let project = project_of(&node.task, context).unwrap_or_else(|| other.clone());
        let removed = |server: &McpServerState| context.live.mcp_removed.iter().any(|removal| removal.hides(&node.agent.0, node.generation, server));
        for server in node.servers.iter().filter(|server| server.name != crate::domain::ALONDRA_MCP_SERVER && !removed(server)) {
            let used = if personal(server) { &mut yours } else { projects.entry(project.clone()).or_default() };
            let key = McpServerKey::of(server);
            let open = node.signing_in.contains(&server.name);
            match used.iter_mut().find(|used| used.key.names(server)) {
                Some(used) => used.reported.push((&node.task, server, open)),
                None => used.push(Used { key, reported: vec![(&node.task, server, open)] }),
            }
        }
    }
    let mut checked: Vec<Item> = vec![];
    for server in context.live.mcp_probes.values().flat_map(|probe| match probe {
        super::LiveMcpProbe::Checked { servers, .. } => servers.as_slice(),
        _ => &[],
    }).filter(|server| personal(server) && server.name != crate::domain::ALONDRA_MCP_SERVER) {
        let listed = yours.iter().any(|used| used.key.names(server))
            || checked.iter().any(|item| matches!(item, Item::Row(row) if row.title == server.name));
        if !listed {
            let mut row = probed_row(server, true, context);
            row.actions.extend(remove_action(server, None, context));
            checked.push(Item::Row(row));
        }
    }
    if !yours.is_empty() || !checked.is_empty() {
        yours.sort_by(|a, b| a.key.cmp(&b.key));
        let mut rows: Vec<Item> = yours.iter().map(|used| server_row(used, context.index.mcp_waiting_for(&used.key).count(), None, context)).collect();
        rows.extend(checked);
        view.sections.push(list(common::section("yours", Some(t!(lang, "Your servers", "Tus servidores").into()), rows)));
    }
    let mut ordered: Vec<(String, Vec<Used<'_>>)> = projects.into_iter().collect();
    ordered.sort_by_key(|(project, _)| *project == other);
    for (project, mut servers) in ordered {
        servers.sort_by(|a, b| a.key.cmp(&b.key));
        let rows = servers.iter().map(|used| {
            let waiting = context.index.mcp_waiting_for(&used.key).count();
            server_row(used, waiting, (project != other).then_some(project.as_str()), context)
        }).collect();
        view.sections.push(list(common::section(&format!("project:{project}"), Some(project), rows)));
    }
    let live_projects: Vec<String> = view.sections.iter().filter_map(|section| section.id.strip_prefix("project:")).map(str::to_owned).collect();
    let mut idle = vec![];
    for entry in context.live.projects.iter().filter(|entry| !live_projects.contains(&entry.name)) {
        let name = entry.name.as_str();
        let (subtitle, again, servers) = match context.live.mcp_probes.get(name) {
            None => (None, false, None),
            Some(super::LiveMcpProbe::Checking) => (Some(t!(lang, "Checking…", "Comprobando…").to_owned()), true, None),
            Some(super::LiveMcpProbe::Failed { error, .. }) => (Some(tf!(lang, "Could not check: {error}", "No se pudo comprobar: {error}")), true, None),
            Some(super::LiveMcpProbe::Checked { servers, at }) => {
                let time = at.get(11..16).unwrap_or_default();
                let servers: Vec<&McpServerState> = servers.iter()
                    .filter(|server| !personal(server) && server.name != crate::domain::ALONDRA_MCP_SERVER).collect();
                let found = if servers.is_empty() {
                    tf!(lang, "No servers of its own · checked at {time}", "Sin servidores propios · comprobado a las {time}")
                } else {
                    tf!(lang, "Checked at {time}", "Comprobado a las {time}")
                };
                (Some(found), true, (!servers.is_empty()).then_some(servers))
            }
        };
        let mut row = common::row(&project_target(name), KEY, Icon::Project, name.to_owned(), subtitle);
        row.link = None;
        if !matches!(context.live.mcp_probes.get(name), Some(super::LiveMcpProbe::Checking))
            && let Some(check) = check_action(name, again, context)
        {
            row.actions.push(check);
        }
        if let Some(servers) = servers {
            row.children = servers.into_iter().map(|server| {
                let mut child = probed_row(server, false, context);
                child.depth = 1;
                child
            }).collect();
        }
        idle.push(Item::Row(row));
    }
    if !idle.is_empty() {
        idle.insert(0, Item::Notice(super::Notice {
            text: t!(lang, "Check reads the state of their servers without starting a task.",
                "Comprobar lee el estado de sus servidores sin empezar ninguna tarea.").into(),
            tone: Tone::Neutral,
            action: None,
        }));
        view.sections.push(list(common::section("idle_projects", Some(t!(lang, "Other projects", "Otros proyectos").into()), idle)));
    }
    view.summary = Some(summary(&view, lang));
    if view.sections.is_empty() {
        view.empty = Some(super::Notice {
            text: t!(lang, "No task uses an MCP server now", "Ahora ninguna tarea usa un servidor MCP").into(),
            tone: Tone::Neutral,
            action: None,
        });
    }
    view
}

/// How many servers the screen lists and how many wait for the user to sign in to them.
fn summary(view: &View, lang: Lang) -> String {
    let servers: Vec<&super::Row> = view.sections.iter().flat_map(|section| {
        let idle = section.id == "idle_projects";
        section.items.iter().filter_map(|item| match item {
            Item::Row(row) => Some(row),
            _ => None,
        }).flat_map(move |row| if idle { row.children.iter().collect::<Vec<_>>() } else { vec![row] })
    }).collect();
    let needs = servers.iter().filter(|row| row.badges.first().is_some_and(|badge| badge.tone == Tone::YourTurn)).count();
    let count = servers.len();
    let listed = match count {
        0 => return t!(lang, "None in use", "Ninguno en uso").into(),
        1 => t!(lang, "1 server", "1 servidor").to_owned(),
        count => tf!(lang, "{count} servers", "{count} servidores"),
    };
    match needs {
        0 => listed,
        1 => format!("{listed} · {}", t!(lang, "1 needs sign-in", "1 pide iniciar sesión")),
        needs => format!("{listed} · {}", tf!(lang, "{needs} need sign-in", "{needs} piden iniciar sesión")),
    }
}
