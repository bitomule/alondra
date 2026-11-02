//! Alondra's card that tells the user how a request of theirs ended once all its tasks are over.
//! Its body holds the facts, so every client words it in its own language.

use serde::{Deserialize, Serialize};

use crate::domain::{CardSpec, TaskPriority, clip};
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

pub const TASKS_PREFIX: &str = "request-tasks-";

/// A task the card waits on: one of the card when its id is there, an earlier one by its title.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct After {
    pub id: String,
    pub title: String,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct AnnouncedTask {
    pub id: String,
    pub title: String,
    #[serde(default, skip_serializing_if = "TaskPriority::is_normal")]
    pub priority: TaskPriority,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub after: Vec<After>,
}

/// Alondra's card that tells the user which tasks a request of theirs got, in the order they run.
/// The cards of a request are also the record of which of its tasks were announced.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct TasksCard {
    pub request_id: String,
    /// An earlier card of the request already named some of its tasks.
    #[serde(default)]
    pub more: bool,
    pub tasks: Vec<AnnouncedTask>,
}

impl TasksCard {
    pub fn id(request_id: &str, n: usize) -> String {
        format!("{TASKS_PREFIX}{request_id}-{n}")
    }

    pub fn to_body(&self) -> String {
        serde_json::to_string(self).unwrap_or_default()
    }

    pub fn from_body(body: &str) -> Option<Self> {
        serde_json::from_str(body).ok()
    }

    fn position(&self, id: &str) -> Option<usize> {
        self.tasks.iter().position(|task| task.id == id)
    }

    /// How deep each task sits in the order: 0 when nothing of this card goes before it.
    fn levels(&self) -> Vec<usize> {
        let mut levels: Vec<usize> = Vec::with_capacity(self.tasks.len());
        for task in &self.tasks {
            let level = task.after.iter()
                .filter_map(|after| self.position(&after.id).and_then(|i| levels.get(i)))
                .map(|level| level + 1)
                .max()
                .unwrap_or(0);
            levels.push(level);
        }
        levels
    }

    pub fn text(&self, lang: Lang) -> (String, String) {
        let need = match (self.tasks.as_slice(), self.more) {
            ([task], false) => tf!(lang, "I created the task “{title}”", "He creado la tarea «{title}»", title = task.title),
            ([task], true) => tf!(lang, "I created one more task: “{title}”", "He creado una tarea más: «{title}»", title = task.title),
            (tasks, false) => tf!(lang, "I created {n} tasks", "He creado {n} tareas", n = tasks.len()),
            (tasks, true) => tf!(lang, "I created {n} more tasks", "He creado {n} tareas más", n = tasks.len()),
        };
        let why = self.order(lang, false);
        let why = if why.chars().count() > CardSpec::WHY_MAX { self.order(lang, true) } else { why };
        (clip(&need, CardSpec::NEED_MAX), clip(&why, CardSpec::WHY_MAX))
    }

    /// The order the tasks run in, naming each by its title or, when that is too long, its number.
    fn order(&self, lang: Lang, by_number: bool) -> String {
        let name = |i: usize| match by_number {
            true => (i + 1).to_string(),
            false => match lang {
                Lang::En => format!("“{}”", self.tasks[i].title),
                Lang::Es => format!("«{}»", self.tasks[i].title),
            },
        };
        if let [task] = self.tasks.as_slice() {
            let earlier: Vec<String> = task.after.iter().map(|after| match lang {
                Lang::En => format!("“{}”", after.title),
                Lang::Es => format!("«{}»", after.title),
            }).collect();
            return match earlier.len() {
                0 => t!(lang, "It starts as soon as there is room.", "Empieza en cuanto haya hueco.").into(),
                1 => tf!(lang, "It starts once {earlier} is done.", "Empieza cuando termine {earlier}.", earlier = earlier[0]),
                _ => tf!(lang, "It starts once {earlier} are done.", "Empieza cuando terminen {earlier}.", earlier = list(lang, &earlier)),
            };
        }
        let levels = self.levels();
        let deepest = levels.iter().copied().max().unwrap_or(0);
        if deepest == 0 {
            return tf!(lang, "All {n} at once, in parallel.", "Las {n} a la vez, en paralelo.", n = self.tasks.len());
        }
        let steps: Vec<String> = (0..=deepest).map(|level| {
            let names: Vec<String> = (0..self.tasks.len()).filter(|&i| levels[i] == level).map(name).collect();
            let together = names.len() > 1;
            let names = list(lang, &names);
            match (level, together) {
                (0, false) => tf!(lang, "First {names}", "Primero {names}", names = names),
                (0, true) => tf!(lang, "First {names}, in parallel", "Primero {names}, en paralelo", names = names),
                (1, false) => tf!(lang, "then {names}", "{names} después", names = names),
                (1, true) => tf!(lang, "then {names}, in parallel", "{names} después, en paralelo", names = names),
                (_, false) => tf!(lang, "then {names}", "luego {names}", names = names),
                (_, true) => tf!(lang, "then {names}, in parallel", "luego {names}, en paralelo", names = names),
            }
        }).collect();
        format!("{}.", steps.join("; "))
    }

    /// The task's line in the card's list: its number, then its title, priority and what it waits on.
    pub fn task_line(&self, i: usize, lang: Lang) -> (String, String) {
        let task = &self.tasks[i];
        let mut value = task.title.clone();
        match task.priority {
            TaskPriority::High => value.push_str(t!(lang, " · High priority", " · Prioridad alta")),
            TaskPriority::Low => value.push_str(t!(lang, " · Low priority", " · Prioridad baja")),
            TaskPriority::Normal => {}
        }
        let after: Vec<String> = task.after.iter().map(|after| match (self.position(&after.id), lang) {
            (Some(position), _) => (position + 1).to_string(),
            (None, Lang::En) => format!("“{}”", after.title),
            (None, Lang::Es) => format!("«{}»", after.title),
        }).collect();
        if !after.is_empty() {
            value.push_str(&tf!(lang, " · after {after}", " · después de {after}", after = list(lang, &after)));
        }
        (format!("{}.", i + 1), value)
    }
}

/// «a, b and c» in the reader's language.
fn list(lang: Lang, names: &[String]) -> String {
    match names {
        [] => String::new(),
        [one] => one.clone(),
        [rest @ .., last] => format!("{} {} {last}", rest.join(", "), t!(lang, "and", "y")),
    }
}
