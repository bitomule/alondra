#[cfg(test)]
#[path = "../../test_support.rs"]
mod test_support;

#[macro_use]
mod i18n;
mod account;
mod cli;
mod commands;
mod client;
mod direct_client;
mod iroh_client;
mod claude_auth;
mod cleanup;
mod handover;
mod doctor;
mod delivery;
mod doc_images;
mod help;
mod mcp;
mod models;
mod provider_accounts;
mod ops;
mod pty;
mod service_setup;
mod service_pair;
mod service_client;
mod client_pair;
mod runtime;
mod setup;
mod node;
mod safe_permissions;
mod setup_projects;
mod state;
mod tui;
mod update;
mod version;

use std::time::Duration;

use anyhow::Result;
use clap::{CommandFactory, FromArgMatches, Parser, Subcommand};
use serde_json::json;

use crate::client::Client;

/// The `Examples:` block under a command's help.
fn examples(lines: &[&str]) -> String {
    let heading = tr!("Examples", "Ejemplos");
    let body: Vec<String> = lines.iter().map(|line| format!("  {line}")).collect();
    format!("{heading}:\n{}", body.join("\n"))
}

#[derive(Parser)]
#[command(name = "alondra", about = tr!(
    "With no command, opens the terminal app — or sets up this Mac, the first time.",
    "Sin comando, abre la app de terminal; la primera vez, configura este Mac."
))]
struct Cli {
    #[arg(short = 'V', long, help = tr!("Print the version of this terminal and of the service", "Muestra la versión de este terminal y la del servicio"))]
    version: bool,
    #[arg(long, global = true, help = tr!("Machine-readable output", "Salida para máquinas"))]
    json: bool,
    #[arg(long, global = true, help = tr!("Never ask; what needs a person exits with 3", "No pregunta nunca; lo que necesita a una persona sale con 3"))]
    no_input: bool,
    #[command(subcommand)]
    command: Option<Cmd>,
}

