//! The options of "New": one row per setting ("Label: value"), each opening a short list of
//! its values. The project, the issue, the built-ins and the user's own options from
//! `options.toml` all work the same way.

use std::collections::BTreeMap;

use alondra_core::issues::Issue;
use alondra_core::options::{AUTO, Choice, EFFORT, Kind, PR, Spec, filter_projects};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use serde_json::{Value, json};

const PROJECT_ROW: usize = 0;
const ISSUE_ROW: usize = 1;
const FIRST_OPTION_ROW: usize = 2;

#[derive(Default)]
pub struct OptionsForm {
    /// Projects as alondrad ranks them: most recently used first.
    pub projects: Vec<String>,
    /// The project of the directory the TUI runs in, offered first.
    pub suggested: Option<String>,
    pub project: Option<String>,
    pub specs: Vec<Spec>,
    /// Only what differs from the defaults; alondrad fills in the rest.
    pub chosen: BTreeMap<String, Choice>,
    /// The row under the cursor, or `None` while typing the text.
    pub focus: Option<usize>,
    pub error: Option<String>,
    pub loaded: bool,
    pub issue: Option<Issue>,
    pub popup: Option<Popup>,
}

pub enum Popup {
    Values { spec: usize, sel: usize },
    Projects { query: String, sel: usize },
    Issues(IssuePicker),
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

/// One entry of a values list: what it sets, its label and the line under it.
pub struct Entry {
    pub choice: Choice,
    pub label: String,
    pub description: Option<String>,
}

impl OptionsForm {
    pub fn current(&self, spec: &Spec) -> Choice {
        self.chosen.get(&spec.id).cloned().unwrap_or_else(|| spec.default.clone())
    }

    fn set(&mut self, spec: usize, choice: Choice) {
        let Some(s) = self.specs.get(spec) else { return };
        if choice == s.default {
            self.chosen.remove(&s.id);
        } else {
            self.chosen.insert(s.id.clone(), choice);
        }
    }

    fn rows(&self) -> usize {
        FIRST_OPTION_ROW + self.specs.len()
    }

    pub fn loaded(&mut self, project: Option<String>, projects: Vec<String>, specs: Vec<Spec>, error: Option<String>) {
        if project != self.project {
            return;
        }
        self.chosen.retain(|id, c| specs.iter().any(|s| &s.id == id && s.accepts(c).is_ok() && s.default != *c));
        self.projects = projects;
        self.specs = specs;
        self.error = error;
        self.loaded = true;
        if let Some(f) = self.focus {
            self.focus = Some(f.min(self.rows() - 1));
        }
    }

