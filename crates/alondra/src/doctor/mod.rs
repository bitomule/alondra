mod checks;
mod render;
mod system;
mod ui;

#[cfg(test)]
mod tests;

use std::io::IsTerminal;

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
            Action::SignIn => None,
        }
    }
}

/// How a problem gets fixed: on its own, after a yes, or only by a person outside Alondra.
#[derive(Clone, PartialEq, Debug)]
pub enum Fix {
    Auto(Action),
    /// `instead` is what a person does when there is no terminal to ask in.
    Ask { question: String, default: bool, action: Action, instead: String },
    Manual(String),
}

impl Fix {
    pub fn text(&self) -> String {
        match self {
            Fix::Auto(_) | Fix::Ask { .. } => "alondra doctor --fix".into(),
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
        matches!(self.fix, Some(Fix::Auto(_) | Fix::Ask { .. }))
    }

    fn waits_for_person(&self) -> bool {
        matches!(self.fix, Some(Fix::Ask { .. }))
    }

    fn next_step(&self) -> String {
        match &self.fix {
            Some(Fix::Ask { instead, .. }) => instead.clone(),
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

/// Settings `--fix` writes say so when they were already right.
const SETTINGS: [&str; 3] = ["config", "identity", "plugin"];

fn show(ui: &mut dyn Ui, outcome: &Outcome, touched: bool) {
    let detail = if outcome.status == Status::Pass && !touched && SETTINGS.contains(&outcome.id) {
        tr!("already set", "ya estaba").to_string()
    } else {
        outcome.detail.clone()
    };
    let next = outcome.next_step();
    let fix = (outcome.status != Status::Pass && !next.is_empty()).then_some(next.as_str());
    ui.step(outcome.status, outcome.name, &detail, fix);
}

/// `doctor --fix`: walks the checks in order, applies each automatic repair and each one a person
/// agrees to, and checks again. A repair already applied this run is not applied twice.
pub fn repair(env: &Env, ui: &mut dyn Ui) -> Result<Repair, Cancelled> {
    let (mut outcomes, mut applied) = (vec![], vec![]);
    for check in checks::CHECKS {
        let Some(mut outcome) = (check.run)(env) else { continue };
        let mut touched = false;
        if outcome.status != Status::Pass
            && let Some(action) = chosen(&outcome, ui, &applied)?
        {
            if !applied.contains(&action) {
                apply(env, ui, action)?;
                applied.push(action);
            }
            touched = true;
            outcome = (check.run)(env).unwrap_or(outcome);
        }
        show(ui, &outcome, touched);
        outcomes.push(outcome);
    }
    Ok(Repair { outcomes, applied })
}

pub fn outro(repair: &Repair) -> String {
    let failed = repair.outcomes.iter().find(|o| o.status == Status::Fail);
    match failed {
        None if repair.applied.is_empty() && repair.outcomes.iter().all(|o| o.status == Status::Pass) =>
            tr!("Nothing to change.", "Nada que cambiar.").into(),
        None => tr!("Everything is set.", "Todo listo.").into(),
        Some(o) => {
            let command = o.next_step();
            if command.is_empty() {
                tr!("Not finished.", "Sin terminar.").into()
            } else {
                trf!("Not finished.\nNext: {command}", "Sin terminar.\nSiguiente: {command}")
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

pub fn run(options: Options) -> Result<()> {
    let sys = system::Real::new();
    let env = Env { sys: &sys, deep: options.deep };
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
        std::process::exit(report.code);
    }
    let outcomes = if options.fix {
        let mut ui: Box<dyn Ui> = if can_ask(options.no_input) {
            Box::new(ui::Interactive { width: 14 })
        } else if options.json {
            Box::new(ui::Plain { out: std::io::stderr() })
        } else {
            Box::new(ui::Plain { out: std::io::stdout() })
        };
        ui.intro(&trf!("Alondra {CLI} · checking this Mac", "Alondra {CLI} · comprobando este Mac"));
        match repair(&env, ui.as_mut()) {
            Ok(repair) => {
                ui.outro(&outro(&repair));
                repair.outcomes
            }
            Err(Cancelled) => std::process::exit(130),
        }
    } else {
        diagnose(&env)
    };
    if options.json {
        println!("{}", serde_json::to_string_pretty(&report(&outcomes))?);
    } else if !options.fix {
        for line in render::grouped(&header(), &outcomes) {
            println!("{line}");
        }
    }
    match exit_code(&outcomes) {
        0 => Ok(()),
        code => std::process::exit(code),
    }
}
