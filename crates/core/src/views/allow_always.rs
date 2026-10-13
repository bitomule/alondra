//! What a permission card and the settings say about «allow always», in the viewer's language.

use super::text::{t, tf};
use super::Lang;
use crate::allow_always::{rule_command, AllowAlways, Why};

/// The programs whose subcommand runs a recipe the repo defines, and the file that defines it.
const RECIPES: &[(&str, &str, &str)] = &[
    ("make", "", "`Makefile`"), ("just", "", "`justfile`"), ("npm", "run", "`package.json`"), ("yarn", "run", "`package.json`"),
    ("pnpm", "run", "`package.json`"), ("bun", "run", "`package.json`"), ("cargo", "run", ""), ("bazel", "run", ""),
    ("bazelisk", "run", ""),
];

/// One rule in words: «`make build` with any arguments», with what it really allows when the
/// command runs a recipe of the repo.
pub fn rule_words(rule: &str, lang: Lang) -> String {
    let command = rule_command(rule);
    let mut words = command.split(' ');
    let (program, sub) = (words.next().unwrap_or_default(), words.next().unwrap_or_default());
    let any = tf!(lang, "`{command}` with any arguments", "`{command}` con cualquier argumento");
    match RECIPES.iter().find(|(p, s, _)| *p == program && (s.is_empty() || *s == sub)) {
        Some((_, _, "")) => format!("{any}{}", t!(lang,
            ": runs the repo's code, even if the node changes it", ": ejecuta el código del repo, también si el nodo lo cambia")),
        Some((_, _, file)) => format!("{any}{}", tf!(lang,
            ": runs what the repo's {file} says, even if the node changes it",
            ": ejecuta lo que diga el {file} del repo, también si el nodo lo cambia")),
        None => any,
    }
}

/// The «allow always» option's description: exactly what stops asking, and where.
pub fn description(offer: &AllowAlways, lang: Lang) -> Option<String> {
    let AllowAlways::Offered { repo, rules, quiet } = offer else { return None };
    let mut text = tf!(lang, "Allows this now and stops asking in `{repo}` for:", "Permite esto ahora y en `{repo}` deja de preguntar por:");
    for rule in rules {
        text.push_str(&format!("\n· {}", rule_words(rule, lang)));
    }
    if !quiet.is_empty() {
        let names = list(&quiet.iter().map(|q| format!("`{q}`")).collect::<Vec<_>>(), lang);
        text.push('\n');
        text.push_str(&if quiet.len() == 1 {
            tf!(lang, "{names} already didn't ask.", "{names} ya no preguntaba.")
        } else {
            tf!(lang, "{names} already didn't ask.", "{names} ya no preguntaban.")
        });
        text.push(' ');
    } else {
        text.push('\n');
    }
    text.push_str(t!(lang, "You can remove it in Settings › Permissions.", "Puedes quitarlo en Ajustes › Permisos."));
    Some(text)
}

/// What the card says once the user chose «allow always».
pub fn chosen(offer: &AllowAlways, lang: Lang) -> Option<String> {
    let AllowAlways::Offered { repo, rules, .. } = offer else { return None };
    let commands = list(&rules.iter().map(|rule| format!("`{} …`", rule_command(rule))).collect::<Vec<_>>(), lang);
    Some(tf!(lang, "Allowed always: {commands} in `{repo}`", "Permitido siempre: {commands} en `{repo}`"))
}