    pub fn issues_loaded(&mut self, result: Result<(Vec<Issue>, Option<String>), String>) {
        let Some(Popup::Issues(p)) = self.popup.as_mut() else { return };
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

    /// The values a popup lists for an option: its own, or yes and no for a toggle.
    pub fn entries(&self, spec: usize) -> Vec<Entry> {
        let Some(s) = self.specs.get(spec) else { return vec![] };
        match s.kind {
            Kind::Toggle => [true, false]
                .into_iter()
                .map(|b| Entry { choice: Choice::Toggle(b), label: value_label(s, &Choice::Toggle(b)), description: None })
                .collect(),
            Kind::Choice => s
                .values
                .iter()
                .map(|v| {
                    let choice = Choice::Value(v.id.clone());
                    Entry { label: value_label(s, &choice), description: description(s, &v.id).or(v.description.clone()), choice }
                })
                .collect(),
        }
    }

    /// What the project list shows for what was typed, with "the orchestrator decides" first
    /// while nothing is typed.
    pub fn project_choices(&self, query: &str) -> Vec<Option<String>> {
        let found = filter_projects(&self.projects, query, self.suggested.as_deref()).into_iter().map(Some);
        if query.trim().is_empty() { std::iter::once(None).chain(found).collect() } else { found.collect() }
    }

    fn open(&mut self, row: usize) -> FormKey {
        match row {
            PROJECT_ROW => self.popup = Some(Popup::Projects { query: String::new(), sel: 0 }),
            ISSUE_ROW => {
                self.popup = Some(Popup::Issues(IssuePicker { loading: true, ..Default::default() }));
                return FormKey::LoadIssues(false);
            }
            r => {
                let spec = r - FIRST_OPTION_ROW;
                let Some(s) = self.specs.get(spec) else { return FormKey::Handled };
                let current = self.current(s);
                let sel = self.entries(spec).iter().position(|e| e.choice == current).unwrap_or(0);
                self.popup = Some(Popup::Values { spec, sel });
            }
        }
        FormKey::Handled
    }

    fn popup_key(&mut self, k: KeyEvent) -> FormKey {
        let n = match &self.popup {
            Some(Popup::Values { spec, .. }) => self.entries(*spec).len(),
            _ => 0,
        };
        let Some(popup) = self.popup.as_mut() else { return FormKey::NotMine };
        if k.code == KeyCode::Esc {
            self.popup = None;
            return FormKey::Handled;
        }
        match popup {
            Popup::Values { spec, sel } => {
                let spec = *spec;
                match k.code {
                    KeyCode::Up | KeyCode::Char('k') => *sel = sel.saturating_sub(1),
                    KeyCode::Down | KeyCode::Char('j') => *sel = (*sel + 1).min(n.saturating_sub(1)),
                    KeyCode::Enter | KeyCode::Char(' ') => {
                        let pick = *sel;
                        self.popup = None;
                        if let Some(e) = self.entries(spec).into_iter().nth(pick) {
                            self.set(spec, e.choice);
                        }
                    }
                    _ => {}
                }
            }
            Popup::Projects { query, sel } => match k.code {
                KeyCode::Up => *sel = sel.saturating_sub(1),
                KeyCode::Down => *sel += 1,
                KeyCode::Backspace => {
                    query.pop();
                    *sel = 0;
                }
                KeyCode::Char(c) => {
                    query.push(c);
                    *sel = 0;
                }
                KeyCode::Enter => {
                    let (query, sel) = (query.clone(), *sel);
                    let choices = self.project_choices(&query);
                    self.popup = None;
                    let Some(project) = choices.get(sel.min(choices.len().saturating_sub(1))).cloned() else { return FormKey::Handled };
                    if project != self.project {
                        self.project = project;
                        return FormKey::Reload(self.project.clone());
                    }
                }
                _ => {}
            },
            Popup::Issues(p) => match k.code {
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
                    self.popup = None;
                    if let Some(project) = name.and_then(|n| self.projects.iter().find(|p| p.eq_ignore_ascii_case(&n)).cloned())
                        && self.project.as_ref() != Some(&project)
                    {
                        self.project = Some(project);
                        return FormKey::Reload(self.project.clone());
                    }
                }
                _ => {}
            },
        }
        FormKey::Handled
    }

    pub fn key(&mut self, k: KeyEvent) -> FormKey {
        if self.popup.is_some() {
            return self.popup_key(k);
        }
        let Some(at) = self.focus else {
            if k.code == KeyCode::Tab && self.loaded {
                self.focus = Some(PROJECT_ROW);
                return FormKey::Handled;
            }
            return FormKey::NotMine;
        };
        match k.code {
            KeyCode::Tab | KeyCode::Esc | KeyCode::BackTab => self.focus = None,
            KeyCode::Up | KeyCode::Char('k') => self.focus = Some(at.saturating_sub(1)),
            KeyCode::Down | KeyCode::Char('j') => self.focus = Some((at + 1).min(self.rows() - 1)),
            KeyCode::Enter | KeyCode::Char(' ') => return self.open(at),
            KeyCode::Char('p') => return self.open(PROJECT_ROW),
            KeyCode::Char('i') => return self.open(ISSUE_ROW),
            KeyCode::Char('x') => self.issue = None,
            _ => {}
        }
        FormKey::Handled
    }

    /// The body of a new request carrying the project, options and issue chosen here.
    pub fn request(&self, request_id: &str, text: &str, attachments: &[alondra_core::domain_v2::AttachmentRef]) -> Value {
        let issue = self.issue.as_ref().map(Issue::reference);
        crate::ops::new_request(request_id, text, self.project.as_deref(), &json!(self.chosen), issue.as_deref(), attachments)
    }

