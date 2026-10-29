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

/// Alondra's reminder to the orchestrator about a node's question it left unanswered.
pub const UNANSWERED_NOTICE_PREFIX: &str = "question-unanswered-";
pub const UNANSWERED_PROBLEM_PREFIX: &str = "problem-unanswered-question-";
pub const UNANSWERED_REMIND: &str = "remind_again";
pub const UNANSWERED_REMIND_LABEL: &str = "Remind it again";
pub const UNANSWERED_WITHDRAW: &str = "withdraw_question";
pub const UNANSWERED_WITHDRAW_LABEL: &str = "Withdraw the question";
/// Alondra's one reminder to the orchestrator about a node's suggestion it left unsettled.
pub const UNSETTLED_NOTICE_PREFIX: &str = "suggestion-unsettled-";
/// Alondra's one reminder to the orchestrator about a stopped node's case it left unsettled.
pub const STALL_NOTICE_PREFIX: &str = "stall-unsettled-";

/// Alondra's reminder to the orchestrator about a plan review it left unsettled.
pub const PLAN_REVIEW_NOTICE_PREFIX: &str = "plan-review-unsettled-";
pub const PLAN_REVIEW_REMIND_LABEL: &str = "Remind the orchestrator again";
pub const PLAN_REVIEW_KEEP_LABEL: &str = "Keep the plans as they are";

/// Alondra's notice, then one reminder, to the orchestrator about a task that waits for a task
/// that was cancelled.
pub const DEPENDENCY_CANCELLED_NOTICE_PREFIX: &str = "dependency-cancelled-";

const ERROR_SHOWN: usize = 600;
const QUESTION_SHOWN: usize = 160;
/// Held tasks a plan-review problem names before it counts the rest.
const PLAN_REVIEW_NAMED: usize = 3;

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
                "The assistant failed to start {failures} times in a row, so new requests wait. Alondra keeps trying by itself.",
                "El asistente no arrancó {failures} veces seguidas, así que las peticiones nuevas esperan. Alondra lo sigue intentando sola.",
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

/// A node's question Alondra handed over twice that still has no answer: its task stands still.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct UnansweredProblem {
    pub question_id: String,
    pub task_title: String,
    pub question: String,
}

impl UnansweredProblem {
    pub fn to_body(&self) -> String {
        serde_json::to_string(self).unwrap_or_default()
    }

    pub fn from_body(body: &str) -> Option<Self> {
        serde_json::from_str(body).ok()
    }

    pub fn text(&self, lang: Lang) -> (String, String) {
        let title: String = self.task_title.chars().take(48).collect();
        let question: String = self.question.chars().take(QUESTION_SHOWN).collect();
        (
            tf!(lang, "“{title}” still has no answer", "«{title}» sigue sin respuesta", title = title),
            tf!(lang,
                "Alondra handed its question over twice and got no answer. It asked: {question}",
                "Alondra pasó su pregunta dos veces y no hubo respuesta. Preguntó: {question}",
                question = question),
        )
    }
}

