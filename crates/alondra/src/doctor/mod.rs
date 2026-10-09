mod checks;
mod devices_wait;
mod projects;
mod render;
mod system;
mod ui;
mod wizard;

#[cfg(test)]
mod tests;

use std::io::IsTerminal;
use std::path::{Path, PathBuf};

use anyhow::Result;
use serde_json::{Value, json};

use system::{Memory, System, TestPush};
use ui::{Cancelled, Ui};

const CLI: &str = env!("CARGO_PKG_VERSION");

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Status {
    Pass,
    Warn,
    Fail,
}

impl Status {
    fn word(self) -> &'static str {
        match self {
            Status::Pass => "pass",
            Status::Warn => "warn",
            Status::Fail => "fail",
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Group {
    Service,
    Claude,
    Devices,
    Tools,
}

/// A repair `doctor --fix` can carry out itself.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Action {
    InstallService,
    ReplaceOtherService,
    RestartService,
    PrepareIdentity,
    RepairPlugin,
    SignIn,
    SetProjects,
    EnableRelay,
}

impl Action {
    fn starts_service(self) -> bool {
        matches!(self, Action::InstallService | Action::ReplaceOtherService | Action::RestartService)
    }

    fn progress(self) -> Option<&'static str> {
        match self {
            Action::InstallService | Action::ReplaceOtherService | Action::RestartService =>
                Some(tr!("Starting the background service…", "Arrancando el servicio en segundo plano…")),
            Action::PrepareIdentity => Some(tr!("Preparing this Mac for your devices…", "Preparando este Mac para tus dispositivos…")),
            Action::RepairPlugin => Some(tr!("Setting up the Claude plugin…", "Configurando el plugin de Claude…")),
            Action::EnableRelay => Some(tr!("Turning on notifications and access away from home…", "Activando las notificaciones y el acceso desde fuera de casa…")),
            Action::SignIn | Action::SetProjects => None,
        }
    }
}

/// How a problem gets fixed: on its own, after a yes, or only by a person outside Alondra.
#[derive(Clone, PartialEq, Debug)]
pub enum Fix {
    Auto(Action),
    /// `instead` is what a person does when there is no terminal to ask in.
    Ask { question: String, default: bool, action: Action, instead: String },
    /// A value to pick, such as the projects folder; `instead` as for `Ask`.
    Choose { question: String, proposal: projects::Proposal, instead: String },
    Manual(String),
}

impl Fix {
    pub fn text(&self) -> String {
        match self {
            Fix::Auto(_) | Fix::Ask { .. } | Fix::Choose { .. } => "alondra doctor --fix".into(),
            Fix::Manual(text) => text.clone(),
        }
    }
}

#[derive(Clone, Debug)]
pub struct Outcome {
    pub id: &'static str,
    pub name: &'static str,
    pub group: Group,
    pub status: Status,
    pub detail: String,
    pub fix: Option<Fix>,
}

impl Outcome {
    pub fn fixable(&self) -> bool {
        matches!(self.fix, Some(Fix::Auto(_) | Fix::Ask { .. } | Fix::Choose { .. }))
    }

    fn waits_for_person(&self) -> bool {
        matches!(self.fix, Some(Fix::Ask { .. } | Fix::Choose { .. }))
    }

    fn next_step(&self) -> String {
        match &self.fix {
            Some(Fix::Ask { instead, .. } | Fix::Choose { instead, .. }) => instead.clone(),
            Some(fix) => fix.text(),
            None => String::new(),
        }
    }
}

/// One check: a stable id and how to run it. A check that does not apply returns nothing.
pub struct Check {
    pub id: &'static str,
    pub run: fn(&Env) -> Option<Outcome>,
}

pub struct Env<'a> {
    pub sys: &'a dyn System,
    pub deep: bool,
    /// The wizard on a Mac never set up: the projects folder and the relay are asked for.
    pub first_run: bool,
}

