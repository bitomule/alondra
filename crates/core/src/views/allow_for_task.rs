//! What a permission card and the task screen say about «allow in this task», in the viewer's
//! language.

use super::text::{t, tf};
use super::Lang;
use crate::allow_for_task::{AllowForTask, NotForTask};
use crate::domain::{AvailableAction, InteractionId, Task};

/// Where a command runs, as the user knows the folder: the task's worktree, a folder in it, or the
/// path itself.
fn place(cwd: &str, task: Option<&Task>, lang: Lang) -> String {
    let inside = task.and_then(|task| task.worktrees.keys()
        .filter_map(|path| cwd.strip_prefix(path.as_str()).filter(|rest| rest.is_empty() || rest.starts_with('/')))
        .min_by_key(|rest| rest.len()));
    match inside.map(|rest| rest.trim_start_matches('/')) {
        Some("") => t!(lang, "the task's worktree", "el worktree de la tarea").into(),
        Some(inside) => tf!(lang, "`{inside}` in the task's worktree", "`{inside}` en el worktree de la tarea"),
        None => format!("`{cwd}`"),
    }
}

fn shape_words(shape: &[String]) -> String {
    format!("`{} …`", shape.join(" "))
}

/// The option's description: exactly what keeps being allowed, where, and until when.
pub fn description(offer: &AllowForTask, task: Option<&Task>, lang: Lang) -> Option<String> {
    match offer {
        AllowForTask::Exact { command, cwd } => {
            let place = place(cwd, task, lang);
            Some(tf!(lang,
                "Only `{command}`, run from {place}, until the task ends. Any other command, path or branch asks again.",
                "Solo `{command}`, lanzado desde {place}, hasta que cierre la tarea. Otro comando, ruta o rama vuelve a preguntar."))
        }
        AllowForTask::Shape { shape, .. } => {
            let shape = shape_words(shape);
            Some(tf!(lang,
                "{shape} with any arguments, inside the task's worktree and Alondra's folders, until the task ends. Outside them it asks again.",
                "{shape} con cualquier argumento, dentro del worktree de la tarea y las carpetas de Alondra, hasta que cierre la tarea. Fuera de ellas vuelve a preguntar."))
        }
        AllowForTask::Refused { .. } => None,
    }
}

/// What the card says once the user chose «allow in this task».
pub fn chosen(offer: &AllowForTask, lang: Lang) -> Option<String> {
    let allowed = match offer {
        AllowForTask::Exact { command, .. } => format!("`{command}`"),
        AllowForTask::Shape { shape, .. } => shape_words(shape),
        AllowForTask::Refused { .. } => return None,
    };
    Some(tf!(lang, "Allowed in this task: {allowed}", "Permitido en esta tarea: {allowed}"))
}

/// The one line a card without «allow in this task» says about why.
pub fn refusal(offer: &AllowForTask, lang: Lang) -> Option<String> {
    let AllowForTask::Refused { why, part } = offer else { return None };
    let reason = match why {
        NotForTask::NotCommand => t!(lang, "only terminal commands can be.", "solo se puede con comandos de terminal.").into(),
        NotForTask::Persistent => t!(lang, "Claude Code says this request can only be allowed once.",
            "Claude Code indica que esta petición solo se puede permitir una vez.").into(),
        NotForTask::AskRule => t!(lang, "your Claude Code settings ask for this command every time.",
            "tu configuración de Claude Code pide preguntar siempre por este comando.").into(),
        NotForTask::NoFolder => t!(lang, "Alondra doesn't know which folder the command runs from.",
            "Alondra no sabe desde qué carpeta se lanza el comando.").into(),
        NotForTask::Outside => tf!(lang, "`{part}` is outside the task's worktree.", "`{part}` está fuera del worktree de la tarea."),
        NotForTask::Unreadable => t!(lang, "the command uses variables, substitutions or `~`, so Alondra can't tell what it touches.",
            "el comando usa variables, sustituciones o `~`, y Alondra no puede saber qué toca.").into(),
        NotForTask::TooLong => t!(lang, "the command is too long to check.", "el comando es demasiado largo para comprobarlo.").into(),
    };
    Some(tf!(lang, "Can't allow in this task: {reason}", "No se puede permitir en esta tarea: {reason}"))
}

/// The task screen's «Allowed in this task»: one row per grant with what it lets through, from
/// where, how often it answered, and a Revoke button.
pub(super) fn section(task: &Task, revokes: &[(InteractionId, AvailableAction)], lang: Lang) -> Option<super::Section> {
    if task.allowed.is_empty() {
        return None;
    }
    let rows = task.allowed.iter().filter_map(|(card, grant)| {
        let (title, mut facts) = match &grant.allows {
            AllowForTask::Exact { command, cwd } => {
                let place = place(cwd, Some(task), lang);
                (command.clone(), vec![tf!(lang, "exact command, from {place}", "comando exacto, desde {place}")])
            }
            AllowForTask::Shape { shape, .. } => (format!("{} …", shape.join(" ")), vec![t!(lang, "any arguments, inside the task's folders", "cualquier argumento, dentro de las carpetas de la tarea").into()]),
            AllowForTask::Refused { .. } => return None,
        };
        let uses = grant.uses;
        facts.push(match uses {
            0 => t!(lang, "not used yet", "aún sin usar").into(),
            1 => t!(lang, "used once", "usado 1 vez").into(),
            _ => tf!(lang, "used {uses} times", "usado {uses} veces"),
        });
        let mut row = super::common::row(&format!("grant-{card}"), "", super::Icon::Check, title, Some(facts.join(" · ").replace('`', "")));
        row.link = None;
        let available = revokes.iter().find(|(revoked, _)| revoked == card).map(|(_, available)| available);
        if let Some(revoke) = available.filter(|available| available.enabled).and_then(|available| {
            let mut revoke = super::common::action(super::action_id::Verb::RevokeGrant, &task.id.0, available.expected_revision,
                t!(lang, "Revoke", "Revocar"), super::Style::Destructive)?;
            revoke.id = super::action_id::ActionId::new(super::action_id::Verb::RevokeGrant, &task.id.0, available.expected_revision, Some(card.0.clone())).ok()?.to_string();
            revoke.stamp = Some(t!(lang, "Revoked", "Revocado").into());
            Some(revoke)
        }) {
            row.actions.push(revoke);
        }
        Some(super::Item::Row(row))
    }).collect();
    Some(super::common::section("allowed_in_task", Some(t!(lang, "Allowed in this task", "Permitido en esta tarea").into()), rows))
}
