use std::cell::Cell;
use std::sync::OnceLock;

use alondra_core::{BackgroundItem, BackgroundKind, Node, NodeState, OrchestratorState, ReportKind};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Lang {
    En,
    Es,
}

static DETECTED: OnceLock<Lang> = OnceLock::new();

thread_local! {
    static OVERRIDE: Cell<Option<Lang>> = const { Cell::new(None) };
}

pub fn lang() -> Lang {
    OVERRIDE.with(Cell::get).unwrap_or_else(|| {
        *DETECTED.get_or_init(|| detect(|k| if k == SYSTEM_LANGUAGE { system_language() } else { std::env::var(k).ok() }))
    })
}

const SYSTEM_LANGUAGE: &str = "macOS AppleLanguages";

fn system_language() -> Option<String> {
    let out = std::process::Command::new("/usr/bin/defaults").args(["read", "-g", "AppleLanguages"]).output().ok()?;
    let text = String::from_utf8_lossy(&out.stdout);
    let first = text.split(|c: char| c == '"' || c == '(' || c == ',' || c.is_whitespace()).find(|t| !t.is_empty() && *t != ")")?;
    Some(first.to_string())
}

#[cfg(test)]
pub fn set_for_this_thread(l: Lang) {
    OVERRIDE.with(|o| o.set(Some(l)));
}

/// `ALONDRA_LANG` wins, then the macOS system language (what the apps follow), then the POSIX
/// order for messages: LC_ALL, LC_MESSAGES, LANG. Terminals often export en_US regardless.
pub fn detect(get: impl Fn(&str) -> Option<String>) -> Lang {
    ["ALONDRA_LANG", SYSTEM_LANGUAGE, "LC_ALL", "LC_MESSAGES", "LANG"]
        .iter()
        .find_map(|k| get(k).filter(|v| !v.trim().is_empty()))
        .map_or(Lang::En, |v| from_locale(&v))
}

fn from_locale(v: &str) -> Lang {
    let v = v.trim().to_ascii_lowercase();
    if v == "es" || v.starts_with("es_") || v.starts_with("es-") || v.starts_with("es.") || v.starts_with("es@") {
        Lang::Es
    } else {
        Lang::En
    }
}

/// `tr!("English", "Español")` picks the string for the current language.
macro_rules! tr {
    ($en:literal, $es:literal) => {
        match $crate::i18n::lang() {
            $crate::i18n::Lang::En => $en,
            $crate::i18n::Lang::Es => $es,
        }
    };
}

/// `trf!("{n} left", "quedan {n}")`: like `tr!` but both are format strings with inline captures.
macro_rules! trf {
    ($en:literal, $es:literal) => {
        match $crate::i18n::lang() {
            $crate::i18n::Lang::En => format!($en),
            $crate::i18n::Lang::Es => format!($es),
        }
    };
}

pub fn state(s: NodeState) -> &'static str {
    match s {
        NodeState::Working => tr!("working", "trabajando"),
        NodeState::WaitingAnswer => tr!("waiting for an answer", "esperando respuesta"),
        NodeState::YourTurn => tr!("your turn", "te toca"),
        NodeState::WaitingMerge => tr!("waiting for your merge", "esperando tu merge"),
        NodeState::Delivered => tr!("delivering a result", "entrega resultado"),
        NodeState::WaitingMachine => tr!("waiting for the machine", "esperando máquina"),
        NodeState::Done => tr!("done", "terminado"),
        NodeState::Unclear => tr!("unclear what it wants", "no sé qué quiere"),
        NodeState::Stalled => tr!("stalled", "parado"),
    }
}

pub fn orchestrator(s: OrchestratorState) -> &'static str {
    match s {
        OrchestratorState::OnDuty => tr!("on duty", "de guardia"),
        OrchestratorState::Handling => tr!("handling a request", "atendiendo una petición"),
        OrchestratorState::WaitingForYou => tr!("waiting for you", "esperándote"),
        OrchestratorState::Offline => tr!("offline", "desconectado"),
        OrchestratorState::Unknown => tr!("unknown", "desconocido"),
    }
}

/// The orchestrator is shown with its own states, every other node with the node states.
pub fn node_state(n: &Node) -> &'static str {
    n.orchestrator_state.map_or_else(|| state(n.state), orchestrator)
}

pub fn report_kind(k: ReportKind) -> &'static str {
    match k {
        ReportKind::Notice => tr!("notice", "aviso"),
        ReportKind::Document => tr!("document", "documento"),
        ReportKind::Progress => tr!("progress", "progreso"),
        ReportKind::Final => tr!("finished", "terminado"),
    }
}

