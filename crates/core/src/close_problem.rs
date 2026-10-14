//! Alondra's card for a session it could not close in three tries. Its body holds the task and
//! what stops the close, so every client words it in its own language.

use serde::{Deserialize, Serialize};

use crate::domain::{CardSpec, QuestionOption, clip};
use crate::views::Lang;
use crate::views::text::{t, tf};

pub const CLOSE_PROBLEM_PREFIX: &str = "problem-close-";
pub const CLOSE_NOW: &str = "retry";
pub const CLOSE_NOW_LABEL: &str = "Close it now";
pub const TRY_AGAIN_LABEL: &str = "Try again";
pub const LEAVE_OPEN: &str = "leave";
pub const LEAVE_OPEN_LABEL: &str = "Leave it open";
const ERROR_SHOWN: usize = 600;

/// What stops the close.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CloseStop {
    /// The node keeps working after Alondra cut its turn.
    Working,
    /// Its worktree holds work git does not have elsewhere.
    Unsaved,
    /// Claude Code failed to close it.
    Error,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct CloseProblem {
    pub title: String,
    /// The task was cancelled: only its session is left.
    #[serde(default)]
    pub cancelled: bool,
    pub stop: CloseStop,
    #[serde(default)]
    pub detail: String,
}

impl CloseProblem {
    pub fn to_body(&self) -> String {
        serde_json::to_string(self).unwrap_or_default()
    }

    pub fn from_body(body: &str) -> Option<Self> {
        serde_json::from_str(body).ok()
    }

    /// The options, the one to retry worded by what it does for this stop, and the recommended one.
    pub fn options(&self) -> (Vec<QuestionOption>, &'static str) {
        let retry = if self.stop == CloseStop::Working { CLOSE_NOW_LABEL } else { TRY_AGAIN_LABEL };
        let option = |id: &str, label: &str| QuestionOption { id: id.into(), label: label.into(), description: None };
        let recommended = if self.stop == CloseStop::Unsaved { LEAVE_OPEN } else { CLOSE_NOW };
        (vec![option(CLOSE_NOW, retry), option(LEAVE_OPEN, LEAVE_OPEN_LABEL)], recommended)
    }

    /// What the card says it needs and why, in `lang`.
    pub fn text(&self, lang: Lang) -> (String, String) {
        let need = tf!(lang, "Alondra can't close the session of “{title}”", "Alondra no puede cerrar la sesión de «{title}»", title = self.title);
        let why = if self.cancelled {
            t!(lang,
                "It tried 3 times and failed. The task is already cancelled; only its session is still open.",
                "Lo intentó 3 veces y falló. La tarea ya está cancelada; solo queda su sesión abierta.")
        } else {
            t!(lang,
                "It tried 3 times and failed. The task does not finish until its session is closed or you leave it open.",
                "Lo intentó 3 veces y falló. La tarea no termina hasta que se cierre su sesión o decidas dejarla abierta.")
        };
        (clip(&need, CardSpec::NEED_MAX), why.into())
    }

    /// The card's detail: what stops the close and what each option does.
    pub fn detail(&self, lang: Lang) -> String {
        let detail = self.detail.trim();
        let mut text = match self.stop {
            CloseStop::Working => t!(lang,
                "**What stops it:** the node keeps working even after Alondra cut its turn.\n\n\
**Close it now:** Alondra cuts its turn again and closes the session; whatever it was running stops. What it saved in git stays.",
                "**Qué lo impide:** el nodo sigue trabajando aunque Alondra le cortó el turno.\n\n\
**Cerrarla ahora:** Alondra le vuelve a cortar el turno y cierra la sesión; lo que estuviera ejecutando se detiene. Lo que guardó en git se queda.").to_owned(),
            CloseStop::Unsaved => tf!(lang,
                "**What stops it:** its worktree has work that git has nowhere else: {detail}. Closing it would lose that work.\n\n\
**Try again:** choose it once that work is pushed or discarded; Alondra then closes the session.",
                "**Qué lo impide:** su worktree tiene trabajo que git no tiene en ningún otro sitio: {detail}. Cerrarla lo perdería.\n\n\
**Reintentar:** elígelo cuando ese trabajo esté subido o descartado; entonces Alondra cierra la sesión.",
                detail = detail),
            CloseStop::Error => t!(lang,
                "**What stops it:** Claude Code failed to close it (the error is below).\n\n\
**Try again:** Alondra tries to close it again now.",
                "**Qué lo impide:** Claude Code falló al cerrarla (el error está abajo).\n\n\
**Reintentar:** Alondra vuelve a intentar cerrarla ahora.").to_owned(),
        };
        text.push_str(if self.cancelled {
            t!(lang,
                "\n\n**Leave it open:** Alondra stops trying; the session stays in Fleet and you close it from there when you want.",
                "\n\n**Dejarla abierta:** Alondra deja de intentarlo; la sesión sigue en la flota y la cierras desde ahí cuando quieras.")
        } else {
            t!(lang,
                "\n\n**Leave it open:** the task is marked finished and the session stays in Fleet; you close it from there when you want.",
                "\n\n**Dejarla abierta:** la tarea se da por terminada y la sesión sigue en la flota; la cierras desde ahí cuando quieras.")
        });
        if self.stop == CloseStop::Error && !detail.is_empty() {
            let shown: String = detail.chars().take(ERROR_SHOWN).collect();
            text.push_str(t!(lang, "\n\nError:\n", "\n\nError:\n"));
            text.push_str(&format!("```\n{shown}\n```"));
        }
        text
    }
}

