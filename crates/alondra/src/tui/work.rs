//! The options row of "New": the project and every option alondrad serves, drawn the same way
//! whether it is built in or declared by the user in `options.toml`.

use std::collections::BTreeMap;

use alondra_core::options::{Choice, EFFORT, PR, Spec};
use crossterm::event::{KeyCode, KeyEvent};
use serde_json::{Value, json};

#[derive(Default)]
pub struct OptionsForm {
    pub projects: Vec<String>,
    pub project: Option<String>,
    pub specs: Vec<Spec>,
    /// Only what the user changed; alondrad fills in the rest from the defaults.
    pub chosen: BTreeMap<String, Choice>,
    /// The option under the cursor, or `None` while typing the text.
    pub focus: Option<usize>,
    pub error: Option<String>,
    pub loaded: bool,
}

pub enum FormKey {
    Handled,
    /// The project changed: fetch its options.
    Reload(Option<String>),
    NotMine,
}

impl OptionsForm {
    pub fn current(&self, spec: &Spec) -> Choice {
        self.chosen.get(&spec.id).cloned().unwrap_or_else(|| spec.default.clone())
    }

    pub fn loaded(&mut self, project: Option<String>, projects: Vec<String>, specs: Vec<Spec>, error: Option<String>) {
        if project != self.project {
            return;
        }
        self.chosen.retain(|id, c| specs.iter().any(|s| &s.id == id && s.accepts(c).is_ok()));
        self.projects = projects;
        self.specs = specs;
        self.error = error;
        self.loaded = true;
        if let Some(f) = self.focus {
            self.focus = Some(f.min(self.specs.len().saturating_sub(1)));
        }
    }

    pub fn key(&mut self, k: KeyEvent) -> FormKey {
        let Some(at) = self.focus else {
            if k.code == KeyCode::Tab && self.loaded && !self.specs.is_empty() {
                self.focus = Some(0);
                return FormKey::Handled;
            }
            return FormKey::NotMine;
        };
        match k.code {
            KeyCode::Tab | KeyCode::Esc | KeyCode::BackTab => self.focus = None,
            KeyCode::Left | KeyCode::Char('h') => self.focus = Some(at.saturating_sub(1)),
            KeyCode::Right | KeyCode::Char('l') => self.focus = Some((at + 1).min(self.specs.len().saturating_sub(1))),
            KeyCode::Char(' ') => {
                if let Some(spec) = self.specs.get(at) {
                    let next = spec.next(&self.current(spec));
                    self.chosen.insert(spec.id.clone(), next);
                }
            }
            KeyCode::Char('p') => {
                let i = self.project.as_ref().and_then(|p| self.projects.iter().position(|x| x == p));
                self.project = match i {
                    None => self.projects.first().cloned(),
                    Some(i) => self.projects.get(i + 1).cloned(),
                };
                return FormKey::Reload(self.project.clone());
            }
            KeyCode::Enter => return FormKey::NotMine,
            _ => {}
        }
        FormKey::Handled
    }

    pub fn body(&self, text: &str, attachments: &[String]) -> Value {
        json!({"text": text, "attachments": attachments, "project": self.project, "options": self.chosen})
    }

    /// `(label, value, focused)` for each part of the row, the project first.
    pub fn parts(&self) -> Vec<(String, String, bool)> {
        let project = self.project.clone().unwrap_or_else(|| tr!("the orchestrator decides", "lo decide el orquestador").into());
        let mut out = vec![(tr!("Project", "Proyecto").to_string(), project, false)];
        out.extend(self.specs.iter().enumerate().map(|(i, s)| (label(s), value_label(s, &self.current(s)), self.focus == Some(i))));
        out
    }
}

pub fn label(s: &Spec) -> String {
    match (s.builtin, s.id.as_str()) {
        (true, PR) => tr!("PRs", "PRs").into(),
        (true, EFFORT) => tr!("Effort", "Esfuerzo").into(),
        _ => s.label.clone(),
    }
}