pub fn diagnose(env: &Env) -> Vec<Outcome> {
    checks::CHECKS.iter().filter_map(|c| (c.run)(env)).collect()
}

pub struct Repair {
    pub outcomes: Vec<Outcome>,
    pub applied: Vec<Action>,
}

/// The action a person agreed to (or an automatic one) for this outcome, or none.
fn chosen(outcome: &Outcome, ui: &mut dyn Ui, applied: &[Action]) -> Result<Option<Action>, Cancelled> {
    match &outcome.fix {
        Some(Fix::Auto(action)) => Ok(Some(*action)),
        Some(Fix::Ask { action, .. }) if applied.contains(action) => Ok(Some(*action)),
        Some(Fix::Ask { question, default, action, .. }) => Ok(ui.confirm(question, *default)?.filter(|yes| *yes).map(|_| *action)),
        _ => Ok(None),
    }
}

/// Applies one action. When the daemon does not come up it shows why and offers to try again.
fn apply(env: &Env, ui: &mut dyn Ui, action: Action) -> Result<(), Cancelled> {
    loop {
        let progress = action.progress().map(|text| ui.progress(text));
        let result = env.sys.apply(action);
        if let Some(progress) = progress {
            progress.finish();
        }
        let Err(error) = result else { return Ok(()) };
        if !action.starts_service() {
            ui.note(&[error.to_string()]);
            return Ok(());
        }
        let mut lines = vec![error.to_string()];
        if let Some(record) = env.sys.startup() {
            let (failures, reason) = (record.failures, record.reason);
            lines.push(trf!("Last start failed {failures} times: {reason}", "El último arranque falló {failures} veces: {reason}"));
        }
        let log = env.sys.home().join("alondrad.log");
        let log = log.display();
        lines.push(trf!("{log}, last lines:", "{log}, últimas líneas:"));
        lines.extend(env.sys.log_tail(10).into_iter().map(|l| format!("  {l}")));
        ui.note(&lines);
        let options = [tr!("Try again", "Reintentar").to_string(), tr!("Show the full log", "Ver el log completo").into(), tr!("Stop here", "Parar aquí").into()];
        loop {
            match ui.select(tr!("What now?", "¿Y ahora?"), &options)? {
                Some(0) => break,
                Some(1) => ui.note(&env.sys.log_tail(usize::MAX)),
                _ => return Ok(()),
            }
        }
    }
}

/// The folder a person picks, or the cwd when nobody can be asked. `None` leaves it unchosen.
fn pick_folder(env: &Env, ui: &mut dyn Ui, question: &str, proposal: &projects::Proposal) -> Result<Option<PathBuf>, Cancelled> {
    let home = env.sys.user_home();
    if let Some(why) = &proposal.refused {
        ui.note(std::slice::from_ref(why));
    } else {
        let mut options: Vec<String> = proposal.folders.iter().map(|f| projects::label(f, &home)).collect();
        options.push(tr!("Other folder…", "Otra carpeta…").into());
        match ui.select(question, &options)? {
            None => {
                let cwd = &proposal.folders[0].path;
                let shown = cwd.display();
                ui.note(&[trf!("projects folder: {shown} (cwd)", "carpeta de proyectos: {shown} (cwd)")]);
                return Ok(Some(cwd.clone()));
            }
            Some(i) if i < proposal.folders.len() => return Ok(Some(proposal.folders[i].path.clone())),
            Some(_) => {}
        }
    }
    let cwd = env.sys.cwd();
    loop {
        let Some(typed) = ui.input(tr!("Projects folder (created if missing)", "Carpeta de proyectos (se crea si no existe)"))? else { return Ok(None) };
        match crate::setup_projects::resolve(Path::new(typed.trim()), &home, &cwd) {
            Ok(path) => match projects::refusal(&path) {
                Some(why) => ui.note(&[why]),
                None => return Ok(Some(path)),
            },
            Err(error) => ui.note(&[error.to_string()]),
        }
    }
}

