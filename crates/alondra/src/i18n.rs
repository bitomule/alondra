use std::cell::Cell;
use std::sync::OnceLock;

use alondra_core::{NodeState, ReportKind};

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
    OVERRIDE.with(Cell::get).unwrap_or_else(|| *DETECTED.get_or_init(|| detect(|k| std::env::var(k).ok())))
}

#[cfg(test)]
pub fn set_for_this_thread(l: Lang) {
    OVERRIDE.with(|o| o.set(Some(l)));
}

/// `ALONDRA_LANG` wins; otherwise the POSIX order for messages: LC_ALL, then LC_MESSAGES, then LANG.
pub fn detect(get: impl Fn(&str) -> Option<String>) -> Lang {
    ["ALONDRA_LANG", "LC_ALL", "LC_MESSAGES", "LANG"]
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
        NodeState::Delivered => tr!("delivering a result", "entrega resultado"),
        NodeState::WaitingMachine => tr!("waiting for the machine", "esperando máquina"),
        NodeState::Done => tr!("done", "terminado"),
        NodeState::Unclear => tr!("unclear what it wants", "no sé qué quiere"),
    }
}

pub fn report_kind(k: ReportKind) -> &'static str {
    match k {
        ReportKind::Notice => tr!("notice", "aviso"),
        ReportKind::Document => tr!("document", "documento"),
        ReportKind::Progress => tr!("progress", "progreso"),
        ReportKind::Final => tr!("finished", "terminado"),
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

#[cfg(test)]
mod tests {
    use super::*;

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
    fn the_thread_override_switches_every_string() {
        set_for_this_thread(Lang::En);
        assert_eq!(state(NodeState::Working), "working");
        assert_eq!(comments(2), "2 comments");
        set_for_this_thread(Lang::Es);
        assert_eq!(state(NodeState::Working), "trabajando");
        assert_eq!(comments(1), "1 comentario");
    }
}
