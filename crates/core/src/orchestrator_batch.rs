//! Alondra's card for an orchestrator batch that stopped halfway after it may have changed
//! something. Its body holds what happened so every client words it in its own language.

use serde::{Deserialize, Serialize};

use crate::views::Lang;
use crate::views::text::{t, tf};

pub const BATCH_PROBLEM_PREFIX: &str = "problem-orchestrator-";
pub const BATCH_CONTINUE: &str = "continue";
pub const BATCH_CONTINUE_LABEL: &str = "Continue without running it again";
pub const BATCH_RETRY: &str = "retry";
pub const BATCH_RETRY_LABEL: &str = "Run it again from the start";
const SUBJECTS_SHOWN: usize = 5;
const ERROR_SHOWN: usize = 600;

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum BatchStop {
    Interrupted,
    /// Claude refused it one of Alondra's tools.
    Denied,
    /// The user stopped it.
    Cancelled,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct BatchProblem {
    pub stop: BatchStop,
    /// The Alondra tools it called that change something, in order.
    #[serde(default)]
    pub calls: Vec<String>,
    /// What the batch was about: the cards and tasks its messages came from.
    #[serde(default)]
    pub subjects: Vec<String>,
    #[serde(default)]
    pub error: String,
}

/// Whether an orchestrator tool changes anything; the ones that only read never do.
pub fn changes_something(tool: &str) -> bool {
    !matches!(tool, "fleet" | "node" | "machine")
}

fn call_text(tool: &str, lang: Lang) -> &'static str {
    match tool {
        "create_task" => t!(lang, "create a task", "crear una tarea"),
        "launch" => t!(lang, "start a node", "arrancar un nodo"),
        "ask" => t!(lang, "ask you something", "preguntarte algo"),
        "request_input" | "request_action" => t!(lang, "ask you for something", "pedirte algo"),
        "publish_information" => t!(lang, "tell you something", "avisarte de algo"),
        "raise_problem" => t!(lang, "warn you of a problem", "avisarte de un problema"),
        "close_request" => t!(lang, "close a request", "cerrar una petición"),
        "title_request" | "retitle_task" => t!(lang, "rename something", "poner un título"),
        "answer_question" => t!(lang, "answer a node", "responder a un nodo"),
        "send" => t!(lang, "message a node", "escribir a un nodo"),
        "interrupt" => t!(lang, "stop a node", "parar un nodo"),
        "withdraw" => t!(lang, "withdraw a card", "retirar una tarjeta"),
        "schedule_create" => t!(lang, "schedule work", "programar un trabajo"),
        _ => t!(lang, "another change in Alondra", "otro cambio en Alondra"),
    }
}

impl BatchProblem {
    pub fn to_body(&self) -> String {
        serde_json::to_string(self).unwrap_or_default()
    }

    pub fn from_body(body: &str) -> Option<Self> {
        serde_json::from_str(body).ok()
    }

    /// What the card says it needs and why, in `lang`.
    pub fn text(&self, lang: Lang) -> (String, String) {
        let need = t!(lang, "The orchestrator stopped halfway: decide whether to run it again", "El orquestador se cortó a medias: decide si lo repite");
        let sure = !self.calls.is_empty();
        let why = match (self.stop, sure) {
            (BatchStop::Interrupted, true) => t!(lang,
                "It stopped before finishing, after it had already changed things in Alondra. Running it again could do them twice.",
                "Se detuvo antes de terminar, cuando ya había hecho cambios en Alondra. Repetirlo podría hacerlos dos veces."),
            (BatchStop::Interrupted, false) => t!(lang,
                "It stopped before finishing and may have changed things in Alondra already. Running it again could do them twice.",
                "Se detuvo antes de terminar y puede que ya hubiera hecho cambios en Alondra. Repetirlo podría hacerlos dos veces."),
            (BatchStop::Denied, true) => t!(lang,
                "Claude refused it one of Alondra's tools halfway, after it had already changed things. Running it again could do them twice.",
                "Claude le negó una herramienta de Alondra a mitad de trabajo, cuando ya había hecho cambios. Repetirlo podría hacerlos dos veces."),
            (BatchStop::Denied, false) => t!(lang,
                "Claude refused it one of Alondra's tools halfway, and it may have changed things already. Running it again could do them twice.",
                "Claude le negó una herramienta de Alondra a mitad de trabajo y puede que ya hubiera hecho cambios. Repetirlo podría hacerlos dos veces."),
            (BatchStop::Cancelled, true) => t!(lang,
                "You stopped it halfway, after it had already changed things in Alondra. Running it again could do them twice.",
                "Lo paraste a mitad de trabajo, cuando ya había hecho cambios en Alondra. Repetirlo podría hacerlos dos veces."),
            (BatchStop::Cancelled, false) => t!(lang,
                "You stopped it halfway, and it may have changed things in Alondra already. Running it again could do them twice.",
                "Lo paraste a mitad de trabajo y puede que ya hubiera hecho cambios en Alondra. Repetirlo podría hacerlos dos veces."),
        };
        (need.into(), why.into())
    }