/// Repairs one outcome, asking what needs a person. Returns whether something was changed.
fn fix_one(env: &Env, ui: &mut dyn Ui, outcome: &Outcome, applied: &mut Vec<Action>) -> Result<bool, Cancelled> {
    if let Some(Fix::Choose { question, proposal, .. }) = &outcome.fix {
        let Some(path) = pick_folder(env, ui, question, proposal)? else { return Ok(false) };
        return Ok(match env.sys.set_projects(&path) {
            Ok(()) => {
                applied.push(Action::SetProjects);
                true
            }
            Err(error) => {
                ui.note(&[error.to_string()]);
                false
            }
        });
    }
    let Some(action) = chosen(outcome, ui, applied)? else { return Ok(false) };
    if !applied.contains(&action) {
        apply(env, ui, action)?;
        applied.push(action);
    }
    Ok(true)
}

/// `doctor --fix` and the wizard: walks the checks in order, applies each automatic repair and
/// each one a person agrees to, checks again, and shows each step once all its checks ran. A
/// repair already applied this run is not applied twice.
pub fn repair(env: &Env, ui: &mut dyn Ui) -> Result<Repair, Cancelled> {
    let (mut outcomes, mut applied, mut ran, mut touched) = (vec![], vec![], vec![], vec![]);
    let mut shown = 0;
    for check in checks::CHECKS {
        if let Some(mut outcome) = (check.run)(env) {
            if outcome.status != Status::Pass && fix_one(env, ui, &outcome, &mut applied)? {
                touched.push(check.id);
                outcome = (check.run)(env).unwrap_or(outcome);
            }
            outcomes.push(outcome);
        }
        ran.push(check.id);
        shown = wizard::show_ready(env, ui, shown, &ran, &outcomes, &touched);
    }
    Ok(Repair { outcomes, applied })
}

pub fn outro(env: &Env, repair: &Repair) -> String {
    let failed = repair.outcomes.iter().find(|o| o.status == Status::Fail);
    match failed {
        None if repair.applied.is_empty() => tr!("Nothing to change.", "Nada que cambiar.").into(),
        None if env.first_run => tr!("Alondra is running. Open it on your iPhone or on this Mac.", "Alondra está en marcha. Ábrela en tu iPhone o en este Mac.").into(),
        None => tr!("Everything is set.", "Todo listo.").into(),
        Some(o) => {
            let mut command = o.next_step();
            if env.first_run {
                command = command.replace("alondra doctor --fix", "alondra");
            }
            let why = if o.id == "claude.signin" { tr!("Claude Code needs a sign-in", "Claude Code necesita iniciar sesión").to_string() } else { o.detail.clone() };
            match (env.first_run, command.is_empty()) {
                (true, true) => trf!("Not finished: {why}.", "Sin terminar: {why}."),
                (true, false) => trf!("Not finished: {why}.\nNext: {command}", "Sin terminar: {why}.\nSiguiente: {command}"),
                (false, true) => tr!("Not finished.", "Sin terminar.").into(),
                (false, false) => trf!("Not finished.\nNext: {command}", "Sin terminar.\nSiguiente: {command}"),
            }
        }
    }
}

/// 4 while the daemon does not answer, then 1 for anything broken, then 3 when what is left
/// waits for a person, else 0. Warnings never fail.
pub fn exit_code(outcomes: &[Outcome]) -> i32 {
    let failed: Vec<&Outcome> = outcomes.iter().filter(|o| o.status == Status::Fail).collect();
    if failed.iter().any(|o| o.id == "alondrad") {
        4
    } else if failed.iter().any(|o| !o.waits_for_person()) {
        1
    } else if !failed.is_empty() {
        3
    } else {
        0
    }
}

