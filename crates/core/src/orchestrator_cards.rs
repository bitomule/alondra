//! The orchestrator is internal: the user only hears of it when it fails them. These are Alondra's
//! cards for those failures, with a body every client words in its own language.

use serde::{Deserialize, Serialize};

use crate::views::Lang;
use crate::views::text::{t, tf};

pub const STUCK_PROBLEM_PREFIX: &str = "problem-stuck-orchestrator-";
pub const STUCK_RETRY: &str = "retry_now";
pub const STUCK_RETRY_LABEL: &str = "Try again now";
/// Launches that failed in a row before the user hears of it.
pub const STUCK_AFTER: u32 = 3;

pub const UNSPLIT_PROBLEM_PREFIX: &str = "problem-unsplit-request-";
pub const UNSPLIT_RETRY: &str = "hand_over_again";
pub const UNSPLIT_RETRY_LABEL: &str = "Try again";
pub const UNSPLIT_CLOSE: &str = "close_request";
pub const UNSPLIT_CLOSE_LABEL: &str = "Close it";
/// Alondra's reminder to the orchestrator about a request it left without tasks.
pub const UNSPLIT_NOTICE_PREFIX: &str = "request-unsplit-";

const ERROR_SHOWN: usize = 600;

/// Claude could not start the orchestrator several times in a row: new requests wait for it.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct StuckProblem {
    pub failures: u32,
    #[serde(default)]
    pub error: String,
}

impl StuckProblem {
    pub fn to_body(&self) -> String {
        serde_json::to_string(self).unwrap_or_default()
    }

    pub fn from_body(body: &str) -> Option<Self> {
        serde_json::from_str(body).ok()
    }

    pub fn text(&self, lang: Lang) -> (String, String) {
        (
            t!(lang, "Alondra cannot start the work on your requests", "Alondra no consigue ponerse con tus peticiones").into(),
            tf!(lang,
                "Claude failed to start {failures} times in a row, so new requests wait. Alondra keeps trying by itself.",
                "Claude no arrancó {failures} veces seguidas, así que las peticiones nuevas esperan. Alondra lo sigue intentando sola.",
                failures = self.failures),
        )
    }

    pub fn detail(&self, lang: Lang) -> String {
        let error = self.error.trim();
        if error.is_empty() {
            return String::new();
        }
        let shown: String = error.chars().take(ERROR_SHOWN).collect();
        format!("{}\n```\n{shown}\n```", t!(lang, "Technical error:", "Error técnico:"))
    }
}

/// A request Alondra handed over twice that still has no task, answer or question.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct UnsplitProblem {
    pub request_id: String,
    pub title: String,
}

impl UnsplitProblem {
    pub fn to_body(&self) -> String {
        serde_json::to_string(self).unwrap_or_default()
    }

    pub fn from_body(body: &str) -> Option<Self> {
        serde_json::from_str(body).ok()
    }

    pub fn text(&self, lang: Lang) -> (String, String) {
        let title: String = self.title.chars().take(48).collect();
        (
            tf!(lang, "“{title}” was not turned into tasks", "«{title}» no se ha convertido en tareas", title = title),
            t!(lang,
                "Alondra worked on it twice and ended with no task, no answer and no question for you.",
                "Alondra la procesó dos veces y no salió ninguna tarea, ni una respuesta, ni una pregunta para ti.").into(),
        )
    }
}

/// The words of this module's options, by the id and English label they were built with.
pub fn option_text(id: &str, label: &str, lang: Lang) -> Option<(&'static str, &'static str)> {
    match (id, label) {
        (STUCK_RETRY, STUCK_RETRY_LABEL) => Some((
            t!(lang, "Try again now", "Reintentar ahora"),
            t!(lang, "Alondra starts it again at once instead of waiting for its next try", "Alondra lo arranca otra vez ya, sin esperar al siguiente intento"),
        )),
        (UNSPLIT_RETRY, UNSPLIT_RETRY_LABEL) => Some((
            t!(lang, "Try again", "Volver a intentarlo"),
            t!(lang, "Alondra works on the request again from the start", "Alondra vuelve a procesar la petición desde el principio"),
        )),
        (UNSPLIT_CLOSE, UNSPLIT_CLOSE_LABEL) => Some((
            t!(lang, "Close it", "Cerrarla"),
            t!(lang, "The request closes with no tasks; nothing else changes", "La petición se cierra sin tareas; no cambia nada más"),
        )),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::CardSpec;

    #[test]
    fn every_text_fits_a_card_and_names_no_orchestrator() {
        let stuck = StuckProblem { failures: 3, error: "spawn failed".into() };
        let unsplit = UnsplitProblem { request_id: "r".into(), title: "A request whose title is far longer than a card's need can hold".into() };
        for lang in [Lang::En, Lang::Es] {
            for (need, why) in [stuck.text(lang), unsplit.text(lang)] {
                assert!(need.chars().count() <= CardSpec::NEED_MAX, "{need}");
                assert!(why.chars().count() <= CardSpec::WHY_MAX, "{why}");
                assert!(!format!("{need} {why}").to_lowercase().contains("orq") && !format!("{need} {why}").to_lowercase().contains("orch"), "{need} {why}");
            }
        }
        assert_eq!(StuckProblem::from_body(&stuck.to_body()), Some(stuck.clone()));
        assert_eq!(UnsplitProblem::from_body(&unsplit.to_body()), Some(unsplit));
        assert!(stuck.detail(Lang::Es).contains("Error técnico:\n```\nspawn failed"));
    }

    #[test]
    fn the_options_are_worded_in_spanish() {
        assert_eq!(option_text(UNSPLIT_CLOSE, UNSPLIT_CLOSE_LABEL, Lang::Es).map(|(label, _)| label), Some("Cerrarla"));
        assert_eq!(option_text(STUCK_RETRY, STUCK_RETRY_LABEL, Lang::Es).map(|(label, _)| label), Some("Reintentar ahora"));
        assert_eq!(option_text(UNSPLIT_RETRY, "Something else", Lang::Es), None);
    }
}
