use super::ui::Ui;
use super::{Env, Outcome, Status};

/// When a step shows up even though all its checks pass.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Show {
    Always,
    /// Only in the wizard; `doctor --fix` shows it when something is wrong.
    Wizard,
    /// Only in `doctor --fix`.
    Fix,
    /// Only when something is wrong.
    Problems,
}

pub struct Step {
    pub name: &'static str,
    pub checks: &'static [&'static str],
    pub show: Show,
}

/// The steps of §3.7, in the order a person reads them. Each check belongs to exactly one.
pub fn steps() -> Vec<Step> {
    vec![
        Step { name: tr!("This Mac", "Este Mac"), checks: &["mac"], show: Show::Always },
        Step { name: tr!("AI providers", "Proveedores de IA"), checks: &["providers"], show: Show::Problems },
        Step { name: "Codex", checks: &["codex"], show: Show::Always },
        Step { name: "Claude Code", checks: &["claude"], show: Show::Always },
        Step { name: "", checks: &["claude.profile"], show: Show::Wizard },
        Step { name: tr!("Claude Code sign-in", "Sesión de Claude Code"), checks: &["claude.signin"], show: Show::Always },
        Step { name: tr!("Projects folder", "Carpeta de proyectos"), checks: &["projects"], show: Show::Wizard },
        Step {
            name: tr!("Background service", "Servicio en segundo plano"),
            checks: &["config", "service", "alondrad", "state", "startup", "upgrade", "update", "hooks"],
            show: Show::Always,
        },
        Step { name: tr!("Codex sign-in", "Sesión de Codex"), checks: &["codex.signin"], show: Show::Always },
        Step { name: tr!("Claude plugin", "Plugin de Claude"), checks: &["plugin", "validate"], show: Show::Always },
        Step { name: tr!("Ready for your devices", "Listo para tus dispositivos"), checks: &["identity"], show: Show::Wizard },
        Step {
            name: tr!("Notifications and access away from home", "Notificaciones y acceso desde fuera de casa"),
            checks: &["relay"],
            show: Show::Always,
        },
        Step { name: tr!("Devices", "Dispositivos"), checks: &["devices", "notifications"], show: Show::Fix },
        Step { name: tr!("Direct connection", "Conexión directa"), checks: &["direct"], show: Show::Problems },
        Step { name: tr!("Tools", "Herramientas"), checks: &["tuicr"], show: Show::Problems },
    ]
}

fn find<'a>(outcomes: &'a [&Outcome], id: &str) -> Option<&'a Outcome> {
    outcomes.iter().copied().find(|o| o.id == id)
}

/// The one line a step shows when everything in it passes, in the words of §3.7.
fn summary(env: &Env, step: &Step, outcomes: &[&Outcome], touched: bool) -> String {
    let detail = |id: &str| find(outcomes, id).map(|o| o.detail.clone()).unwrap_or_default();
    let already = tr!("already set", "ya estaba").to_string();
    match step.checks[0] {
        "mac" if !env.first_run && !touched => already,
        "mac" => detail("mac"),
        "codex" => detail("codex"),
        "codex.signin" => detail("codex.signin"),
        "claude" => detail("claude").trim_end_matches(" (Claude Code)").to_string(),
        "claude.profile" => detail("claude.profile"),
        "claude.signin" => tr!("signed in", "sesión iniciada").into(),
        "projects" => detail("projects"),
        "config" if env.first_run => {
            let port = env.sys.config().port;
            trf!("running on port {port}", "en marcha en el puerto {port}")
        }
        "config" => {
            let label = env.sys.service().label;
            trf!("running ({label})", "en marcha ({label})")
        }
        "plugin" if touched => tr!("installed, loads only in Alondra's sessions", "instalado, solo se carga en las sesiones de Alondra").into(),
        "plugin" => already,
        "relay" => tr!("on", "activadas").into(),
        "direct" => detail("direct"),
        "devices" => {
            let n = env.sys.devices().map(|d| d.len()).unwrap_or(0);
            let away = if env.sys.relay() { tr!("on", "sí") } else { tr!("off", "no") };
            trf!("{n} paired · away from home: {away}", "{n} emparejados · fuera de casa: {away}")
        }
        _ => String::new(),
    }
}

fn render(env: &Env, ui: &mut dyn Ui, step: &Step, outcomes: &[&Outcome], touched: bool) {
    let problems: Vec<&Outcome> = outcomes.iter().copied().filter(|o| o.status != Status::Pass).collect();
    if problems.is_empty() {
        let shown = match step.show {
            Show::Always => true,
            Show::Wizard => env.first_run,
            Show::Fix => !env.first_run,
            Show::Problems => false,
        };
        if shown {
            ui.step(Status::Pass, step.name, &summary(env, step, outcomes, touched), None);
        }
        return;
    }
    if step.show == Show::Fix && env.first_run {
        return;
    }
    for o in problems {
        let next = o.next_step();
        ui.step(o.status, step.name, &o.detail, (!next.is_empty()).then_some(next.as_str()));
    }
}

/// Shows, from step `from` on, every step whose checks have all run, and returns the first one
/// still waiting. A step whose checks all stayed silent shows nothing.
pub fn show_ready(env: &Env, ui: &mut dyn Ui, from: usize, ran: &[&str], outcomes: &[Outcome], touched: &[&str]) -> usize {
    let steps = steps();
    let mut next = from;
    while let Some(step) = steps.get(next)
        && step.checks.iter().all(|id| ran.contains(id))
    {
        let mine: Vec<&Outcome> = outcomes.iter().filter(|o| step.checks.contains(&o.id)).collect();
        if !mine.is_empty() {
            render(env, ui, step, &mine, step.checks.iter().any(|id| touched.contains(id)));
        }
        next += 1;
    }
    next
}