#[derive(Subcommand)]
enum Cmd {
    #[command(about = tr!("Check everything; --fix repairs or finishes the setup", "Lo comprueba todo; --fix arregla o termina la instalación"),
        after_help = examples(&["alondra doctor", "alondra doctor --fix", "alondra doctor --fix --yes --only service"]))]
    Doctor {
        #[arg(long, help = tr!("Repair what it can, asking what it needs", "Arregla lo que puede y pregunta lo que necesita"))]
        fix: bool,
        #[arg(long, requires = "fix", help = tr!("Accept every repair that would ask, and the proposed projects folder", "Acepta cada arreglo que preguntaría, y la carpeta de proyectos propuesta"))]
        yes: bool,
        #[arg(long, value_delimiter = ',', conflicts_with = "skip", value_parser = clap::builder::PossibleValuesParser::new(doctor::check_ids()),
            help = tr!("Run only these checks, by id", "Solo estas comprobaciones, por id"))]
        only: Vec<String>,
        #[arg(long, value_delimiter = ',', value_parser = clap::builder::PossibleValuesParser::new(doctor::check_ids()),
            help = tr!("Leave out these checks, by id", "Sin estas comprobaciones, por id"))]
        skip: Vec<String>,
        #[arg(long, help = tr!("Also run the slow checks (validating Alondra's plugin for nodes, up to a minute)", "Ejecuta también las comprobaciones lentas (validar el plugin de Alondra para los nodos, hasta un minuto)"))]
        deep: bool,
        #[arg(long, conflicts_with_all = ["fix", "deep"], help = tr!("Send a test notification to the device used most recently", "Envía una notificación de prueba al dispositivo usado más recientemente"))]
        test_push: bool,
    },
    #[command(about = tr!("Install the newest release", "Instala la última versión"),
        long_about = tr!("Install the newest release with Homebrew and wait until the daemon runs it", "Instala la última versión con Homebrew y espera a que el servidor la ejecute"),
        after_help = examples(&["alondra update", "alondra update --check"]))]
    Update {
        #[arg(long, help = tr!("Only say whether a newer release exists", "Solo dice si hay una versión más nueva"))]
        check: bool,
    },
    #[command(about = tr!("Show or change settings", "Enseña o cambia los ajustes"),
        after_help = examples(&["alondra config list", "alondra config set projects_root ~/Developer", "alondra config unset port"]))]
    Config {
        #[command(subcommand)]
        command: ConfigCmd,
    },
    #[command(about = tr!("Questions and tasks, as text", "Preguntas y tareas, en texto"),
        after_help = examples(&["alondra status"]))]
    Status,
    #[command(about = tr!("Answer a question", "Contesta una pregunta"),
        long_about = tr!("Answer a question by its id; the text goes to the option that takes it", "Contesta una pregunta por su id; el texto va a la opción que lo admite"),
        after_help = examples(&["alondra answer q-12 --option 1", "alondra answer q-12 Use the staging database"]))]
    Answer {
        id: String,
        #[arg(long, help = tr!("Option number or id", "Número o id de la opción"))]
        option: Option<String>,
        text: Vec<String>,
    },
    #[command(about = tr!("Send a message to a task or the orchestrator", "Manda un mensaje a una tarea o al orquestador"),
        long_about = tr!("Send a message to a Task's node (task id) or the orchestrator (orchestrator). Anything about one task in progress goes to its task id, not to the orchestrator. A message to the orchestrator is maintenance: it acts on it and the user gets no card, unless --reply asks for an answer.", "Manda un mensaje al nodo de una tarea (id de tarea) o al orquestador (orchestrator). Lo que trata de una tarea en marcha va a su id de tarea, no al orquestador. Un mensaje al orquestador es de mantenimiento: actúa y el usuario no recibe tarjeta, salvo que --reply pida respuesta."),
        after_help = examples(&["alondra send t-login-fix Use the new endpoint", "alondra send orchestrator --reply Why is this task waiting?"]))]
    Send {
        to: String,
        #[arg(long, help = tr!("The orchestrator answers the user with a card", "El orquestador responde al usuario con una tarjeta"))]
        reply: bool,
        text: Vec<String>,
    },
    #[command(about = tr!("Cancel tasks", "Cancela tareas"),
        long_about = tr!("Cancel tasks by id", "Cancela tareas por su id"),
        after_help = examples(&["alondra cancel t-1 t-2 --reason \"Done elsewhere\""]))]
    Cancel {
        #[arg(required = true)]
        tasks: Vec<String>,
        #[arg(long, help = tr!("Why, shown with each task", "Por qué, se muestra en cada tarea"))]
        reason: Option<String>,
    },
    #[command(about = tr!("Devices that use a Mac", "Dispositivos que usan un Mac"),
        after_help = examples(&["alondra device list", "alondra device approve <id>", "alondra device invite"]))]
    Device {
        #[command(subcommand)]
        command: DeviceCmd,
    },
    #[command(about = tr!("The Macs this terminal uses", "Los Macs que usa este terminal"),
        after_help = examples(&["alondra mac list", "alondra mac connect --host 192.168.1.20", "alondra mac forget <mac>"]))]
    Mac {
        #[command(subcommand)]
        command: MacCmd,
    },
    #[command(about = tr!("Your Alondra account", "Tu cuenta de Alondra"),
        after_help = examples(&["alondra account status", "alondra account login"]))]
    Account {
        #[command(subcommand)]
        command: AccountCmd,
    },
    #[command(about = tr!("The Claude accounts nodes may run on", "Las cuentas de Claude con las que pueden trabajar los nodos"),
        after_help = examples(&["alondra accounts list", "alondra accounts add Work", "alondra accounts login Work", "alondra accounts remove Work"]))]
    Accounts {
        #[command(subcommand)]
        command: AccountsCmd,
    },
    #[command(about = tr!("Notifications and access away from home", "Notificaciones y acceso fuera de casa"),
        after_help = examples(&["alondra relay status", "alondra relay on"]))]
    Relay {
        #[command(subcommand)]
        command: RelayCmd,
    },
    #[command(about = tr!("Close the sessions of finished tasks, also the ones stuck working, and the ones stopped without a task; free the tasks of stopped sessions", "Cierra las sesiones de tareas terminadas, también las atascadas trabajando, y las paradas sin tarea; libera las tareas de sesiones paradas"),
        after_help = examples(&["alondra cleanup --dry-run", "alondra cleanup"]))]
    Cleanup {
        #[arg(long, help = tr!("Only show what it would close", "Solo enseña lo que cerraría"))]
        dry_run: bool,
    },
    #[command(about = tr!("What moving to the Agent SDK does with each task in progress", "Qué hace el paso al Agent SDK con cada tarea en curso"),
        long_about = tr!("List each task in progress and whether its node moves to the Agent SDK now, waits until it stops (and why) or stays as it is. Only a dry run: the daemon moves each node by itself.", "Lista cada tarea en curso y si su nodo pasa al Agent SDK ahora, espera a detenerse (y por qué) o se queda como está. Solo es un ensayo: el daemon pasa cada nodo por sí mismo."),
        after_help = examples(&["alondra handover --dry-run"]))]
    Handover {
        #[arg(long, required = true, help = tr!("Only show what would happen; required, since the daemon moves each node by itself", "Solo enseña lo que pasaría; obligatorio, porque el daemon pasa cada nodo por sí mismo"))]
        dry_run: bool,
    },
    #[command(about = tr!("How each model the orchestrator chose did: outcomes, questions, time and cost", "Cómo le fue a cada modelo que eligió el orquestador: resultados, preguntas, tiempo y coste"),
        after_help = examples(&["alondra models --since 7d"]))]
    Models {
        #[arg(long, help = tr!("Only launches in this period, like 7d, 12h or 2w", "Solo los lanzamientos de este periodo, como 7d, 12h o 2w"))]
        since: Option<String>,
    },
    #[command(about = tr!("Audit or safely rebuild the local event-derived state", "Audita o reconstruye de forma segura el estado local derivado de eventos"),
        after_help = examples(&["alondra state audit"]))]
    State {
        #[command(subcommand)]
        command: StateCmd,
    },
    #[command(about = tr!("Inspect or explicitly resolve uncertain deliveries; Local by default", "Revisa o resuelve a mano las entregas inciertas; Local por defecto"),
        after_help = examples(&["alondra delivery list"]))]
    Delivery(delivery::Options),
    #[command(about = tr!("Stop, acknowledge or retry an orchestrator run", "Para, da por resuelta o reintenta una ejecución del orquestador"),
        after_help = examples(&["alondra orchestrator retry <run-id> --confirm"]))]
    Orchestrator {
        #[command(subcommand)]
        command: OrchestratorCmd,
    },
    /// Node MCP server, started by the node plugin.
    #[command(hide = true)]
    McpNode,
    /// Orchestrator MCP server, started by its separate plugin.
    #[command(hide = true)]
    McpOrchestrator,
    /// What the node plugin runs to report a session to alondrad and to hold its connection open.
    #[command(hide = true)]
    Node {
        #[command(subcommand)]
        command: node::Cmd,
    },
    /// Local Claude Code hook transport; the daemon remains the sole state writer.
    #[command(hide = true)]
    Hook,
    /// Prints the raw bytes and the parsed key of every key pressed, to debug terminal input
    #[command(hide = true)]
    Keys,
}

#[derive(Subcommand)]
enum ConfigCmd {
    #[command(about = tr!("Every setting in effect (environment > config.toml > default)", "Todos los ajustes en vigor (entorno > config.toml > por defecto)"))]
    List,
    #[command(about = tr!("One setting's value", "El valor de un ajuste"))]
    Get { key: String },
    #[command(about = tr!(
        "Change a setting: name, projects_root, public_url, listen, port, wake, retention_hours, service_label or max_working_nodes",
        "Cambia un ajuste: name, projects_root, public_url, listen, port, wake, retention_hours, service_label o max_working_nodes"
    ))]
    Set { key: String, value: String },
    #[command(about = tr!("Put a setting back to its default", "Devuelve un ajuste a su valor por defecto"))]
    Unset { key: String },
}

#[derive(Subcommand)]
enum DeviceCmd {
    #[command(about = tr!("Paired devices and pairing requests waiting on this Mac", "Dispositivos vinculados y peticiones de emparejamiento pendientes en este Mac"))]
    List,
    #[command(about = tr!("Let a device that asked to pair use this Mac", "Deja usar este Mac a un dispositivo que pidió emparejarse"))]
    Approve { id: String },
    #[command(about = tr!("Refuse a pairing request", "Rechaza una petición de emparejamiento"))]
    Deny { id: String },
    #[command(about = tr!("Unpair a device", "Desvincula un dispositivo"))]
    Remove { id: String },
    #[command(about = tr!("Invite a device that is not on this network, through the relay", "Invita a un dispositivo que no está en esta red, a través del relay"))]
    Invite,
}