/// The tasks that start after one the user decided on, still held because the orchestrator was
/// handed the decision twice and reviewed none of their plans.
impl crate::domain::PlanReviewProblem {
    pub fn text(&self, lang: Lang) -> (String, String) {
        let title: String = self.task_title.chars().take(36).collect();
        let named: Vec<String> = self.dependents.iter().take(PLAN_REVIEW_NAMED).map(|title| {
            let title: String = title.chars().take(28).collect();
            match lang {
                Lang::Es => format!("«{title}»"),
                Lang::En => format!("“{title}”"),
            }
        }).collect();
        let more = self.dependents.len().saturating_sub(PLAN_REVIEW_NAMED);
        let tasks = match more {
            0 => named.join(", "),
            more => tf!(lang, "{named} and {more} more", "{named} y {more} más", named = named.join(", "), more = more),
        };
        (
            tf!(lang, "Tasks after “{title}” wait for a plan review", "Las tareas tras «{title}» esperan revisión de plan", title = title),
            tf!(lang,
                "The orchestrator got your decision twice and has not reviewed whether these plans still hold: {tasks}. They wait until then.",
                "El orquestador recibió tu decisión dos veces y no ha revisado si estos planes siguen valiendo: {tasks}. Esperan hasta entonces.",
                tasks = tasks),
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
        (UNANSWERED_REMIND, UNANSWERED_REMIND_LABEL) => Some((
            t!(lang, "Remind it again", "Recordárselo otra vez"),
            t!(lang, "Alondra hands the question over again so it gets its answer", "Alondra vuelve a pasar la pregunta para que tenga respuesta"),
        )),
        (UNANSWERED_WITHDRAW, UNANSWERED_WITHDRAW_LABEL) => Some((
            t!(lang, "Withdraw the question", "Retirar la pregunta"),
            t!(lang, "The node goes on without that answer, or asks you", "El nodo sigue sin esa respuesta, o te pregunta a ti"),
        )),
        (crate::domain::PLAN_REVIEW_REMIND, PLAN_REVIEW_REMIND_LABEL) => Some((
            t!(lang, "Remind it again", "Recordárselo otra vez"),
            t!(lang, "Alondra hands your decision to the orchestrator again so it reviews those plans", "Alondra vuelve a pasar tu decisión al orquestador para que revise esos planes"),
        )),
        (crate::domain::PLAN_REVIEW_KEEP_PLANS, PLAN_REVIEW_KEEP_LABEL) => Some((
            t!(lang, "Keep the plans as they are", "Dejar los planes como están"),
            t!(lang, "The held tasks start with the plans they have, without the orchestrator's review", "Las tareas retenidas arrancan con el plan que tienen, sin la revisión del orquestador"),
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

    #[test]
    fn a_plan_review_problem_fits_its_card_whatever_the_titles() {
        let long = "An Redraft index phase whose title runs far longer than a card can hold";
        let problem = crate::domain::PlanReviewProblem { review_id: "q".into(), task_title: long.into(), dependents: vec![long.into(); 5] };
        for lang in [Lang::En, Lang::Es] {
            let (need, why) = problem.text(lang);
            assert!(need.chars().count() <= CardSpec::NEED_MAX, "{need}");
            assert!(why.chars().count() <= CardSpec::WHY_MAX, "{} {why}", why.chars().count());
        }
        assert!(problem.text(Lang::Es).1.contains("y 2 más"));
    }
    use crate::domain::CardSpec;

    #[test]
    fn every_text_fits_a_card_and_names_no_orchestrator() {
        let stuck = StuckProblem { failures: 3, error: "spawn failed".into() };
        let unsplit = UnsplitProblem { request_id: "r".into(), title: "A request whose title is far longer than a card's need can hold".into() };
        let unanswered = UnansweredProblem { question_id: "q".into(), task_title: "A task whose title is far longer than a card's need can hold".into(),
            question: "Create one task per phase of the migration and adjust the four queued tasks so they build on them. ".repeat(4) };
        for lang in [Lang::En, Lang::Es] {
            for (need, why) in [stuck.text(lang), unsplit.text(lang), unanswered.text(lang)] {
                assert!(need.chars().count() <= CardSpec::NEED_MAX, "{need}");
                assert!(why.chars().count() <= CardSpec::WHY_MAX, "{why}");
                assert!(!format!("{need} {why}").to_lowercase().contains("orq") && !format!("{need} {why}").to_lowercase().contains("orch"), "{need} {why}");
            }
        }
        assert_eq!(StuckProblem::from_body(&stuck.to_body()), Some(stuck.clone()));
        assert_eq!(UnsplitProblem::from_body(&unsplit.to_body()), Some(unsplit));
        assert_eq!(UnansweredProblem::from_body(&unanswered.to_body()), Some(unanswered));
        assert!(stuck.detail(Lang::Es).contains("Error técnico:\n```\nspawn failed"));
    }

    #[test]
    fn the_options_are_worded_in_spanish() {
        assert_eq!(option_text(UNSPLIT_CLOSE, UNSPLIT_CLOSE_LABEL, Lang::Es).map(|(label, _)| label), Some("Cerrarla"));
        assert_eq!(option_text(STUCK_RETRY, STUCK_RETRY_LABEL, Lang::Es).map(|(label, _)| label), Some("Reintentar ahora"));
        assert_eq!(option_text(UNSPLIT_RETRY, "Something else", Lang::Es), None);
        assert_eq!(option_text(UNANSWERED_REMIND, UNANSWERED_REMIND_LABEL, Lang::Es).map(|(label, _)| label), Some("Recordárselo otra vez"));
        assert_eq!(option_text(UNANSWERED_WITHDRAW, UNANSWERED_WITHDRAW_LABEL, Lang::Es).map(|(label, _)| label), Some("Retirar la pregunta"));
    }
}
