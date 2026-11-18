//! Bringing Claude Code to the release Alondra needs, as `doctor --fix` and `alondra update` say it.
use alondra_core::claude_code;
use super::system::System as _;
use alondra_core::claude_update::{Failure, Outcome};

pub fn describe(outcome: &Outcome) -> String {
    let min = claude_code::MIN_TEXT;
    match outcome {
        Outcome::Current(version) => trf!("Claude Code {version} is up to date", "Claude Code {version} está al día"),
        Outcome::Updated { from, to } => trf!("Claude Code updated from {from} to {to}", "Claude Code actualizado de {from} a {to}"),
        Outcome::Waiting => tr!(
            "A Claude session is running the Claude Code that would be replaced: Alondra updates it as soon as none is",
            "Una sesión de Claude está usando el Claude Code que se sustituiría: Alondra lo actualizará en cuanto no haya ninguna").into(),
        Outcome::Failed(Failure::NoAnswer) => tr!("Claude Code did not answer `claude --version`", "Claude Code no respondió a `claude --version`").into(),
        Outcome::Failed(Failure::NotUpdatable(path)) => {
            let path = path.display();
            trf!("{path} is a launcher of your own or an install Alondra does not know: update it where you installed it",
                "{path} es un lanzador tuyo o una instalación que Alondra no conoce: actualízalo donde lo instalaste")
        }
        Outcome::Failed(Failure::Refused(output)) => {
            let why = claude_code::tail(output);
            trf!("The Claude Code updater refused: {why}", "El actualizador de Claude Code se negó: {why}")
        }
        Outcome::Failed(Failure::TimedOut) => tr!(
            "The update did not finish within 5 minutes and the previous Claude Code is untouched",
            "La actualización no terminó en 5 minutos y el Claude Code anterior sigue intacto").into(),
        Outcome::Failed(Failure::StillBelow { version, output }) => {
            let why = claude_code::tail(output);
            trf!("The update finished but Claude Code is still {version}: the release channel it follows has no {min} yet, or a launcher of your own keeps the old version ({why})",
                "La actualización terminó pero Claude Code sigue en {version}: el canal de versiones que sigue aún no tiene {min}, o un lanzador tuyo mantiene la versión antigua ({why})")
        }
    }
}

/// `doctor --update-claude`, which `alondra update` runs with the release it just installed: one
/// line saying what happened; the exit code is 0 when Claude Code is at the minimum.
pub fn report() -> i32 {
    let outcome = super::system::Real::new().update_claude();
    println!("{}", describe(&outcome));
    i32::from(!matches!(outcome, Outcome::Current(_) | Outcome::Updated { .. }))
}