    /// The card's detail: what it was handling, what it had done, how to choose, and the error.
    pub fn detail(&self, lang: Lang) -> String {
        let mut text = String::new();
        if !self.subjects.is_empty() {
            text.push_str(t!(lang, "**It was handling:**\n", "**Estaba atendiendo:**\n"));
            for subject in self.subjects.iter().take(SUBJECTS_SHOWN) {
                text.push_str(&format!("- {subject}\n"));
            }
            if self.subjects.len() > SUBJECTS_SHOWN {
                text.push_str(&tf!(lang, "- and {count} more\n", "- y {count} más\n", count = self.subjects.len() - SUBJECTS_SHOWN));
            }
            text.push('\n');
        }
        let mut done: Vec<&str> = vec![];
        for call in &self.calls {
            let phrase = call_text(call, lang);
            if !done.contains(&phrase) {
                done.push(phrase);
            }
        }
        if !done.is_empty() {
            text.push_str(&tf!(lang, "**It had already done:** {done}.\n\n", "**Ya había hecho:** {done}.\n\n", done = done.join(", ")));
        }
        text.push_str(t!(lang,
            "**How to choose:** if the fleet already shows those tasks or questions, continue without running it again. If you see none of it, run it again from the start.",
            "**Cómo elegir:** si en la flota ya ves esas tareas o preguntas, sigue sin repetirlo. Si no ves nada de eso, repítelo desde el principio."));
        let error = self.error.trim();
        if !error.is_empty() {
            let shown: String = error.chars().take(ERROR_SHOWN).collect();
            text.push_str(t!(lang, "\n\nTechnical error:\n", "\n\nError técnico:\n"));
            text.push_str(&format!("```\n{shown}\n```"));
        }
        text
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn problem(calls: &[&str]) -> BatchProblem {
        BatchProblem {
            stop: BatchStop::Interrupted,
            calls: calls.iter().map(|call| (*call).to_owned()).collect(),
            subjects: vec!["Make the editor readable".into()],
            error: "Claude exited without a successful terminal result".into(),
        }
    }

    #[test]
    fn the_card_says_in_spanish_what_happened_what_it_did_and_how_to_choose() {
        let card = problem(&["create_task", "ask", "create_task"]);
        let (need, why) = card.text(Lang::Es);
        assert_eq!(need, "El orquestador se cortó a medias: decide si lo repite");
        assert!(why.starts_with("Se detuvo antes de terminar, cuando ya había hecho cambios"), "{why}");
        let detail = card.detail(Lang::Es);
        assert!(detail.contains("**Estaba atendiendo:**\n- Make the editor readable\n"), "{detail}");
        assert!(detail.contains("**Ya había hecho:** crear una tarea, preguntarte algo.\n"), "{detail}");
        assert!(detail.contains("**Cómo elegir:**") && detail.contains("Error técnico:\n```\nClaude exited"), "{detail}");
        assert!(need.chars().count() <= crate::domain::CardSpec::NEED_MAX && why.chars().count() <= crate::domain::CardSpec::WHY_MAX);
        assert_eq!(BatchProblem::from_body(&card.to_body()), Some(card));
    }

    #[test]
    fn without_a_call_seen_the_card_only_says_it_may_have_changed_things() {
        let (_, why) = problem(&[]).text(Lang::Es);
        assert!(why.contains("puede que ya hubiera hecho cambios"), "{why}");
        assert!(!problem(&[]).detail(Lang::En).contains("It had already done"));
    }

    #[test]
    fn every_text_fits_a_card() {
        for stop in [BatchStop::Interrupted, BatchStop::Denied, BatchStop::Cancelled] {
            for calls in [vec![], vec!["ask".to_owned()]] {
                for lang in [Lang::En, Lang::Es] {
                    let (need, why) = BatchProblem { stop, calls: calls.clone(), subjects: vec![], error: String::new() }.text(lang);
                    assert!(need.chars().count() <= crate::domain::CardSpec::NEED_MAX, "{need}");
                    assert!(why.chars().count() <= crate::domain::CardSpec::WHY_MAX, "{why}");
                }
            }
        }
    }

    #[test]
    fn reading_tools_change_nothing() {
        assert!(!changes_something("fleet") && !changes_something("node") && !changes_something("machine"));
        assert!(changes_something("create_task") && changes_something("ask"));
    }
}