#[derive(Subcommand)]
enum MacCmd {
    #[command(about = tr!("This Mac and the Macs this terminal is connected to", "Este Mac y los Macs a los que está conectado este terminal"))]
    List,
    #[command(about = tr!(
        "Connect this terminal to another Mac: found on this network, at --host, or by Mac ID through the relay",
        "Conecta este terminal con otro Mac: encontrado en esta red, en --host, o por ID de Mac a través del relay"
    ))]
    Connect {
        mac: Option<String>,
        #[arg(long, conflicts_with = "mac", help = tr!("Address of the Mac (IP or name, optional :port)", "Dirección del Mac (IP o nombre, :puerto opcional)"))]
        host: Option<String>,
        #[arg(long, help = tr!("Pair again even if this terminal already trusts that Mac", "Vuelve a emparejar aunque este terminal ya confíe en ese Mac"))]
        repair: bool,
    },
    #[command(about = tr!("Take a Mac off this terminal's list", "Quita un Mac de la lista de este terminal"))]
    Forget { mac: String },
    #[command(about = tr!(
        "Time round trips to a Mac this terminal is connected to, and say which way they went",
        "Mide la ida y vuelta con un Mac al que está conectado este terminal, y dice por qué camino va"
    ))]
    Ping {
        mac: Option<String>,
        #[arg(long, default_value_t = 20, value_parser = clap::value_parser!(u32).range(1..=1000), help = tr!("How many reads", "Cuántas lecturas"))]
        count: u32,
    },
    #[command(about = tr!(
        "Give this Mac's service a new identity; every device pairs again. Asks you to type the Mac's name",
        "Da una identidad nueva al servicio de este Mac; todos los dispositivos se vuelven a emparejar. Pide escribir el nombre del Mac"
    ))]
    ResetIdentity,
}

#[derive(Subcommand)]
enum AccountCmd {
    #[command(about = tr!("Who is signed in", "Quién tiene la sesión iniciada"))]
    Status,
    #[command(about = tr!("Sign in in your browser", "Inicia sesión en el navegador"))]
    Login,
    #[command(about = tr!("Sign out on this Mac", "Cierra la sesión en este Mac"))]
    Logout,
}

#[derive(Subcommand)]
enum AccountsCmd {
    #[command(about = tr!("Every Claude account, with its email, plan and state", "Todas las cuentas de Claude, con su email, plan y estado"))]
    List,
    #[command(about = tr!("Add a Claude account by name and sign it in", "Añade una cuenta de Claude por su nombre e inicia su sesión"))]
    Add {
        #[arg(required = true)]
        name: Vec<String>,
    },
    #[command(about = tr!("Sign a Claude account in again", "Vuelve a iniciar la sesión de una cuenta de Claude"))]
    Login {
        #[arg(required = true)]
        name: Vec<String>,
    },
    #[command(about = tr!("Let new work start on an account again", "Deja que el trabajo nuevo vuelva a empezar en una cuenta"))]
    Enable {
        #[arg(required = true)]
        name: Vec<String>,
    },
    #[command(about = tr!("Stop new work from starting on an account", "Evita que el trabajo nuevo empiece en una cuenta"))]
    Disable {
        #[arg(required = true)]
        name: Vec<String>,
    },
    #[command(about = tr!("Sign an account out and remove it", "Cierra la sesión de una cuenta y la quita"))]
    Remove {
        #[arg(required = true)]
        name: Vec<String>,
    },
    #[command(about = tr!(
        "Whether a node whose account reaches its limit goes on with another account (off by default)",
        "Si un nodo cuya cuenta llega al límite sigue con otra cuenta (desactivado por defecto)"
    ))]
    Move {
        #[arg(value_parser = ["on", "off"])]
        state: Option<String>,
    },
}

#[derive(Subcommand)]
enum RelayCmd {
    #[command(about = tr!("Whether it is on, and with which account", "Si está activado y con qué cuenta"))]
    Status,
    #[command(about = tr!("Turn it on: sign in in your browser and register this Mac", "Actívalo: inicia sesión en el navegador y da de alta este Mac"))]
    On,
    #[command(about = tr!("Turn it off: devices reach this Mac only on the local network", "Desactívalo: los dispositivos solo llegan a este Mac por la red local"))]
    Off,
}

#[derive(Subcommand)]
enum OrchestratorCmd {
    #[command(about = tr!("Stop the active orchestrator execution; inspect partial effects before retrying", "Para la ejecución activa del orquestador; revisa los efectos parciales antes de reintentar"))]
    Cancel { run_id: String },
    #[command(about = tr!("Continue without replaying the interrupted batch after checking its effects", "Sigue sin repetir el lote interrumpido después de revisar sus efectos"))]
    Acknowledge {
        run_id: String,
        #[arg(long, required = true, help = tr!("Confirm that you checked the batch's effects", "Confirma que revisaste los efectos del lote"))]
        confirm: bool,
    },
    #[command(about = tr!("Retry an ambiguous orchestrator run after checking its previous effects", "Reintenta una ejecución dudosa del orquestador después de revisar sus efectos anteriores"))]
    Retry {
        run_id: String,
        #[arg(long, required = true, help = tr!("Confirm that replay may repeat actions from this execution", "Confirma que repetirla puede repetir acciones de esta ejecución"))]
        confirm: bool,
    },
}

#[derive(Subcommand)]
enum StateCmd {
    #[command(about = tr!("Replay and validate the local database", "Reproduce y valida la base de datos local"))]
    Audit,
    #[command(about = tr!("Back up the database and rebuild from a complete event log; stop alondrad first", "Crea una copia y reconstruye desde un historial completo; detén alondrad antes"))]
    Repair,
}