/// Today's `{ok, checks:[{check,status,detail,fix}]}`, plus `schema`, `id`, `fixable` and
/// `next_step`.
pub fn report(outcomes: &[Outcome]) -> Value {
    let checks: Vec<Value> = outcomes.iter().map(|o| json!({
        "check": o.name, "id": o.id, "status": o.status.word(), "detail": o.detail,
        "fix": o.fix.as_ref().map(Fix::text).unwrap_or_default(), "fixable": o.fixable(),
    })).collect();
    let next = outcomes.iter().find(|o| o.status == Status::Fail).map(Outcome::next_step).filter(|s| !s.is_empty());
    json!({"schema": "alondra.doctor/1", "ok": !outcomes.iter().any(|o| o.status == Status::Fail), "checks": checks, "next_step": next})
}

pub struct TestPushReport {
    pub code: i32,
    pub device: Option<(String, String)>,
    pub detail: String,
    pub next: Option<String>,
}

/// The device used most recently, by when it was last seen, else when it paired.
fn most_recent(devices: &[Value]) -> Option<(String, String)> {
    let when = |d: &Value| d.get("last_seen").and_then(Value::as_i64).or_else(|| d.get("created_at").and_then(Value::as_i64)).unwrap_or(0);
    let device = devices.iter().max_by_key(|d| when(d))?;
    let id = device.get("id").and_then(Value::as_str)?.to_string();
    let name = device.get("name").and_then(Value::as_str).unwrap_or(&id).to_string();
    Some((id, name))
}

pub fn test_push(env: &Env) -> TestPushReport {
    let refuse = |code, detail: &str, next: &str| TestPushReport { code, device: None, detail: detail.into(), next: Some(next.into()) };
    if !env.sys.service_config() || !env.sys.relay() {
        return refuse(3, tr!("off — local network only", "desactivadas: solo red local"), "alondra setup --relay");
    }
    if env.sys.health().is_err() {
        return refuse(4, tr!("Alondra isn't running.", "Alondra no está en marcha."), "alondra doctor --fix");
    }
    let Some((id, name)) = most_recent(&env.sys.devices().unwrap_or_default()) else {
        return refuse(3, tr!("no paired device", "ningún dispositivo emparejado"), tr!("open Alondra on your iPhone", "abre Alondra en tu iPhone"));
    };
    match env.sys.test_push(&id) {
        Ok(()) => {
            let memory = env.sys.memory();
            env.sys.remember(&Memory { test_push: Some(TestPush { device: id.clone(), name: name.clone(), at: env.sys.now() }), ..memory });
            let detail = trf!("Test notification sent to “{name}”. Check that it arrived.", "Notificación de prueba enviada a «{name}». Comprueba que ha llegado.");
            TestPushReport { code: 0, device: Some((id, name)), detail, next: None }
        }
        Err(error) => TestPushReport { code: 1, device: Some((id, name)), detail: error.to_string(), next: None },
    }
}

fn test_push_json(report: &TestPushReport) -> Value {
    let (device, name) = report.device.clone().unzip();
    json!({"schema": "alondra.doctor/1", "ok": report.code == 0,
        "test_push": {"device": device, "name": name, "sent": report.code == 0, "detail": report.detail},
        "next_step": report.next})
}

#[derive(Default, Clone, Copy)]
pub struct Options {
    pub json: bool,
    pub fix: bool,
    pub deep: bool,
    pub test_push: bool,
    pub no_input: bool,
}

/// Questions need a person at a terminal: never with `--no-input`, `ALONDRA_NO_INPUT=1`, inside a
/// session Alondra launched, or without a TTY.
fn can_ask(no_input: bool) -> bool {
    let set = |k: &str| std::env::var(k).is_ok_and(|v| !v.is_empty());
    !no_input
        && std::env::var("ALONDRA_NO_INPUT").as_deref() != Ok("1")
        && !set("ALONDRA_LAUNCHED")
        && std::io::stdin().is_terminal()
        && std::io::stdout().is_terminal()
        && std::io::stderr().is_terminal()
}