    /// `(label, value, focused)` for each row: project, issue, then every option.
    pub fn rows_shown(&self) -> Vec<(String, String, bool)> {
        let project = self.project.clone().unwrap_or_else(|| tr!("the orchestrator decides", "lo decide el orquestador").into());
        let issue = self.issue.as_ref().map(Issue::reference).unwrap_or_else(|| tr!("none", "ninguna").into());
        let mut out = vec![(tr!("Project", "Proyecto").to_string(), project), (tr!("Issue", "Issue").to_string(), issue)];
        out.extend(self.specs.iter().map(|s| (label(s), value_label(s, &self.current(s)))));
        out.into_iter().enumerate().map(|(i, (l, v))| (l, v, self.focus == Some(i))).collect()
    }
}

pub fn label(s: &Spec) -> String {
    match (s.builtin, s.id.as_str()) {
        (true, PR) => tr!("PRs", "PRs").into(),
        (true, EFFORT) => tr!("Effort", "Esfuerzo").into(),
        _ => s.label.clone(),
    }
}

/// How a task's PRs are handled, in the launch option's words.
pub fn pr_handling(auto_merge: bool) -> &'static str {
    if auto_merge { tr!("auto-merge", "merge automático") } else { tr!("review each one", "revisar cada una") }
}

pub fn value_label(s: &Spec, c: &Choice) -> String {
    let builtin = |v: &str| match v {
        "review" => Some(pr_handling(false)),
        "auto_merge" => Some(pr_handling(true)),
        AUTO => Some(tr!("auto", "automático")),
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

/// The built-ins' value descriptions in the user's language; the user's own come from alondrad.
fn description(s: &Spec, value: &str) -> Option<String> {
    if !s.builtin {
        return None;
    }
    let text = match value {
        "review" => tr!("Each PR is a card for you; the node waits for your merge", "Cada PR es una tarjeta para ti; el nodo espera a que hagas merge"),
        "auto_merge" => tr!("The node merges its own PR once its checks pass", "El nodo hace merge de su propio PR cuando pasan los checks"),
        AUTO => tr!("The orchestrator picks the model and effort from the task", "El orquestador elige modelo y esfuerzo según la tarea"),
        "light" => tr!("A quick, cheaper model for small, clear work", "Un modelo rápido y barato para trabajo pequeño y claro"),
        "normal" => tr!("Claude's default model and effort", "El modelo y el esfuerzo por defecto de Claude"),
        "hard" => tr!("The strongest model and the most effort", "El modelo más potente y el máximo esfuerzo"),
        _ => return None,
    };
    Some(text.into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::i18n;
    use alondra_core::options::OptionsFile;

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn form() -> OptionsForm {
        let file = OptionsFile::parse("[[option]]\nid = \"far\"\nlabel = \"Hasta dónde\"\nkind = \"choice\"\nvalues = [{ id = \"merge\", label = \"merge\" }, { id = \"beta\", label = \"beta\", description = \"Sube una beta\" }]\n[[option]]\nid = \"device\"\nlabel = \"Device\"\nkind = \"toggle\"\n").unwrap();
        let mut f = OptionsForm::default();
        f.loaded(None, vec!["Recent".into(), "Older".into(), "Here".into()], file.catalog(None), None);
        f.suggested = Some("Here".into());
        f
    }

    fn keys(f: &mut OptionsForm, codes: &[KeyCode]) {
        for c in codes {
            f.key(key(*c));
        }
    }

    #[test]
    fn each_option_is_a_row_whose_values_open_in_a_list() {
        i18n::set_for_this_thread(i18n::Lang::En);
        let mut f = form();
        assert!(matches!(f.key(key(KeyCode::Down)), FormKey::NotMine), "while typing, arrows are text");
        keys(&mut f, &[KeyCode::Tab, KeyCode::Down, KeyCode::Down, KeyCode::Down, KeyCode::Down]);
        assert_eq!(f.focus, Some(4), "rows: project, issue, PRs, effort, how far");
        f.key(key(KeyCode::Enter));
        let Some(Popup::Values { sel, .. }) = &f.popup else { panic!("Enter opens the values") };
        assert_eq!(*sel, 0, "the list opens on the current value");
        let labels: Vec<(String, Option<String>)> = f.entries(2).into_iter().map(|e| (e.label, e.description)).collect();
        assert_eq!(labels, [("merge".to_string(), None), ("beta".into(), Some("Sube una beta".into()))]);
        keys(&mut f, &[KeyCode::Down, KeyCode::Enter]);
        assert!(f.popup.is_none());
        assert_eq!(f.chosen, BTreeMap::from([("far".into(), Choice::Value("beta".into()))]));
        keys(&mut f, &[KeyCode::Enter, KeyCode::Up, KeyCode::Enter]);
        assert!(f.chosen.is_empty(), "back to the default sends nothing");
        keys(&mut f, &[KeyCode::Up, KeyCode::Enter, KeyCode::Down, KeyCode::Down, KeyCode::Enter]);
        assert_eq!(f.chosen, BTreeMap::from([(EFFORT.into(), Choice::Value("normal".into()))]));
        keys(&mut f, &[KeyCode::Down, KeyCode::Down, KeyCode::Enter, KeyCode::Esc]);
        assert!(f.popup.is_none() && f.focus == Some(5), "Esc closes only the list");
        let body = f.request("request-1", "hola", &[]);
        assert_eq!(body, json!({"request_id": "request-1", "text": "hola", "attachments": [], "project": null, "options": {"effort": "normal"}}));
    }

    #[test]
    fn the_project_list_filters_as_you_type_with_this_directorys_project_first() {
        let mut f = form();
        assert_eq!(f.project_choices(""), [None, Some("Here".into()), Some("Recent".into()), Some("Older".into())]);
        assert_eq!(f.project_choices("re"), [Some("Here".into()), Some("Recent".into())]);
        keys(&mut f, &[KeyCode::Tab, KeyCode::Enter, KeyCode::Char('o'), KeyCode::Char('l')]);
        match f.key(key(KeyCode::Enter)) {
            FormKey::Reload(p) => assert_eq!(p.as_deref(), Some("Older")),
            _ => panic!("picking a project loads its options"),
        }
        keys(&mut f, &[KeyCode::Char('p')]);
        match f.key(key(KeyCode::Enter)) {
            FormKey::Reload(p) => assert_eq!(p, None, "the first entry lets the orchestrator decide"),
            _ => panic!("a change reloads"),
        }
    }

    fn issue(repo: &str, number: u64, title: &str) -> Issue {
        Issue { repo: repo.into(), number, title: title.into(), url: format!("https://github.com/{repo}/issues/{number}"), author: None, association: None, labels: vec![], updated_at: None, comments: 0 }
    }

    #[test]
    fn the_issue_row_opens_the_issues_and_picking_one_picks_its_project() {
        let mut f = form();
        f.key(key(KeyCode::Tab));
        f.key(key(KeyCode::Down));
        assert!(matches!(f.key(key(KeyCode::Enter)), FormKey::LoadIssues(false)));
        f.issues_loaded(Ok((vec![issue("me/Recent", 1, "Crash on launch"), issue("me/B", 2, "Slow sync"), issue("x/Other", 3, "Crash in export")], None)));
        for c in "crash".chars() {
            f.key(key(KeyCode::Char(c)));
        }
        let Some(Popup::Issues(p)) = &f.popup else { panic!("the list is open") };
        assert_eq!(p.visible().iter().map(|i| i.reference()).collect::<Vec<_>>(), ["me/Recent#1", "x/Other#3"]);
        match f.key(key(KeyCode::Enter)) {
            FormKey::Reload(p) => assert_eq!(p.as_deref(), Some("Recent")),
            _ => panic!("the issue's repo names a known project"),
        }
        assert_eq!(f.request("r", "x", &[])["issue"], "me/Recent#1");
        assert!(matches!(f.key(key(KeyCode::Char('i'))), FormKey::LoadIssues(false)));
        assert!(matches!(f.key(KeyEvent::new(KeyCode::Char('r'), KeyModifiers::CONTROL)), FormKey::LoadIssues(true)));
        f.issues_loaded(Err("gh: not logged in".into()));
        f.key(key(KeyCode::Esc));
        f.key(key(KeyCode::Char('x')));
        assert!(f.request("r", "x", &[]).get("issue").is_none(), "x drops the issue");
    }

    #[test]
    fn rows_speak_the_users_language_and_user_options_keep_their_own_words() {
        i18n::set_for_this_thread(i18n::Lang::Es);
        let f = form();
        let rows: Vec<String> = f.rows_shown().into_iter().map(|(l, v, _)| format!("{l}: {v}")).collect();
        assert_eq!(rows, ["Proyecto: lo decide el orquestador", "Issue: ninguna", "PRs: revisar cada una", "Esfuerzo: automático", "Hasta dónde: merge", "Device: no"]);
        assert_eq!(f.entries(1)[0].description.as_deref(), Some("El orquestador elige modelo y esfuerzo según la tarea"));
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
        let before = f.specs.len();
        f.loaded(Some("A".into()), vec![], vec![], None);
        assert_eq!(f.specs.len(), before, "an answer for a project no longer chosen is dropped");
    }
}