fn main() -> Result<()> {
    unsafe {
        libc::signal(libc::SIGPIPE, libc::SIG_DFL);
    }
    let cli = parse(std::env::args_os());
    if cli.version {
        return version::run(cli.json);
    }
    if cli.json && !takes_json(cli.command.as_ref()) {
        let name = std::env::args().nth(1).filter(|a| !a.starts_with('-')).unwrap_or_else(|| "alondra".into());
        help::localized(Cli::command())
            .error(clap::error::ErrorKind::ArgumentConflict, trf!("--json is not available for `{name}`", "--json no está disponible para `{name}`"))
            .exit();
    }
    let warns = match &cli.command {
        None | Some(Cmd::McpNode | Cmd::McpOrchestrator | Cmd::Hook | Cmd::Node { .. } | Cmd::Keys) => false,
        Some(_) => !cli.json,
    };
    let talks_to_daemon = matches!(&cli.command, Some(Cmd::Status | Cmd::Answer { .. } | Cmd::Send { .. }
        | Cmd::Cancel { .. } | Cmd::Device { .. } | Cmd::Orchestrator { .. } | Cmd::Handover { .. }));
    let result = run(cli.command, cli.json, cli.no_input);
    if warns && let Some(warning) = version::older_warning() {
        eprintln!("{warning}");
    }
    match result {
        Err(error) if error.is::<commands::NeedsPerson>() => {
            eprintln!("{error}");
            std::process::exit(3)
        }
        Err(error) if talks_to_daemon && client::delivery(&error) == client::Delivery::Unsent => {
            eprintln!("{}", tr!("Alondra isn't running on this Mac. Repair it with alondra doctor --fix.", "Alondra no está en marcha en este Mac. Repáralo con alondra doctor --fix."));
            std::process::exit(4)
        }
        result => result,
    }
}

/// The commands that print a document with `--json`; any other one refuses the flag.
fn takes_json(command: Option<&Cmd>) -> bool {
    matches!(command, Some(
        Cmd::Doctor { .. } | Cmd::Models { .. } | Cmd::Cleanup { .. } | Cmd::Handover { .. }
        | Cmd::Config { command: ConfigCmd::List | ConfigCmd::Get { .. } }
        | Cmd::Device { command: DeviceCmd::List }
        | Cmd::Mac { command: MacCmd::List | MacCmd::Ping { .. } }
        | Cmd::Account { command: AccountCmd::Status }
        | Cmd::Accounts { command: AccountsCmd::List }
        | Cmd::Relay { command: RelayCmd::Status }
    ))
}

fn run(command: Option<Cmd>, json: bool, no_input: bool) -> Result<()> {
    match command {
        None => doctor::launch(),
        Some(Cmd::Doctor { fix, yes, only, skip, deep, test_push }) => {
            let select = match (only.is_empty(), skip.is_empty()) {
                (false, _) => doctor::Selection::Only(only),
                (_, false) => doctor::Selection::Skip(skip),
                _ => doctor::Selection::All,
            };
            doctor::run(doctor::Options { json, fix, deep, test_push, no_input, yes, select })
        }
        Some(Cmd::Update { check }) => update::run(check),
        Some(Cmd::Config { command }) => match command {
            ConfigCmd::List => commands::config_list(json),
            ConfigCmd::Get { key } => commands::config_get(&key, json),
            ConfigCmd::Set { key, value } => commands::config_set(&key, Some(&value)),
            ConfigCmd::Unset { key } => commands::config_set(&key, None),
        },
        Some(Cmd::Status) => status(),
        Some(Cmd::Answer { id, option, text }) => cli::answer_cmd(&id, option.as_deref(), &text.join(" ")),
        Some(Cmd::Send { to, reply, text }) => cli::send(&to, &text.join(" "), reply),
        Some(Cmd::Cancel { tasks, reason }) => cli::cancel(&tasks, reason.as_deref()),
        Some(Cmd::Device { command }) => match command {
            DeviceCmd::List => commands::device_list(json),
            DeviceCmd::Approve { id } => commands::device_decide(&id, true),
            DeviceCmd::Deny { id } => commands::device_decide(&id, false),
            DeviceCmd::Remove { id } => commands::device_remove(&id),
            DeviceCmd::Invite => commands::device_invite(),
        },
        Some(Cmd::Mac { command }) => match command {
            MacCmd::List => commands::mac_list(json),
            MacCmd::Connect { mac, host, repair } => commands::mac_connect(mac.as_deref(), host.as_deref(), repair),
            MacCmd::Forget { mac } => commands::mac_forget(&mac),
            MacCmd::Ping { mac, count } => crate::direct_client::ping(mac.as_deref(), count, json),
            MacCmd::ResetIdentity => commands::mac_reset_identity(no_input),
        },
        Some(Cmd::Account { command }) => match command {
            AccountCmd::Status => commands::account_status(json),
            AccountCmd::Login => commands::account_login(no_input),
            AccountCmd::Logout => account::logout(),
        },
        Some(Cmd::Accounts { command }) => match command {
            AccountsCmd::List => provider_accounts::list(json),
            AccountsCmd::Add { name } => provider_accounts::add(&name.join(" "), no_input),
            AccountsCmd::Login { name } => provider_accounts::login(&name.join(" "), no_input),
            AccountsCmd::Enable { name } => provider_accounts::set_enabled(&name.join(" "), true),
            AccountsCmd::Disable { name } => provider_accounts::set_enabled(&name.join(" "), false),
            AccountsCmd::Remove { name } => provider_accounts::remove(&name.join(" ")),
            AccountsCmd::Move { state } => provider_accounts::moving(state.map(|state| state == "on")),
        },
        Some(Cmd::Relay { command }) => match command {
            RelayCmd::Status => commands::relay_status(json),
            RelayCmd::On => commands::relay_on(no_input),
            RelayCmd::Off => commands::relay_off(no_input),
        },
        Some(Cmd::Cleanup { dry_run }) => cleanup::run(dry_run, json),
        Some(Cmd::Handover { .. }) => handover::run(json),
        Some(Cmd::Models { since }) => models::run(since.as_deref(), json),
        Some(Cmd::State { command }) => {
            let home = client::config().home.clone();
            match command {
                StateCmd::Audit => {
                    let (seq, tasks) = state::audit(&home)?;
                    println!("State valid through event {seq}; {tasks} tasks.");
                }
                StateCmd::Repair => println!("{}", state::repair_from_events(&home)?),
            }
            Ok(())
        }
        Some(Cmd::Delivery(options)) => delivery::run(options),
        Some(Cmd::Orchestrator { command }) => match command {
            OrchestratorCmd::Cancel { run_id } => cli::recover(ops::Recovery::Cancel, &run_id),
            OrchestratorCmd::Acknowledge { run_id, confirm: _ } => cli::recover(ops::Recovery::Acknowledge, &run_id),
            OrchestratorCmd::Retry { run_id, confirm: _ } => cli::recover(ops::Recovery::Retry, &run_id),
        },
        Some(Cmd::McpNode) => mcp::serve_node(),
        Some(Cmd::McpOrchestrator) => mcp::serve_orchestrator(),
        Some(Cmd::Hook) => hook(),
        Some(Cmd::Node { command }) => node::run(command),
        Some(Cmd::Keys) => keys_probe(),
    }
}

