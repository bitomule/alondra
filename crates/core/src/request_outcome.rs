//! Alondra's card that tells the user how a request of theirs ended once all its tasks are over.
//! Its body holds the facts, so every client words it in its own language.

use serde::{Deserialize, Serialize};

use crate::domain::{CardSpec, clip};
use crate::views::Lang;
use crate::views::text::{t, tf};

pub const DONE_PREFIX: &str = "request-done-";

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct DoneTask {
    pub id: String,
    pub title: String,
    #[serde(default)]
    pub cancelled: bool,
    #[serde(default)]
    pub pr: Option<u64>,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct DoneCard {
    pub request_id: String,
    /// What came of it, as the orchestrator wrote it when it closed the request.
    #[serde(default)]
    pub summary: Option<String>,
    pub tasks: Vec<DoneTask>,
}

impl DoneCard {
    pub fn id(request_id: &str) -> String {
        format!("{DONE_PREFIX}{request_id}")
    }

    pub fn to_body(&self) -> String {
        serde_json::to_string(self).unwrap_or_default()
    }

    pub fn from_body(body: &str) -> Option<Self> {
        serde_json::from_str(body).ok()
    }

    fn cancelled(&self) -> usize {
        self.tasks.iter().filter(|task| task.cancelled).count()
    }

    pub fn text(&self, lang: Lang) -> (String, String) {
        let total = self.tasks.len();
        let cancelled = self.cancelled();
        let need = match (cancelled, total) {
            (0, _) => t!(lang, "Done", "Hecho").to_owned(),
            (1, 1) => t!(lang, "Not done: its task was cancelled", "No se hizo: se canceló su tarea").to_owned(),
            (c, n) if c == n => t!(lang, "Not done: its tasks were cancelled", "No se hizo: se cancelaron sus tareas").to_owned(),
            (1, n) => tf!(lang, "Done in part: 1 of {n} tasks was cancelled", "Hecho en parte: se canceló 1 de {n} tareas", n = n),
            (c, n) => tf!(lang, "Done in part: {c} of {n} tasks were cancelled", "Hecho en parte: se cancelaron {c} de {n} tareas", c = c, n = n),
        };
        let why = match self.summary.as_deref().map(str::trim).filter(|summary| !summary.is_empty()) {
            Some(summary) => clip(summary, CardSpec::WHY_MAX),
            None if cancelled == total => t!(lang, "Nothing of it was finished.", "No se terminó nada de ella.").into(),
            None => t!(lang, "All its work is finished.", "Todo su trabajo está terminado.").into(),
        };
        (need, why)
    }

    /// The whole summary when the card's line had to cut it.
    pub fn full_summary(&self) -> Option<&str> {
        self.summary.as_deref().map(str::trim).filter(|summary| summary.chars().count() > CardSpec::WHY_MAX)
    }

    pub fn task_line(task: &DoneTask, lang: Lang) -> (String, String) {
        let label = if task.cancelled { t!(lang, "Cancelled", "Cancelada") } else { t!(lang, "Done", "Hecha") };
        let value = match task.pr {
            Some(number) => format!("{} · PR #{number}", task.title),
            None => task.title.clone(),
        };
        (label.into(), value)
    }
}