/// The words of this card's options in `lang`, keyed by the id and English label they were built with.
pub fn option_text(id: &str, label: &str, lang: Lang) -> Option<(&'static str, &'static str)> {
    Some(match (id, label) {
        (CLOSE_NOW, CLOSE_NOW_LABEL) => (
            t!(lang, "Close it now", "Cerrarla ahora"),
            t!(lang, "Cuts what it is doing and closes the session", "Corta lo que esté haciendo y cierra la sesión"),
        ),
        (CLOSE_NOW, TRY_AGAIN_LABEL) => (
            t!(lang, "Try again", "Reintentar"),
            t!(lang, "Alondra tries to close the session again now", "Alondra vuelve a intentar cerrar la sesión ahora"),
        ),
        (LEAVE_OPEN, LEAVE_OPEN_LABEL) => (
            t!(lang, "Leave it open", "Dejarla abierta"),
            t!(lang, "Alondra stops trying; you close it from Fleet when you want", "Alondra deja de intentarlo; la cierras desde la flota cuando quieras"),
        ),
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn problem(stop: CloseStop, cancelled: bool, detail: &str) -> CloseProblem {
        CloseProblem { title: "Make the editor readable".into(), cancelled, stop, detail: detail.into() }
    }

    #[test]
    fn the_card_says_in_spanish_what_stops_the_close_and_what_each_option_does() {
        let card = problem(CloseStop::Unsaved, false, "a.txt is not committed");
        let (need, why) = card.text(Lang::Es);
        assert_eq!(need, "Alondra no puede cerrar la sesión de «Make the editor readable»");
        assert!(why.starts_with("Lo intentó 3 veces y falló. La tarea no termina"), "{why}");
        let detail = card.detail(Lang::Es);
        assert!(detail.contains("**Qué lo impide:** su worktree tiene trabajo que git no tiene en ningún otro sitio: a.txt is not committed."), "{detail}");
        assert!(detail.contains("**Reintentar:**") && detail.contains("**Dejarla abierta:** la tarea se da por terminada"), "{detail}");
        assert_eq!(CloseProblem::from_body(&card.to_body()), Some(card));
    }

    #[test]
    fn a_node_that_keeps_working_is_closed_now_and_that_is_recommended() {
        let card = problem(CloseStop::Working, true, "");
        let (options, recommended) = card.options();
        assert_eq!(options.iter().map(|o| (o.id.as_str(), o.label.as_str())).collect::<Vec<_>>(),
            [(CLOSE_NOW, CLOSE_NOW_LABEL), (LEAVE_OPEN, LEAVE_OPEN_LABEL)]);
        assert_eq!(recommended, CLOSE_NOW);
        assert_eq!(option_text(CLOSE_NOW, CLOSE_NOW_LABEL, Lang::Es).unwrap().0, "Cerrarla ahora");
        assert!(card.text(Lang::Es).1.contains("La tarea ya está cancelada"));
        assert!(card.detail(Lang::Es).contains("**Cerrarla ahora:**"));
    }

    #[test]
    fn unsaved_work_recommends_leaving_it_open_and_an_error_shows_the_error() {
        assert_eq!(problem(CloseStop::Unsaved, false, "x").options().1, LEAVE_OPEN);
        let error = problem(CloseStop::Error, false, "claude rm did not answer in 60 s");
        assert_eq!(error.options().0[0].label, TRY_AGAIN_LABEL);
        assert!(error.detail(Lang::En).contains("```\nclaude rm did not answer in 60 s\n```"));
    }

    #[test]
    fn every_text_fits_a_card() {
        for stop in [CloseStop::Working, CloseStop::Unsaved, CloseStop::Error] {
            for cancelled in [false, true] {
                for lang in [Lang::En, Lang::Es] {
                    let (need, why) = CloseProblem { title: "t".repeat(200), cancelled, stop, detail: String::new() }.text(lang);
                    assert!(need.chars().count() <= CardSpec::NEED_MAX, "{need}");
                    assert!(why.chars().count() <= CardSpec::WHY_MAX, "{why}");
                }
            }
        }
    }
}