/// `alondra help more` prints the advanced commands; an old or unknown command gets clap's error
/// with the new help under it.
fn parse(args: impl IntoIterator<Item = std::ffi::OsString>) -> Cli {
    let args: Vec<std::ffi::OsString> = args.into_iter().collect();
    let cmd = help::localized(Cli::command());
    if args.iter().skip(1).map(|a| a.to_string_lossy()).eq(["help", "more"]) {
        print!("{}", help::more(&cmd));
        std::process::exit(0);
    }
    let matches = match cmd.clone().try_get_matches_from(&args) {
        Ok(matches) => matches,
        Err(error) if error.kind() == clap::error::ErrorKind::InvalidSubcommand => {
            let _ = error.print();
            eprintln!("\n{}", cmd.clone().render_help());
            std::process::exit(2)
        }
        Err(error) => error.exit(),
    };
    Cli::from_arg_matches(&matches).unwrap_or_else(|e| e.exit())
}

/// Set for a task's Claude that runs through the Agent SDK host: the host reports what Claude does
/// and asks for its permissions, so the node plugin's hooks tell the daemon nothing.
const SDK_HOST: &str = "ALONDRA_SDK_HOST";

/// A node under the Agent SDK tells the daemon nothing through its hooks: the host reports its
/// facts. What nodes may never do is still refused before the tool runs, here, without the daemon,
/// because a tool an allow rule or the permission mode approves never reaches the host.
fn guard_sdk_node(input: &[u8]) -> Result<()> {
    let Ok(payload) = serde_json::from_slice::<serde_json::Value>(input) else { return Ok(()) };
    if payload.get("hook_event_name").and_then(serde_json::Value::as_str) != Some("PreToolUse") {
        return Ok(());
    }
    let tool = payload.get("tool_name").and_then(serde_json::Value::as_str);
    if let Some(reason) = alondra_core::node_guard::forbidden_for_nodes(tool, payload.get("tool_input")) {
        println!("{}", json!({"hookSpecificOutput": {"hookEventName": "PreToolUse", "permissionDecision": "deny", "permissionDecisionReason": reason}}));
    }
    Ok(())
}

fn hook() -> Result<()> {
    use std::io::{Read, Write};

    let mut input = Vec::new();
    std::io::stdin().read_to_end(&mut input)?;
    if std::env::var_os(SDK_HOST).is_some() {
        return guard_sdk_node(&input);
    }
    let mut payload: serde_json::Value = serde_json::from_slice(&input)?;
    if std::env::var("ALONDRA_LAUNCHED").is_ok_and(|s| !s.is_empty())
        && payload.get("hook_event_name").and_then(serde_json::Value::as_str) == Some("PreToolUse")
        && payload.get("tool_name").and_then(serde_json::Value::as_str) == Some("CronCreate")
        && !short_session_poll(&payload)
    {
        let reason = if std::env::var("ALONDRA_ORCHESTRATOR_EXECUTION").is_ok() {
            "This session is managed by Alondra. Use the alondra schedule_create tool for a durable task visible in Alondra. CronCreate only lives inside this Claude session and expires after seven days."
        } else {
            "This session is managed by Alondra. CronCreate only lives inside this Claude session and expires after seven days. If this work should run on a schedule, ask the orchestrator for a durable Alondra schedule: Alondra's ask tool with recipient kind orchestrator."
        };
        let output = json!({"hookSpecificOutput": {"hookEventName": "PreToolUse", "permissionDecision": "deny", "permissionDecisionReason": reason}});
        println!("{output}");
        return Ok(());
    }
    if payload.get("hook_event_name").and_then(serde_json::Value::as_str) == Some("PermissionRequest") {
        if let Some(decision) = node::permission_request(&payload) {
            println!("{decision}");
        }
        return Ok(());
    }
    if payload.get("hook_event_name").and_then(serde_json::Value::as_str) == Some("MessageDisplay")
        && payload.get("final") != Some(&serde_json::Value::Bool(true))
    {
        std::io::stdout().write_all(b"{}\n")?;
        return Ok(());
    }
    compact_hook_payload(&mut payload);
    if let Ok(run) = std::env::var("ALONDRA_ORCHESTRATOR_EXECUTION") {
        payload["_alondra_run"] = json!(run);
    }
    let event = payload.get("hook_event_name").and_then(serde_json::Value::as_str).map(str::to_string);
    let critical = matches!(event.as_deref(), Some("SessionStart" | "PreToolUse" | "Elicitation"));
    let launched = std::env::var("ALONDRA_LAUNCHED").is_ok_and(|s| !s.is_empty());
    let body = serde_json::to_vec(&json!({"launched": launched, "payload": payload}))?;
    match send_hook(&body) {
        HookReply::Answered => Ok(()),
        HookReply::Refused(reason) if critical => {
            eprintln!("Alondra cannot validate this action: {reason}");
            std::process::exit(2);
        }
        HookReply::Refused(reason) => Err(anyhow::anyhow!(reason)),
        HookReply::Unreachable(error) => {
            eprintln!("Alondra did not answer; continuing without it: {error:#}");
            Ok(())
        }
    }
}

enum HookReply {
    Answered,
    Refused(String),
    Unreachable(anyhow::Error),
}

