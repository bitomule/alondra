//! Alondra's card for the worktrees of cancelled tasks that hold changes git has nowhere else.
//! Its body holds the tasks and their files, so every client words it in its own language.

use serde::{Deserialize, Serialize};

use crate::domain::{CardSpec, QuestionOption, clip};
use crate::views::Lang;
use crate::views::text::{t, tf};

pub const DISCARD_WORKTREES_PREFIX: &str = "problem-discard-worktrees-";
pub const DISCARD: &str = "discard";
pub const DISCARD_LABEL: &str = "Delete";
pub const DISCARD_ALL_LABEL: &str = "Delete all";
pub const KEEP: &str = "keep";
pub const KEEP_LABEL: &str = "Keep";
pub const KEEP_ALL_LABEL: &str = "Keep all";
const TITLE_SHOWN: usize = 40;

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct CancelledWorktree {
    pub task_id: String,
    pub title: String,
    pub files: usize,
    pub paths: Vec<String>,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct DiscardWorktrees {
    pub tasks: Vec<CancelledWorktree>,
}

fn files_text(lang: Lang, files: usize) -> String {
    if files == 1 {
        t!(lang, "1 file", "1 fichero").into()
    } else {
        tf!(lang, "{files} files", "{files} ficheros", files = files)
    }
}

impl DiscardWorktrees {
    pub fn to_body(&self) -> String {
        serde_json::to_string(self).unwrap_or_default()
    }

    pub fn from_body(body: &str) -> Option<Self> {
        serde_json::from_str(body).ok()
    }

    fn several(&self) -> bool {
        self.tasks.len() > 1
    }

    pub fn options(&self) -> (Vec<QuestionOption>, &'static str) {
        let option = |id: &str, label: &str| QuestionOption { id: id.into(), label: label.into(), description: None };
        let (discard, keep) = if self.several() { (DISCARD_ALL_LABEL, KEEP_ALL_LABEL) } else { (DISCARD_LABEL, KEEP_LABEL) };
        (vec![option(DISCARD, discard), option(KEEP, keep)], KEEP)
    }

    /// What the card says it needs and why, in `lang`.
    pub fn text(&self, lang: Lang) -> (String, String) {
        let need = match self.tasks.as_slice() {
            [only] => tf!(lang,
                "Task “{title}” had unsaved changes in {files}: delete them?",
                "La tarea «{title}» tenía cambios sin guardar en {files}: ¿los borro?",
                title = clip(&only.title, TITLE_SHOWN), files = files_text(lang, only.files)),
            tasks => tf!(lang,
                "{count} cancelled tasks have unsaved changes: delete them?",
                "{count} tareas canceladas tienen cambios sin guardar: ¿los borro?",
                count = tasks.len()),
        };
        let why = t!(lang,
            "The tasks are cancelled. Their changes are in no commit and Alondra keeps no copy: once deleted they cannot be recovered.",
            "Las tareas están canceladas. Sus cambios no están en ningún commit y Alondra no guarda copia: al borrarlos no se pueden recuperar.");
        (clip(&need, CardSpec::NEED_MAX), why.into())
    }

    /// The card's detail: each task with its files, and what each option does.
    pub fn detail(&self, lang: Lang) -> String {
        let list: String = self.tasks.iter()
            .map(|task| format!("- **{}**: {}\n", task.title, files_text(lang, task.files)))
            .collect();
        let (discard, keep) = if self.several() {
            (t!(lang, "Delete all", "Borrar todas"), t!(lang, "Keep all", "Conservar todas"))
        } else {
            (t!(lang, "Delete", "Borrar"), t!(lang, "Keep", "Conservar"))
        };
        let (discards, keeps) = (
            t!(lang, "deletes the worktrees with those changes; there is no copy to recover them from.", "borra los worktrees con esos cambios; no hay copia para recuperarlos."),
            t!(lang, "touches nothing and Alondra does not ask again.", "no toca nada y Alondra no vuelve a preguntar."),
        );
        let heading = t!(lang, "**Unsaved changes:**", "**Cambios sin guardar:**");
        format!("{heading}\n\n{list}\n**{discard}:** {discards}\n\n**{keep}:** {keeps}")
    }
}

/// The words of this card's options in `lang`, keyed by the id and English label they were built with.
pub fn option_text(id: &str, label: &str, lang: Lang) -> Option<(&'static str, &'static str)> {
    let deletes = t!(lang, "Deletes the worktree and its unsaved changes for good", "Borra el worktree y sus cambios sin guardar para siempre");
    let keeps = t!(lang, "Leaves everything as it is and does not ask again", "Lo deja todo como está y no vuelve a preguntar");
    Some(match (id, label) {
        (DISCARD, DISCARD_LABEL) => (t!(lang, "Delete", "Borrar"), deletes),
        (DISCARD, DISCARD_ALL_LABEL) => (t!(lang, "Delete all", "Borrar todas"), deletes),
        (KEEP, KEEP_LABEL) => (t!(lang, "Keep", "Conservar"), keeps),
        (KEEP, KEEP_ALL_LABEL) => (t!(lang, "Keep all", "Conservar todas"), keeps),
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn task(id: &str, files: usize) -> CancelledWorktree {
        CancelledWorktree { task_id: id.into(), title: format!("Title of {id}"), files, paths: vec![format!("/repo/.claude/worktrees/{id}")] }
    }

    #[test]
    fn one_task_asks_in_spanish_whether_to_delete_its_unsaved_changes() {
        let card = DiscardWorktrees { tasks: vec![task("a", 3)] };
        let (need, why) = card.text(Lang::Es);
        assert_eq!(need, "La tarea «Title of a» tenía cambios sin guardar en 3 ficheros: ¿los borro?");
        assert!(why.contains("Alondra no guarda copia"), "{why}");
        let (options, recommended) = card.options();
        assert_eq!(options.iter().map(|o| (o.id.as_str(), o.label.as_str())).collect::<Vec<_>>(), [(DISCARD, DISCARD_LABEL), (KEEP, KEEP_LABEL)]);
        assert_eq!(recommended, KEEP, "deleting is never the suggested answer");
        assert_eq!(option_text(DISCARD, DISCARD_LABEL, Lang::Es).unwrap().0, "Borrar");
        assert_eq!(option_text(KEEP, KEEP_LABEL, Lang::Es).unwrap().0, "Conservar");
        assert!(DiscardWorktrees { tasks: vec![task("a", 1)] }.text(Lang::Es).0.contains("en 1 fichero:"));
        assert_eq!(DiscardWorktrees::from_body(&card.to_body()), Some(card));
    }

    #[test]
    fn several_tasks_are_one_card_with_a_list_and_buttons_for_all() {
        let card = DiscardWorktrees { tasks: vec![task("a", 3), task("b", 1)] };
        assert_eq!(card.text(Lang::Es).0, "2 tareas canceladas tienen cambios sin guardar: ¿los borro?");
        let detail = card.detail(Lang::Es);
        assert!(detail.contains("- **Title of a**: 3 ficheros") && detail.contains("- **Title of b**: 1 fichero"), "{detail}");
        let (options, _) = card.options();
        assert_eq!(options.iter().map(|o| o.label.as_str()).collect::<Vec<_>>(), [DISCARD_ALL_LABEL, KEEP_ALL_LABEL]);
        assert_eq!(option_text(DISCARD, DISCARD_ALL_LABEL, Lang::Es).unwrap().0, "Borrar todas");
        assert_eq!(option_text(KEEP, KEEP_ALL_LABEL, Lang::En).unwrap().0, "Keep all");
        assert_eq!(option_text(KEEP, "Something else", Lang::Es), None);
    }

    #[test]
    fn every_text_fits_a_card() {
        for tasks in [vec![task("a", 100)], vec![task("a", 1), task("b", 2)]] {
            for lang in [Lang::En, Lang::Es] {
                let mut card = DiscardWorktrees { tasks: tasks.clone() };
                card.tasks[0].title = "t".repeat(200);
                let (need, why) = card.text(lang);
                assert!(need.chars().count() <= CardSpec::NEED_MAX, "{need}");
                assert!(why.chars().count() <= CardSpec::WHY_MAX, "{why}");
            }
        }
    }
}