/// Option labels and hints the server writes in English; free labels pass through.
pub fn option_label(label: &str) -> &str {
    use alondra_core::answer_option as o;
    match label {
        o::APPROVE => tr!("Approve", "Aprobar"),
        o::REQUEST_CHANGES => tr!("Request changes", "Pedir cambios"),
        o::ALLOW => tr!("Allow", "Permitir"),
        o::DENY => tr!("Deny", "Denegar"),
        o::MORE_CONTEXT => tr!("I need more context", "Necesito más contexto"),
        o::DONE => tr!("Done", "Hecho"),
        o::CANNOT => tr!("I can't / explain", "No puedo / explícamelo"),
        o::WRITE_THE_REASON => tr!("Write the reason", "Escribe el motivo"),
        "Write what to change" => tr!("Write what to change", "Escribe qué cambiar"),
        other => other,
    }
}

pub fn open_in_browser() -> &'static str {
    tr!("Open in browser", "Abrir en el navegador")
}

pub fn merge_method(m: alondra_core::pr::MergeMethod) -> &'static str {
    use alondra_core::pr::MergeMethod;
    match m {
        MergeMethod::Merge => tr!("merge commit", "commit de merge"),
        MergeMethod::Squash => tr!("squash", "squash"),
        MergeMethod::Rebase => tr!("rebase", "rebase"),
    }
}

/// Why `POST /v1/nodes/{key}/close` refused, in the user's language and without the server's
/// status code or raw prose. `kind` is the reason alondrad sent; `detail`, when present, names
/// the worktree or files at stake and is not itself translated.
pub fn close_reason(kind: &str, detail: Option<&str>) -> String {
    match kind {
        "busy" => tr!("it is working; wait until it stops", "está trabajando; espera a que termine").to_string(),
        "permission" => {
            tr!("it has a permission pending on a tool call that is running", "tiene un permiso pendiente en una llamada a herramienta en marcha").to_string()
        }
        "orchestrator" => tr!("it is the orchestrator, which Alondra keeps alive", "es el orquestador, que Alondra mantiene vivo").to_string(),
        "worktree" => {
            let detail = detail.unwrap_or_default();
            trf!("it has changes not pushed in {detail}", "tiene cambios sin subir en {detail}")
        }
        _ => detail.unwrap_or(kind).to_string(),
    }
}

pub fn comments(n: usize) -> String {
    match (lang(), n) {
        (Lang::En, 1) => "1 comment".into(),
        (Lang::En, n) => format!("{n} comments"),
        (Lang::Es, 1) => "1 comentario".into(),
        (Lang::Es, n) => format!("{n} comentarios"),
    }
}

pub fn background_kind(k: BackgroundKind) -> &'static str {
    match k {
        BackgroundKind::Agent => tr!("subagent", "subagente"),
        BackgroundKind::Shell => tr!("shell", "shell"),
        BackgroundKind::Other => tr!("task", "tarea"),
    }
}

fn background_count(k: BackgroundKind, n: usize) -> String {
    match (lang(), k, n) {
        (_, _, 1) => format!("1 {}", background_kind(k)),
        (Lang::En, BackgroundKind::Agent, n) => format!("{n} subagents"),
        (Lang::En, BackgroundKind::Shell, n) => format!("{n} shells"),
        (Lang::En, BackgroundKind::Other, n) => format!("{n} tasks"),
        (Lang::Es, BackgroundKind::Agent, n) => format!("{n} subagentes"),
        (Lang::Es, BackgroundKind::Shell, n) => format!("{n} shells"),
        (Lang::Es, BackgroundKind::Other, n) => format!("{n} tareas"),
    }
}

pub fn possibly_hung() -> &'static str {
    tr!("hung?", "¿colgada?")
}