/// A reply already printed stands even if its acknowledgement is lost: the daemon records
/// that delivery as uncertain. A 5xx, like no answer at all, lets Claude continue; a 4xx is
/// a deliberate refusal.
fn send_hook(body: &[u8]) -> HookReply {
    use std::io::Write;
    use std::os::unix::net::UnixStream;

    let path = client::config().home.join("hooks.sock");
    let exchange = (|| -> Result<Vec<u8>> {
        let mut stream = UnixStream::connect(&path)
            .map_err(|e| anyhow::anyhow!("cannot send hook to {}: {e}", path.display()))?;
        anyhow::ensure!(body.len() <= u32::MAX as usize, "hook input is too large");
        stream.write_all(&(body.len() as u32).to_be_bytes())?;
        stream.write_all(body)?;
        let response = read_reply(&mut stream)?;
        anyhow::ensure!(response.len() >= 2, "daemon closed the hook connection without a reply");
        if u16::from_be_bytes([response[0], response[1]]) < 400 {
            let mut stdout = std::io::stdout();
            if stdout.write_all(&response[2..]).and_then(|()| stdout.flush()).is_ok() {
                let _ = stream.write_all(&[6]);
            }
        }
        Ok(response)
    })();
    let response = match exchange {
        Ok(response) => response,
        Err(error) => return HookReply::Unreachable(error),
    };
    let detail = String::from_utf8_lossy(&response[2..]);
    match u16::from_be_bytes([response[0], response[1]]) {
        status @ 500.. => HookReply::Unreachable(anyhow::anyhow!("daemon failed ({status}): {detail}")),
        status @ 400..=499 => HookReply::Refused(format!("daemon rejected hook ({status}): {detail}")),
        _ => HookReply::Answered,
    }
}

/// A successful reply is one JSON value: once it is complete the hook acknowledges it without
/// waiting for the daemon to close the connection. A refusal's text runs until the close.
fn read_reply(stream: &mut impl std::io::Read) -> Result<Vec<u8>> {
    let mut response = Vec::new();
    let mut chunk = [0; 8192];
    loop {
        let read = stream.read(&mut chunk)?;
        if read == 0 {
            return Ok(response);
        }
        response.extend_from_slice(&chunk[..read]);
        if response.len() > 2 && u16::from_be_bytes([response[0], response[1]]) < 400
            && serde_json::from_slice::<serde::de::IgnoredAny>(&response[2..]).is_ok()
        {
            return Ok(response);
        }
    }
}

/// Alondra schedules start at one-hour intervals. Keep shorter session polling in Claude.
fn short_session_poll(payload: &serde_json::Value) -> bool {
    let cron = payload.pointer("/tool_input/cron").and_then(serde_json::Value::as_str)
        .or_else(|| payload.pointer("/tool_input/cron_expression").and_then(serde_json::Value::as_str));
    let Some(cron) = cron else { return false; };
    let parts: Vec<_> = cron.split_whitespace().collect();
    if parts.len() != 5 { return false; }
    // Any minute field with more than one value can fire more than once an hour,
    // including lists and ranges on specific weekdays. Alondra cannot express it.
    !parts[0].parse::<u8>().is_ok_and(|minute| minute <= 59)
}

#[cfg(test)]
mod schedule_hook_tests {
    use super::*;

    #[test]
    fn short_polls_stay_in_claude_while_daily_work_uses_alondra() {
        assert!(short_session_poll(&json!({"tool_input": {"cron": "*/5 * * * *"}})));
        assert!(short_session_poll(&json!({"tool_input": {"cron": "* * * * *"}})));
        assert!(short_session_poll(&json!({"tool_input": {"cron": "0,30 9-17 * * 1-5"}})));
        assert!(!short_session_poll(&json!({"tool_input": {"cron": "0 9 * * 1"}})));
        assert!(!short_session_poll(&json!({"tool_input": {"cron": "0 * * * *"}})));
    }
}

/// Claude's batch hook includes complete tool responses; Alondra needs only the fact that a
/// batch finished. Assistant display deltas are also deliberately kept out of the event log.
fn compact_hook_payload(payload: &mut serde_json::Value) {
    let event = payload.get("hook_event_name").and_then(serde_json::Value::as_str).map(str::to_string);
    match event.as_deref() {
        Some("PostToolBatch") => {
            let count = payload.get("tool_calls").and_then(serde_json::Value::as_array).map(Vec::len);
            if let Some(map) = payload.as_object_mut() {
                map.remove("tool_calls");
                if let Some(count) = count {
                    map.insert("tool_calls_count".into(), count.into());
                }
            }
        }
        Some("PostToolUse") => {
            // PR detection reads only the last stdout line. Complete tool output can be
            // megabytes and is never otherwise persisted by the daemon.
            // A worktree a subagent or EnterWorktree made is the task's: its path, branch and the
            // subagent's id stay for Alondra to record.
            if let Some(map) = payload.as_object_mut() {
                if let Some(response) = map.remove("tool_response") {
                    let stdout = response.get("stdout").and_then(serde_json::Value::as_str)
                        .or_else(|| response.as_str());
                    let mut kept = serde_json::Map::new();
                    if let Some(last) = stdout.and_then(|s| s.lines().map(str::trim).rfind(|s| !s.is_empty())) {
                        kept.insert("stdout".into(), last.into());
                    }
                    for key in ["worktreePath", "worktreeBranch", "agentId", "status", "action"] {
                        if let Some(value) = response.get(key).filter(|value| value.is_string()) {
                            kept.insert(key.into(), value.clone());
                        }
                    }
                    if !kept.is_empty() {
                        map.insert("tool_response".into(), serde_json::Value::Object(kept));
                    }
                }
            }
        }
        Some("PostToolUseFailure") => { if let Some(map) = payload.as_object_mut() { map.remove("tool_response"); } }
        Some("MessageDisplay") => { if let Some(map) = payload.as_object_mut() { map.remove("delta"); } }
        Some("InstructionsLoaded") => { if let Some(map) = payload.as_object_mut() { map.remove("content"); } }
        _ => {}
    }
}

fn status() -> Result<()> {
    let client = Client::for_user(Duration::from_secs(10));
    let home = client.views("home", None)?;
    let tasks = client.views("tasks", None)?;
    print!("{}", cli::status_text(&home, &tasks));
    Ok(())
}


