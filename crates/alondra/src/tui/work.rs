//! The options row of "New": the project and every option alondrad serves, drawn the same way
//! whether it is built in or declared by the user in `options.toml`.

use std::collections::BTreeMap;

use alondra_core::issues::Issue;
use alondra_core::options::{Choice, EFFORT, PR, Spec};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
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
    /// The issue the work comes from, which the node's PR closes.
    pub issue: Option<Issue>,
    pub picker: Option<IssuePicker>,
}

/// The open issues, filtered as the user types; the list comes once from alondrad's cache.
#[derive(Default)]
pub struct IssuePicker {
    pub all: Vec<Issue>,
    pub query: String,
    pub sel: usize,
    pub loading: bool,
    pub error: Option<String>,
}

impl IssuePicker {
    pub fn visible(&self) -> Vec<&Issue> {
        self.all.iter().filter(|i| i.matches(&self.query)).collect()
    }
}

pub enum FormKey {
    Handled,
    /// The project changed: fetch its options.
    Reload(Option<String>),
    /// Fetch the open issues; `true` asks alondrad to search GitHub again.
    LoadIssues(bool),
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

    pub fn issues_loaded(&mut self, result: Result<(Vec<Issue>, Option<String>), String>) {
        let Some(p) = self.picker.as_mut() else { return };
        p.loading = false;
        match result {
            Ok((all, error)) => {
                p.all = all;
                p.error = error;
                p.sel = p.sel.min(p.visible().len().saturating_sub(1));
            }
            Err(e) => p.error = Some(e),
        }
    }

    fn picker_key(&mut self, k: KeyEvent) -> FormKey {
        let Some(p) = self.picker.as_mut() else { return FormKey::NotMine };
        match k.code {
            KeyCode::Esc => self.picker = None,
            KeyCode::Char('r') if k.modifiers.contains(KeyModifiers::CONTROL) => {
                p.loading = true;
                return FormKey::LoadIssues(true);
            }
            KeyCode::Up => p.sel = p.sel.saturating_sub(1),
            KeyCode::Down => p.sel = (p.sel + 1).min(p.visible().len().saturating_sub(1)),
            KeyCode::Backspace => {
                p.query.pop();
                p.sel = 0;
            }
            KeyCode::Char(c) => {
                p.query.push(c);
                p.sel = 0;
            }
            KeyCode::Enter => {
                let Some(issue) = p.visible().get(p.sel).map(|i| (*i).clone()) else { return FormKey::Handled };
                let name = issue.repo.split_once('/').map(|(_, n)| n.to_string());
                self.issue = Some(issue);
                self.picker = None;
                if let Some(project) = name.and_then(|n| self.projects.iter().find(|p| p.eq_ignore_ascii_case(&n)).cloned())
                    && self.project.as_ref() != Some(&project)
                {
                    self.project = Some(project);
                    return FormKey::Reload(self.project.clone());
                }
            }
            _ => {}
        }
        FormKey::Handled
    }

    pub fn key(&mut self, k: KeyEvent) -> FormKey {
        if self.picker.is_some() {
            return self.picker_key(k);
        }
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
            KeyCode::Char('i') => {
                self.picker = Some(IssuePicker { loading: true, ..Default::default() });
                return FormKey::LoadIssues(false);
            }
            KeyCode::Char('x') => self.issue = None,
            KeyCode::Enter => return FormKey::NotMine,
            _ => {}
        }
        FormKey::Handled
    }

    pub fn body(&self, text: &str, attachments: &[String]) -> Value {
        let mut body = json!({"text": text, "attachments": attachments, "project": self.project, "options": self.chosen});
        if let Some(i) = &self.issue {
            body["issue"] = json!(i.reference());
        }
        body
    }

    /// `(label, value, focused)` for each part of the row, the project first.
    pub fn parts(&self) -> Vec<(String, String, bool)> {
        let project = self.project.clone().unwrap_or_else(|| tr!("the orchestrator decides", "lo decide el orquestador").into());
        let mut out = vec![(tr!("Project", "Proyecto").to_string(), project, false)];
        if let Some(i) = &self.issue {
            out.push((tr!("Issue", "Issue").to_string(), i.reference(), false));
        }
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

    fn issue(repo: &str, number: u64, title: &str) -> Issue {
        Issue { repo: repo.into(), number, title: title.into(), url: format!("https://github.com/{repo}/issues/{number}"), author: None, association: None, labels: vec![], updated_at: None, comments: 0 }
    }

    #[test]
    fn i_opens_the_issues_typing_filters_and_enter_picks_one_and_its_project() {
        let mut f = form();
        f.focus = Some(0);
        assert!(matches!(f.key(key(KeyCode::Char('i'))), FormKey::LoadIssues(false)));
        f.issues_loaded(Ok((vec![issue("me/A", 1, "Crash on launch"), issue("me/B", 2, "Slow sync"), issue("x/Other", 3, "Crash in export")], None)));
        for c in "crash".chars() {
            f.key(key(KeyCode::Char(c)));
        }
        let shown: Vec<String> = f.picker.as_ref().unwrap().visible().iter().map(|i| i.reference()).collect();
        assert_eq!(shown, ["me/A#1", "x/Other#3"]);
        f.key(key(KeyCode::Down));
        assert!(matches!(f.key(key(KeyCode::Enter)), FormKey::Handled), "an issue of no known project keeps the project");
        assert_eq!((f.issue.as_ref().map(Issue::reference), f.picker.is_none()), (Some("x/Other#3".into()), true));

        f.key(key(KeyCode::Char('i')));
        f.issues_loaded(Ok((vec![issue("me/A", 1, "Crash on launch")], None)));
        match f.key(key(KeyCode::Enter)) {
            FormKey::Reload(p) => assert_eq!(p.as_deref(), Some("A"), "the issue's repo names a known project"),
            _ => panic!("picking an issue of a known project loads its options"),
        }
        assert_eq!(f.body("x", &[])["issue"], "me/A#1");
        f.key(key(KeyCode::Char('x')));
        assert!(f.body("x", &[]).get("issue").is_none(), "x drops the issue");
    }

    #[test]
    fn ctrl_r_searches_again_and_esc_closes_the_list() {
        let mut f = form();
        f.focus = Some(0);
        f.key(key(KeyCode::Char('i')));
        assert!(matches!(f.key(KeyEvent::new(KeyCode::Char('r'), KeyModifiers::CONTROL)), FormKey::LoadIssues(true)));
        f.issues_loaded(Err("gh: not logged in".into()));
        assert_eq!(f.picker.as_ref().unwrap().error.as_deref(), Some("gh: not logged in"));
        f.key(key(KeyCode::Esc));
        assert!(f.picker.is_none() && f.focus == Some(0), "Esc closes only the list");
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