/// The one line a card without «allow always» says about why.
pub fn refusal(offer: &AllowAlways, lang: Lang) -> Option<String> {
    let AllowAlways::Refused { part, why } = offer else { return None };
    let part = format!("`{part}`");
    let reason = match why {
        Why::Deletes => tf!(lang, "{part} deletes or overwrites files and always asks.", "{part} borra o sobrescribe ficheros y siempre pregunta."),
        Why::Git => tf!(lang, "{part} can discard changes or rewrite git history and always asks.",
            "{part} puede descartar cambios o reescribir la historia de git y siempre pregunta."),
        Why::Forced => tf!(lang, "{part} skips the command's safety checks and always asks.",
            "{part} se salta las comprobaciones del comando y siempre pregunta."),
        Why::RunsAnything => tf!(lang, "{part} runs whatever it is given and always asks.", "{part} ejecuta lo que se le pase y siempre pregunta."),
        Why::System => tf!(lang, "{part} acts on the Mac or the network, beyond the repo, and always asks.",
            "{part} actúa sobre el Mac o la red, fuera del repo, y siempre pregunta."),
        Why::Alondra => tf!(lang, "{part} acts on Alondra itself and always asks.", "{part} actúa sobre el propio Alondra y siempre pregunta."),
        Why::Outside => tf!(lang, "{part} reaches outside the repo and always asks.", "{part} toca fuera del repo y siempre pregunta."),
        Why::Unreadable => t!(lang, "the command uses variables, substitutions, loops or subshells, which no rule can cover.",
            "el comando usa variables, sustituciones, bucles o subshells, y ninguna regla puede cubrirlos.").into(),
        Why::NoSubcommand => tf!(lang, "{part} has no subcommand a rule could allow on its own.",
            "{part} no tiene un subcomando que una regla pueda permitir por separado."),
        Why::ChangesDirectory => tf!(lang, "{part} changes directory in a way that always asks.",
            "{part} cambia de directorio de una forma que siempre pregunta."),
        Why::WritesFile => tf!(lang, "{part} writes a file, which keeps asking in this session's mode even with a rule.",
            "{part} escribe un fichero, y en el modo de esta sesión eso sigue preguntando aunque haya regla."),
        Why::AutoIgnores => tf!(lang, "auto mode ignores a rule for {part}.", "el modo auto no hace caso de una regla para {part}."),
        Why::TooMany => t!(lang, "it would need more than 5 rules.", "necesitaría más de 5 reglas.").into(),
        Why::TooLong => t!(lang, "the command is too long for Claude Code to check by rule.",
            "el comando es demasiado largo para que Claude Code lo compruebe con reglas.").into(),
        Why::AlreadyAllowed => t!(lang, "its rules are already allowed, so something else in it asks.",
            "sus reglas ya están permitidas, así que pregunta por otra cosa.").into(),
        Why::Repo => t!(lang, "this repo can't keep its own permissions on this Mac.",
            "este repo no puede guardar sus propios permisos en este Mac.").into(),
    };
    Some(tf!(lang, "Can't allow always: {reason}", "No se puede permitir siempre: {reason}"))
}

fn list(items: &[String], lang: Lang) -> String {
    match items {
        [] => String::new(),
        [one] => one.clone(),
        [rest @ .., last] => format!("{} {} {last}", rest.join(", "), t!(lang, "and", "y")),
    }
}

/// "13 Oct" / "13 oct" from a local ISO time.
fn day(iso: &str, lang: Lang) -> String {
    const EN: [&str; 12] = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];
    const ES: [&str; 12] = ["ene", "feb", "mar", "abr", "may", "jun", "jul", "ago", "sept", "oct", "nov", "dic"];
    let mut parts = iso.get(..10).unwrap_or_default().split('-').skip(1);
    let (Some(month), Some(day)) = (parts.next().and_then(|m| m.parse::<usize>().ok()), parts.next().and_then(|d| d.parse::<u32>().ok())) else {
        return String::new();
    };
    let names = if lang == Lang::Es { ES } else { EN };
    names.get(month.wrapping_sub(1)).map(|name| format!("{day} {name}")).unwrap_or_default()
}

/// This Mac's «Permissions allowed always», one row per rule with what it lets through, its
/// repo, when and from which task, and a Remove button.
pub(super) fn section(context: &super::RenderContext<'_>) -> Option<super::Section> {
    let lang = context.lang;
    let rules = &context.live.allowed_rules;
    if rules.is_empty() {
        return None;
    }
    let rows = rules.iter().filter_map(|rule| {
        let mut facts = vec![rule.repo.clone()];
        let added = day(&rule.added, lang);
        if !added.is_empty() {
            facts.push(tf!(lang, "added {added}", "añadido {added}"));
        }
        if let Some(task) = rule.task_id.as_ref().and_then(|id| context.domain.tasks.get(&crate::domain::TaskId(id.clone()))) {
            let title = &task.title;
            facts.push(tf!(lang, "task «{title}»", "tarea «{title}»"));
        }
        let subject = format!("rule-{}", rule.id);
        let mut row = super::common::row(&subject, "machine", super::Icon::Check, rule_words(&rule.rule, lang), Some(facts.join(" · ")));
        row.link = None;
        let mut remove = super::common::action(super::action_id::Verb::RemoveRule, &subject, context.domain.revision,
            t!(lang, "Remove", "Quitar"), super::Style::Destructive)?;
        remove.stamp = Some(t!(lang, "Removed", "Quitado").into());
        row.actions.push(remove);
        Some(super::Item::Row(row))
    }).collect();
    Some(super::common::section("allowed_rules", Some(t!(lang, "Permissions allowed always", "Permisos permitidos siempre").into()), rows))
}