fn keys_probe() -> anyhow::Result<()> {
    use std::io::Read;
    println!("Press keys; q quits. Shows the raw bytes the terminal sends.\r");
    crossterm::terminal::enable_raw_mode()?;
    let mut stdin = std::io::stdin();
    let mut buf = [0u8; 64];
    let result = loop {
        let n = match stdin.read(&mut buf) {
            Ok(n) => n,
            Err(e) => break Err(e.into()),
        };
        let bytes = &buf[..n];
        let shown: String = bytes.iter().map(|&b| if b == 0x1b { "ESC".to_string() } else if b.is_ascii_graphic() { (b as char).to_string() } else { format!("\\x{b:02x}") }).collect();
        print!("{shown}\r\n");
        if bytes == b"q" {
            break Ok(());
        }
    };
    crossterm::terminal::disable_raw_mode()?;
    result
}


#[cfg(test)]
mod hook_payload_tests {
    use super::*;

    #[test]
    fn a_tool_response_keeps_its_last_line_and_the_worktree_it_names() {
        let mut agent = json!({"hook_event_name": "PostToolUse", "tool_name": "Agent", "tool_input": {"isolation": "worktree"},
            "tool_response": {"status": "completed", "agentId": "a9e1782746c156d58", "content": [{"type": "text", "text": "long report"}],
                "worktreePath": "/repo/.claude/worktrees/agent-a9e1782746c156d58", "worktreeBranch": "worktree-agent-a9e1782746c156d58"}});
        compact_hook_payload(&mut agent);
        assert_eq!(agent["tool_response"], json!({"status": "completed", "agentId": "a9e1782746c156d58",
            "worktreePath": "/repo/.claude/worktrees/agent-a9e1782746c156d58", "worktreeBranch": "worktree-agent-a9e1782746c156d58"}));
        let mut bash = json!({"hook_event_name": "PostToolUse", "tool_name": "Bash", "tool_response": {"stdout": "a\nhttps://github.com/o/r/pull/1\n", "stderr": "noise"}});
        compact_hook_payload(&mut bash);
        assert_eq!(bash["tool_response"], json!({"stdout": "https://github.com/o/r/pull/1"}));
    }
}

#[cfg(test)]
mod mcp_entrypoint_tests {
    use super::*;
    #[test]
    fn node_and_orchestrator_have_fixed_separate_entrypoints() {
        assert!(matches!(Cli::try_parse_from(["alondra", "mcp-node"]).unwrap().command, Some(Cmd::McpNode)));
        assert!(matches!(Cli::try_parse_from(["alondra", "mcp-orchestrator"]).unwrap().command, Some(Cmd::McpOrchestrator)));
        assert!(Cli::try_parse_from(["alondra", "mcp", "--role", "orchestrator"]).is_err());
        assert!(Cli::try_parse_from(["alondra", "mcp-node", "--role", "orchestrator"]).is_err());
    }

    #[test]
    fn cancel_takes_several_tasks_and_there_is_no_close() {
        let Some(Cmd::Cancel { tasks, reason }) = Cli::try_parse_from(["alondra", "cancel", "t-1", "t-2", "--reason", "done elsewhere"]).unwrap().command
            else { panic!("cancel did not parse") };
        assert_eq!((tasks, reason.as_deref()), (vec!["t-1".to_string(), "t-2".to_string()], Some("done elsewhere")));
        assert!(Cli::try_parse_from(["alondra", "cancel"]).is_err());
        assert!(Cli::try_parse_from(["alondra", "close", "node"]).is_err(), "cancelling a task closes its node");
    }
}

#[cfg(test)]
mod api_tests {
    use super::*;
    use clap::error::ErrorKind;

    fn parsed(args: &[&str]) -> Cli {
        Cli::try_parse_from(std::iter::once("alondra").chain(args.iter().copied())).unwrap_or_else(|e| panic!("{args:?}: {e}"))
    }

    fn refused(args: &[&str]) -> ErrorKind {
        match Cli::try_parse_from(std::iter::once("alondra").chain(args.iter().copied())) {
            Ok(_) => panic!("{args:?} parsed"),
            Err(error) => error.kind(),
        }
    }

    #[test]
    fn every_new_command_parses_with_its_verb() {
        assert!(matches!(parsed(&["device", "list"]).command, Some(Cmd::Device { command: DeviceCmd::List })));
        assert!(matches!(parsed(&["device", "approve", "r1"]).command, Some(Cmd::Device { command: DeviceCmd::Approve { id } }) if id == "r1"));
        assert!(matches!(parsed(&["device", "deny", "r1"]).command, Some(Cmd::Device { command: DeviceCmd::Deny { .. } })));
        assert!(matches!(parsed(&["device", "remove", "phone"]).command, Some(Cmd::Device { command: DeviceCmd::Remove { .. } })));
        assert!(matches!(parsed(&["device", "invite"]).command, Some(Cmd::Device { command: DeviceCmd::Invite })));
        assert!(matches!(parsed(&["mac", "list"]).command, Some(Cmd::Mac { command: MacCmd::List })));
        assert!(matches!(parsed(&["mac", "connect", "mac-a"]).command, Some(Cmd::Mac { command: MacCmd::Connect { mac: Some(m), host: None, repair: false } }) if m == "mac-a"));
        assert!(matches!(parsed(&["mac", "connect", "--host", "192.168.1.2", "--repair"]).command, Some(Cmd::Mac { command: MacCmd::Connect { mac: None, host: Some(_), repair: true } })));
        assert_eq!(refused(&["mac", "connect", "mac-a", "--host", "x"]), ErrorKind::ArgumentConflict);
        assert!(matches!(parsed(&["mac", "forget", "Mac mini"]).command, Some(Cmd::Mac { command: MacCmd::Forget { mac } }) if mac == "Mac mini"));
        assert!(matches!(parsed(&["mac", "reset-identity"]).command, Some(Cmd::Mac { command: MacCmd::ResetIdentity })));
        for (verb, want) in [("status", 0), ("login", 1), ("logout", 2)] {
            let Some(Cmd::Account { command }) = parsed(&["account", verb]).command else { panic!("{verb}") };
            assert_eq!(match command { AccountCmd::Status => 0, AccountCmd::Login => 1, AccountCmd::Logout => 2 }, want);
        }
        for (verb, want) in [("status", 0), ("on", 1), ("off", 2)] {
            let Some(Cmd::Relay { command }) = parsed(&["relay", verb]).command else { panic!("{verb}") };
            assert_eq!(match command { RelayCmd::Status => 0, RelayCmd::On => 1, RelayCmd::Off => 2 }, want);
        }
        assert!(matches!(parsed(&["orchestrator", "cancel", "run-1"]).command, Some(Cmd::Orchestrator { command: OrchestratorCmd::Cancel { .. } })));
        assert!(matches!(parsed(&["orchestrator", "retry", "run-1", "--confirm"]).command, Some(Cmd::Orchestrator { command: OrchestratorCmd::Retry { .. } })));
        assert!(matches!(parsed(&["orchestrator", "acknowledge", "run-1", "--confirm"]).command, Some(Cmd::Orchestrator { command: OrchestratorCmd::Acknowledge { .. } })));
        assert_eq!(refused(&["orchestrator", "retry", "run-1"]), ErrorKind::MissingRequiredArgument);
        assert!(matches!(parsed(&["config", "list"]).command, Some(Cmd::Config { command: ConfigCmd::List })));
        assert!(matches!(parsed(&["config", "get", "port"]).command, Some(Cmd::Config { command: ConfigCmd::Get { key } }) if key == "port"));
        assert!(matches!(parsed(&["config", "set", "name", "Ada"]).command, Some(Cmd::Config { command: ConfigCmd::Set { key, value } }) if key == "name" && value == "Ada"));
        assert!(matches!(parsed(&["config", "unset", "port"]).command, Some(Cmd::Config { command: ConfigCmd::Unset { .. } })));
        for noun in ["device", "mac", "account", "relay", "orchestrator", "config"] {
            assert_eq!(refused(&[noun]), ErrorKind::DisplayHelpOnMissingArgumentOrSubcommand, "{noun} needs a verb");
        }
    }

