//! Alondra's card for a task that cannot start by itself. Its body holds the problem, so every
//! client words it in its own language.

use crate::domain::{CardKind, CardSpec, ContentKind, QuestionOption, START_CANCEL, START_RETRIES, START_RETRY, START_WITHOUT, StartProblem, clip};
use crate::views::Lang;
use crate::views::text::{t, tf};

pub fn from_body(body: &str) -> Option<StartProblem> {
    serde_json::from_str(body).ok()
}

/// What the card says about `title`'s problem; `dependency` is the title of the cancelled task it
/// waited for.
pub fn card(title: &str, problem: &StartProblem, dependency: Option<&str>, lang: Lang) -> CardSpec {
    let need = |text: String| clip(&text, CardSpec::NEED_MAX);
    let why = |text: String| clip(&text, CardSpec::WHY_MAX);
    match problem {
        StartProblem::LaunchFailed { error } => CardSpec::new(CardKind::Problem, need(tf!(lang, "“{title}” does not start", "«{title}» no arranca")))
            .why(why(tf!(lang, "Alondra tried to start it {n} times and it failed every time.", "Alondra intentó arrancarla {n} veces y falló todas.", n = START_RETRIES.len() + 1)))
            .content(ContentKind::Diagnosis, error.trim()),
        StartProblem::Untrusted { folder, error } => {
            let name = folder.rsplit('/').find(|part| !part.is_empty()).unwrap_or(folder);
            let reason = match error {
                None => t!(lang, "Claude Code only starts sessions in folders you trust, and it does not trust this one yet.",
                    "Claude Code solo arranca sesiones en carpetas de confianza, y aún no confía en esta.").to_owned(),
                Some(_) => t!(lang, "You chose to trust it, but Alondra could not save that in Claude Code's settings.",
                    "Elegiste confiar en ella, pero Alondra no pudo guardarlo en los ajustes de Claude Code.").to_owned(),
            };
            let mut detail = tf!(lang, "Folder: {folder}", "Carpeta: {folder}");
            if let Some(error) = error {
                detail.push_str(&tf!(lang, "\n\nWhy it failed: {error}", "\n\nPor qué falló: {error}"));
            }
            detail.push_str(&tf!(lang,
                "\n\n**Trust it and start it:** Alondra marks the folder as trusted in Claude Code, the same as accepting Claude Code's trust prompt there, and starts the task.\n\nOr on the Mac: run `claude` in {folder} and accept the prompt.",
                "\n\n**Confiar y arrancarla:** Alondra marca la carpeta como de confianza en Claude Code, igual que aceptar ahí el aviso de confianza de Claude Code, y arranca la tarea.\n\nO en el Mac: ejecuta `claude` en {folder} y acepta el aviso."));
            CardSpec::new(CardKind::Problem, need(tf!(lang, "Trust {name} so “{title}” can start", "Confía en {name} para que «{title}» arranque")))
                .why(why(reason))
                .content(ContentKind::Steps, &detail)
        }
        StartProblem::NodeClosed => CardSpec::new(CardKind::Problem, need(tf!(lang, "Relaunch “{title}” or cancel it", "Relanza «{title}» o cancélala")))
            .why(why(t!(lang, "Its node was closed while the task was still open. A relaunch starts a new node in the same folder, with its files.",
                "Su nodo se cerró con la tarea aún abierta. Al relanzarla, un nodo nuevo la arranca en la misma carpeta, con sus archivos.").to_owned())),
        StartProblem::DependencyCancelled { .. } => {
            let other = dependency.unwrap_or(t!(lang, "A task it depends on", "Una tarea de la que depende"));
            CardSpec::new(CardKind::Problem, need(tf!(lang, "“{other}” was cancelled: start “{title}” without it?", "«{other}» se canceló: ¿arrancar «{title}» sin ella?")))
                .why(why(tf!(lang, "“{title}” was waiting for “{other}” to finish, and it will not.", "«{title}» esperaba a que «{other}» terminara, y no va a terminar.")))
        }
    }
}

/// The ways out the card offers, each saying what choosing it does.
pub fn options(problem: &StartProblem, lang: Lang) -> Vec<QuestionOption> {
    let option = |id: &str, label: &str, description: &str| QuestionOption { id: id.into(), label: label.into(), description: Some(description.into()), inputs: vec![] };
    let cancel = option(START_CANCEL, t!(lang, "Cancel the task", "Cancelar la tarea"), t!(lang, "The task closes without starting", "La tarea se cierra sin arrancar"));
    match problem {
        StartProblem::LaunchFailed { .. } => vec![
            option(START_RETRY, t!(lang, "Try again", "Reintentar"), t!(lang, "Alondra starts it again as soon as there is room", "Alondra la vuelve a arrancar en cuanto haya hueco")),
            cancel,
        ],
        StartProblem::Untrusted { .. } => vec![
            option(START_RETRY, t!(lang, "Trust it and start it", "Confiar y arrancarla"),
                t!(lang, "Alondra marks the folder as trusted in Claude Code, as its trust prompt does, and starts the task",
                    "Alondra marca la carpeta como de confianza en Claude Code, como hace su aviso de confianza, y arranca la tarea")),
            cancel,
        ],
        StartProblem::NodeClosed => vec![
            option(START_RETRY, t!(lang, "Relaunch", "Relanzar"), t!(lang, "A new node starts it in the folder the closed one worked in", "Un nodo nuevo la arranca en la carpeta donde trabajaba el que se cerró")),
            cancel,
        ],
        StartProblem::DependencyCancelled { .. } => vec![
            option(START_WITHOUT, t!(lang, "Start without it", "Arrancar sin ella"), t!(lang, "It no longer waits for that task and starts when there is room", "Deja de esperar a esa tarea y arranca cuando haya hueco")),
            option(START_CANCEL, t!(lang, "Cancel this task", "Cancelar esta tarea"), t!(lang, "It closes without starting", "Se cierra sin arrancar")),
        ],
    }
}