pub fn value_label(s: &Spec, c: &Choice) -> String {
    let builtin = |v: &str| match v {
        "review" => Some(tr!("review each one", "revisar cada una")),
        "auto_merge" => Some(tr!("auto-merge", "merge automático")),
        "light" => Some(tr!("light", "ligero")),
        "normal" => Some(tr!("normal", "normal")),
        "hard" => Some(tr!("hard", "a fondo")),
        _ => None,
    };
    match c {
        Choice::Toggle(true) => tr!("yes", "sí").into(),
        Choice::Toggle(false) => tr!("no", "no").into(),
        Choice::Value(v) => s.builtin.then(|| builtin(v)).flatten().map(str::to_string).unwrap_or_else(|| s.value_label(c)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::i18n;
    use alondra_core::options::OptionsFile;
    use crossterm::event::KeyModifiers;

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn form() -> OptionsForm {
        let file = OptionsFile::parse("[[option]]\nid = \"far\"\nlabel = \"Hasta dónde\"\nkind = \"choice\"\nvalues = [{ id = \"merge\", label = \"merge\" }, { id = \"beta\", label = \"beta\" }]\n[[option]]\nid = \"device\"\nlabel = \"Device\"\nkind = \"toggle\"\n").unwrap();
        let mut f = OptionsForm::default();
        f.loaded(None, vec!["A".into(), "B".into()], file.catalog(None), None);
        f
    }

    #[test]
    fn tab_moves_into_the_row_and_space_changes_only_what_it_touches() {
        i18n::set_for_this_thread(i18n::Lang::En);
        let mut f = form();
        assert!(matches!(f.key(key(KeyCode::Char(' '))), FormKey::NotMine), "while typing, space is text");
        for (code, focus) in [(KeyCode::Tab, Some(0)), (KeyCode::Right, Some(1)), (KeyCode::Char('l'), Some(2)), (KeyCode::Right, Some(3)), (KeyCode::Right, Some(3)), (KeyCode::Left, Some(2))] {
            f.key(key(code));
            assert_eq!(f.focus, focus, "{code:?}");
        }
        f.key(key(KeyCode::Char(' ')));
        f.key(key(KeyCode::Char(' ')));
        f.key(key(KeyCode::Left));
        f.key(key(KeyCode::Left));
        f.key(key(KeyCode::Char(' ')));
        assert_eq!(f.chosen, BTreeMap::from([("far".into(), Choice::Value("merge".into())), (PR.into(), Choice::Value("auto_merge".into()))]));
        assert_eq!(f.body("hola", &["a1".into()]), json!({"text": "hola", "attachments": ["a1"], "project": null, "options": {"far": "merge", "pr": "auto_merge"}}));
        f.key(key(KeyCode::Tab));
        assert_eq!(f.focus, None);
    }

    #[test]
    fn p_cycles_the_project_and_asks_for_its_options() {
        let mut f = form();
        f.focus = Some(0);
        let mut seen = vec![];
        for _ in 0..3 {
            match f.key(key(KeyCode::Char('p'))) {
                FormKey::Reload(p) => seen.push(p),
                _ => panic!("p reloads"),
            }
        }
        assert_eq!(seen, [Some("A".to_string()), Some("B".to_string()), None]);
        let before = f.specs.len();
        f.loaded(Some("A".into()), vec![], vec![], None);
        assert_eq!(f.specs.len(), before, "an answer for a project no longer chosen is dropped");
    }

    #[test]
    fn built_ins_speak_the_users_language_and_user_options_keep_their_own_words() {
        i18n::set_for_this_thread(i18n::Lang::Es);
        let f = form();
        let parts: Vec<String> = f.parts().into_iter().map(|(l, v, _)| format!("{l}: {v}")).collect();
        assert_eq!(parts, ["Proyecto: lo decide el orquestador", "PRs: revisar cada una", "Esfuerzo: normal", "Hasta dónde: merge", "Device: no"]);
    }

    #[test]
    fn a_new_catalog_drops_choices_that_no_longer_exist() {
        let mut f = form();
        f.chosen.insert("gone".into(), Choice::Toggle(true));
        f.chosen.insert("far".into(), Choice::Value("beta".into()));
        let specs = f.specs.clone();
        f.loaded(None, vec![], specs, Some("options.toml: broken".into()));
        assert_eq!(f.chosen.keys().collect::<Vec<_>>(), ["far"]);
        assert_eq!(f.error.as_deref(), Some("options.toml: broken"));
    }
}