fn header() -> String {
    let mac = std::process::Command::new("/usr/sbin/scutil").args(["--get", "ComputerName"]).output().ok()
        .filter(|o| o.status.success()).map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .filter(|n| !n.is_empty()).unwrap_or_else(|| tr!("this Mac", "este Mac").into());
    trf!("Alondra {CLI} on {mac}", "Alondra {CLI} en {mac}")
}

/// The width step names are aligned to.
const WIDTH: usize = 26;

/// `doctor`: prints its report and returns the exit code.
fn check(options: Options) -> Result<i32> {
    let sys = system::Real::new();
    let env = Env { sys: &sys, deep: options.deep, first_run: false };
    if options.test_push {
        let report = test_push(&env);
        if options.json {
            println!("{}", serde_json::to_string_pretty(&test_push_json(&report))?);
        } else {
            println!("{}", report.detail);
            if let Some(next) = &report.next {
                println!("{}", trf!("Next: {next}", "Siguiente: {next}"));
            }
        }
        return Ok(report.code);
    }
    let outcomes = if options.fix {
        let mut ui: Box<dyn Ui> = if can_ask(options.no_input) {
            Box::new(ui::Interactive { width: WIDTH })
        } else if options.json {
            Box::new(ui::Plain { out: std::io::stderr() })
        } else {
            Box::new(ui::Plain { out: std::io::stdout() })
        };
        ui.intro(&trf!("Alondra {CLI} · checking this Mac", "Alondra {CLI} · comprobando este Mac"));
        match repair(&env, ui.as_mut()) {
            Ok(repair) => {
                ui.outro(&outro(&env, &repair));
                repair.outcomes
            }
            Err(Cancelled) => return Ok(130),
        }
    } else {
        diagnose(&env)
    };
    if options.json {
        println!("{}", serde_json::to_string_pretty(&crate::version::with_warning(report(&outcomes)))?);
    } else if !options.fix {
        for line in render::grouped(&header(), &outcomes) {
            println!("{line}");
        }
    }
    Ok(exit_code(&outcomes))
}

pub fn run(options: Options) -> Result<()> {
    match check(options)? {
        0 => Ok(()),
        code => std::process::exit(code),
    }
}

/// The wizard (§3.2): `doctor --fix` that also asks for the projects folder and the relay, then
/// waits for a device to pair. Returns the exit code.
fn wizard() -> i32 {
    let sys = system::Real::new();
    let env = Env { sys: &sys, deep: false, first_run: true };
    let mut ui = ui::Interactive { width: WIDTH };
    ui.intro(&trf!("Alondra {CLI} · setting up this Mac", "Alondra {CLI} · configurando este Mac"));
    let finished = repair(&env, &mut ui).and_then(|repair| {
        let code = exit_code(&repair.outcomes);
        if code == 0 {
            let daemon = devices_wait::Daemon(crate::client::Client::new(std::time::Duration::from_secs(5)));
            devices_wait::wait(&daemon, &mut ui, &devices_wait::Terminal(std::time::Instant::now()))?;
        }
        Ok((code, repair))
    });
    match finished {
        Ok((code, repair)) => {
            ui.outro(&outro(&env, &repair));
            code
        }
        Err(Cancelled) => 130,
    }
}

/// What `alondra` without arguments does (§3.1).
#[derive(Debug, PartialEq)]
pub enum Launch {
    Wizard,
    /// Never set up and nobody to ask: exit 3.
    NotSetUp,
    Tui,
    /// The daemon failed to start recently: say why, then the TUI.
    StartFailed(String),
    OfferRepair,
    /// The daemon is down and nobody to ask: exit 4.
    Down,
}

/// `answers` waits up to five seconds for the daemon; it is asked only on a configured Mac.
pub fn launch_decision(configured: bool, interactive: bool, launched: bool, answers: impl FnOnce() -> bool, failure: impl FnOnce() -> Option<String>) -> Launch {
    if !configured {
        return if interactive && !launched { Launch::Wizard } else { Launch::NotSetUp };
    }
    if answers() {
        return Launch::Tui;
    }
    match (interactive, failure()) {
        (false, _) => Launch::Down,
        (true, Some(reason)) => Launch::StartFailed(reason),
        (true, None) => Launch::OfferRepair,
    }
}