/// "2 subagentes · 1 shell": what the node runs in the background, by kind, or nothing.
pub fn background_badge(items: &[BackgroundItem]) -> Option<String> {
    let parts: Vec<String> = [BackgroundKind::Agent, BackgroundKind::Shell, BackgroundKind::Other]
        .into_iter()
        .filter_map(|k| {
            let n = items.iter().filter(|b| b.kind == k).count();
            (n > 0).then(|| background_count(k, n))
        })
        .collect();
    (!parts.is_empty()).then(|| parts.join(" · "))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_background_badge_counts_each_kind_in_the_user_language() {
        let item = |kind| BackgroundItem { kind, label: String::new(), started_at: 0, suspect: false };
        let items = [item(BackgroundKind::Agent), item(BackgroundKind::Shell), item(BackgroundKind::Agent), item(BackgroundKind::Other)];
        set_for_this_thread(Lang::Es);
        assert_eq!(background_badge(&items).as_deref(), Some("2 subagentes · 1 shell · 1 tarea"));
        assert_eq!(background_badge(&items[1..2]).as_deref(), Some("1 shell"));
        assert_eq!(background_badge(&[item(BackgroundKind::Shell), item(BackgroundKind::Shell)]).as_deref(), Some("2 shells"));
        assert_eq!(background_badge(&[]), None);
        set_for_this_thread(Lang::En);
        assert_eq!(background_badge(&items).as_deref(), Some("2 subagents · 1 shell · 1 task"));
        assert_eq!(background_badge(&items[..1]).as_deref(), Some("1 subagent"));
    }

    #[test]
    fn action_options_read_in_the_user_language() {
        set_for_this_thread(Lang::Es);
        assert_eq!(option_label(alondra_core::answer_option::DONE), "Hecho");
        assert_eq!(option_label(alondra_core::answer_option::CANNOT), "No puedo / explícamelo");
        assert_eq!(option_label("Merge #150"), "Merge #150");
        set_for_this_thread(Lang::En);
        assert_eq!(option_label(alondra_core::answer_option::CANNOT), "I can't / explain");
    }

    fn env(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
        let pairs: Vec<(String, String)> = pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect();
        move |k| pairs.iter().find(|(pk, _)| pk == k).map(|(_, v)| v.clone())
    }

    #[test]
    fn spanish_locales_pick_spanish_and_everything_else_english() {
        for v in ["es_ES.UTF-8", "es", "es_MX", "es-419", "ES_es.utf8", "es.UTF-8", "es@euro"] {
            assert_eq!(detect(env(&[("LANG", v)])), Lang::Es, "{v}");
        }
        for v in ["en_US.UTF-8", "C", "POSIX", "C.UTF-8", "fr_FR.UTF-8", "est_EE", "eu_ES.UTF-8"] {
            assert_eq!(detect(env(&[("LANG", v)])), Lang::En, "{v}");
        }
        assert_eq!(detect(env(&[])), Lang::En);
    }

    #[test]
    fn precedence_is_override_then_lc_all_then_lc_messages_then_lang() {
        assert_eq!(detect(env(&[("ALONDRA_LANG", "en"), ("LC_ALL", "es_ES.UTF-8")])), Lang::En);
        assert_eq!(detect(env(&[("ALONDRA_LANG", "es"), ("LANG", "en_US.UTF-8")])), Lang::Es);
        assert_eq!(detect(env(&[("LC_ALL", "es_ES.UTF-8"), ("LC_MESSAGES", "en_US"), ("LANG", "en_US")])), Lang::Es);
        assert_eq!(detect(env(&[("LC_MESSAGES", "es_ES"), ("LANG", "en_US")])), Lang::Es);
        assert_eq!(detect(env(&[("ALONDRA_LANG", " "), ("LC_ALL", ""), ("LANG", "es_ES")])), Lang::Es);
    }

    #[test]
    fn system_language_beats_the_terminal_locale_but_not_the_override() {
        assert_eq!(detect(env(&[(SYSTEM_LANGUAGE, "es-ES"), ("LC_ALL", "en_US.UTF-8")])), Lang::Es);
        assert_eq!(detect(env(&[("ALONDRA_LANG", "en"), (SYSTEM_LANGUAGE, "es-ES")])), Lang::En);
    }

    #[test]
    fn close_reasons_read_in_the_user_language_without_a_status_code() {
        set_for_this_thread(Lang::Es);
        assert_eq!(close_reason("busy", None), "está trabajando; espera a que termine");
        assert_eq!(close_reason("permission", None), "tiene un permiso pendiente en una llamada a herramienta en marcha");
        assert_eq!(close_reason("worktree", Some("2 commit(s) not pushed in /tmp/x")), "tiene cambios sin subir en 2 commit(s) not pushed in /tmp/x");
        set_for_this_thread(Lang::En);
        assert_eq!(close_reason("busy", None), "it is working; wait until it stops");
        for kind in ["busy", "permission", "worktree"] {
            assert!(!close_reason(kind, Some("x")).contains("40"), "{kind} must not leak a status code");
        }
    }

    #[test]
    fn the_thread_override_switches_every_string() {
        set_for_this_thread(Lang::En);
        assert_eq!(state(NodeState::Working), "working");
        assert_eq!(comments(2), "2 comments");
        set_for_this_thread(Lang::Es);
        assert_eq!(state(NodeState::Working), "trabajando");
        assert_eq!(comments(1), "1 comentario");
    }
}