    #[test]
    fn json_and_no_input_are_global_and_json_goes_only_where_a_document_comes_out() {
        let cli = parsed(&["device", "list", "--json", "--no-input"]);
        assert!(cli.json && cli.no_input && takes_json(cli.command.as_ref()));
        let cli = parsed(&["--json", "mac", "list"]);
        assert!(cli.json && takes_json(cli.command.as_ref()));
        for args in [&["doctor"][..], &["models"], &["cleanup"], &["config", "list"], &["config", "get", "port"], &["account", "status"], &["relay", "status"]] {
            assert!(takes_json(parsed(args).command.as_ref()), "{args:?}");
        }
        for args in [&["answer", "q", "--option", "1"][..], &["device", "approve", "r"], &["relay", "on"], &["config", "set", "port", "1"], &["status"]] {
            assert!(!takes_json(parsed(args).command.as_ref()), "{args:?}");
        }
        assert!(takes_json(None) == false);
        assert!(parsed(&["--version", "--json"]).version);
    }

    #[test]
    fn doctor_takes_yes_only_and_skip() {
        let Some(Cmd::Doctor { fix, yes, only, .. }) = parsed(&["doctor", "--fix", "--yes", "--only", "service,plugin"]).command else { panic!() };
        assert!(fix && yes);
        assert_eq!(only, ["service", "plugin"]);
        let Some(Cmd::Doctor { skip, .. }) = parsed(&["doctor", "--skip", "validate", "--skip", "tuicr"]).command else { panic!() };
        assert_eq!(skip, ["validate", "tuicr"]);
        assert_eq!(refused(&["doctor", "--yes"]), ErrorKind::MissingRequiredArgument, "--yes only with --fix");
        assert_eq!(refused(&["doctor", "--only", "service", "--skip", "plugin"]), ErrorKind::ArgumentConflict);
        assert_eq!(refused(&["doctor", "--only", "everything"]), ErrorKind::InvalidValue);
        assert!(matches!(parsed(&["doctor", "--fix", "--no-input"]).command, Some(Cmd::Doctor { fix: true, .. })));
    }

    #[test]
    fn every_old_name_is_an_error() {
        for old in [
            &["setup"][..], &["setup", "--relay"], &["pair"], &["devices"], &["devices", "--approve", "r"], &["machines"],
            &["connect"], &["connect", "--host", "x"], &["login"], &["logout"], &["inbox"],
            &["orchestrator-cancel", "r"], &["orchestrator-acknowledge", "r", "--confirm"], &["orchestrator-retry", "r", "--confirm"],
        ] {
            assert_eq!(refused(old), ErrorKind::InvalidSubcommand, "{old:?}");
        }
        assert_eq!(refused(&["account"]), ErrorKind::DisplayHelpOnMissingArgumentOrSubcommand, "account alone no longer shows the account");
        assert_eq!(refused(&["config", "--get", "port"]), ErrorKind::UnknownArgument);
        assert_eq!(refused(&["config"]), ErrorKind::DisplayHelpOnMissingArgumentOrSubcommand);
        assert_eq!(refused(&["doctor", "--fix", "--force"]), ErrorKind::UnknownArgument);
    }

    #[test]
    fn startup_does_not_accept_remote_destination_flags() {
        assert!(parsed(&[]).command.is_none());
        assert_eq!(refused(&["--mac", "mac-a"]), ErrorKind::UnknownArgument);
        assert_eq!(refused(&["--remote"]), ErrorKind::UnknownArgument);
    }
}

#[cfg(test)]
mod delivery_cli_tests {
    use super::*;
    #[test]
    fn delivery_confirmation_and_destination_flags_are_scoped() {
        assert!(Cli::try_parse_from(["alondra","delivery","retry","7","--expected-version","2"]).is_err());
        assert!(Cli::try_parse_from(["alondra","delivery","retry","7","--expected-version","2","--checked"]).is_err());
        let cli=Cli::try_parse_from(["alondra","delivery","retry","7","--mac","mac-b","--expected-version","2","--checked","--confirm-duplicate-risk"]).unwrap();
        assert!(matches!(cli.command,Some(Cmd::Delivery(delivery::Options{mac:Some(mac),..})) if mac=="mac-b"));
        assert!(Cli::try_parse_from(["alondra","answer","7","--mac","mac-b"]).is_err());
        let cli=Cli::try_parse_from(["alondra","delivery","list"]).unwrap();
        assert!(matches!(cli.command,Some(Cmd::Delivery(delivery::Options{mac:None,..}))));
    }
}