/// Set up means a `config.toml` or a LaunchAgent that starts `alondrad` for this HOME. Anything
/// unreadable counts as set up, so a Mac in doubt opens the TUI and never the wizard.
pub fn configured(alondra_home: &Path, launch_agents: Option<&Path>) -> bool {
    if alondra_core::config::path_in(alondra_home).symlink_metadata().is_ok() {
        return true;
    }
    let Some(dir) = launch_agents else { return false };
    match std::fs::read_dir(dir) {
        Ok(entries) => entries.filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| p.extension().is_some_and(|e| e == "plist"))
            .any(|p| std::fs::read_to_string(p).map_or(true, |t| t.contains("/alondrad</string>"))),
        Err(e) => e.kind() != std::io::ErrorKind::NotFound,
    }
}

fn answers_within(limit: std::time::Duration) -> bool {
    let client = crate::client::Client::new(std::time::Duration::from_millis(100));
    if client.get::<Value>("/daemon").is_ok() {
        return true;
    }
    let spinner = std::io::stderr().is_terminal().then(|| {
        let s = cliclack::spinner();
        s.start(tr!("Waiting for Alondra…", "Esperando a Alondra…"));
        s
    });
    let started = std::time::Instant::now();
    let mut up = false;
    while !up && started.elapsed() < limit {
        std::thread::sleep(std::time::Duration::from_millis(250));
        up = client.get::<Value>("/daemon").is_ok();
    }
    if let Some(s) = spinner {
        s.clear();
    }
    up
}

/// `alondra` without arguments: the wizard on a Mac never set up, a repair when the daemon is
/// down, else the TUI.
pub fn launch() -> Result<()> {
    let env = |k: &str| std::env::var(k).ok();
    let home = alondra_core::config::alondra_home(&env);
    let agents = (!system::sandboxed()).then(|| alondra_core::config::home_dir(&env).join("Library/LaunchAgents"));
    let launched = std::env::var("ALONDRA_LAUNCHED").is_ok_and(|v| !v.is_empty());
    let interactive = can_ask(false);
    let decision = launch_decision(configured(&home, agents.as_deref()), interactive, launched,
        || answers_within(std::time::Duration::from_secs(5)),
        || alondra_core::startup::read(&home).map(|r| r.reason));
    match decision {
        Launch::Wizard => std::process::exit(wizard()),
        Launch::NotSetUp => {
            eprintln!("{}", tr!("Alondra is not set up on this Mac. Run `alondra` in a terminal, or `alondra doctor --fix --no-input`.",
                "Alondra no está configurada en este Mac. Ejecuta `alondra` en una terminal, o `alondra doctor --fix --no-input`."));
            std::process::exit(3)
        }
        Launch::Down => {
            eprintln!("{}", tr!("Alondra isn't running. Run `alondra doctor --fix`.", "Alondra no está en marcha. Ejecuta `alondra doctor --fix`."));
            std::process::exit(4)
        }
        Launch::StartFailed(reason) => {
            let _ = cliclack::log::error(trf!("Alondra didn't start: {reason}\nRun `alondra doctor` for details.",
                "Alondra no arrancó: {reason}\nEjecuta `alondra doctor` para ver los detalles."));
        }
        Launch::OfferRepair => {
            let mut ui = ui::Interactive { width: WIDTH };
            match ui.confirm(tr!("Alondra isn't running. Repair it now?", "Alondra no está en marcha. ¿La reparo ahora?"), true) {
                Err(Cancelled) => std::process::exit(130),
                Ok(Some(true)) => {
                    if check(Options { fix: true, ..Options::default() })? == 130 {
                        std::process::exit(130);
                    }
                }
                Ok(_) => {}
            }
        }
        Launch::Tui => {}
    }
    crate::tui::run()
}
