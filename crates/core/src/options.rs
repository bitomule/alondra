//! The options a user picks when asking for new work: two built in (how PRs are handled and the
//! effort), and any number the user declares in `options.toml`, globally or per project.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::domain::{ModelChooser, TaskModel};

pub const FILE_NAME: &str = "options.toml";
pub const PR: &str = "pr";
pub const EFFORT: &str = "effort";
pub const REVIEW: &str = "review";
pub const AUTO_MERGE: &str = "auto_merge";
pub const AUTO: &str = "auto";
const MAPPED: [&str; 3] = ["light", "normal", "hard"];
const EFFORT_LEVELS: [&str; 5] = ["low", "medium", "high", "xhigh", "max"];
const MODEL_ALIASES: [&str; 4] = ["haiku", "sonnet", "opus", "fable"];

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
#[serde(untagged)]
pub enum Choice {
    Toggle(bool),
    Value(String),
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    Choice,
    Toggle,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Default)]
#[serde(deny_unknown_fields)]
pub struct EffortFlags {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effort: Option<String>,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct OptionValue {
    pub id: String,
    pub label: String,
    #[serde(default)]
    pub mission: String,
    /// One line the clients show under the value; the mission text when absent.
    #[serde(default)]
    pub description: Option<String>,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct UserOption {
    pub id: String,
    pub label: String,
    pub kind: Kind,
    #[serde(default)]
    pub default: Option<Choice>,
    #[serde(default)]
    pub values: Vec<OptionValue>,
    #[serde(default)]
    pub on: Option<String>,
    #[serde(default)]
    pub off: Option<String>,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Default)]
#[serde(default, deny_unknown_fields)]
pub struct ProjectOptions {
    pub defaults: BTreeMap<String, Choice>,
    pub option: Vec<UserOption>,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Default)]
#[serde(default, deny_unknown_fields)]
pub struct OptionsFile {
    pub projects_root: Option<String>,
    pub effort: BTreeMap<String, EffortFlags>,
    /// Extra model aliases the orchestrator may pick, and what `--model` receives for any alias
    /// (the built-in ones pass themselves).
    pub models: BTreeMap<String, String>,
    pub defaults: BTreeMap<String, Choice>,
    pub option: Vec<UserOption>,
    pub projects: BTreeMap<String, ProjectOptions>,
    pub issues: IssuesConfig,
    /// How many nodes may work at once on this Mac: `"auto"` (the default) derives it from its
    /// cores and memory.
    pub max_working_nodes: NodeLimit,
}

/// `max_working_nodes`: a number from 1 to `MAX_WORKING_NODES`, or `"auto"`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum NodeLimit {
    #[default]
    Auto,
    Fixed(u32),
}

pub const MAX_WORKING_NODES: u32 = 12;

impl NodeLimit {
    /// `min(ceil(cores / 2), floor((GB of RAM - 4) / 2))`, from 1 to `MAX_WORKING_NODES`: 4 GB
    /// for macOS and the user's apps, 2 GB for what a node runs.
    pub fn resolve(self, cores: usize, memory_bytes: u64) -> u32 {
        match self {
            NodeLimit::Fixed(count) => count.clamp(1, MAX_WORKING_NODES),
            NodeLimit::Auto => {
                let by_cores = (cores as u32).div_ceil(2);
                let gigabytes = memory_bytes / (1 << 30);
                let by_memory = (gigabytes.saturating_sub(4) / 2) as u32;
                by_cores.min(by_memory).clamp(1, MAX_WORKING_NODES)
            }
        }
    }
}

impl Serialize for NodeLimit {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self {
            NodeLimit::Auto => serializer.serialize_str(AUTO),
            NodeLimit::Fixed(count) => serializer.serialize_u32(*count),
        }
    }
}

impl<'de> Deserialize<'de> for NodeLimit {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Raw {
            Count(i64),
            Word(String),
        }
        match Raw::deserialize(deserializer)? {
            Raw::Word(word) if word == AUTO => Ok(NodeLimit::Auto),
            Raw::Count(count) if (1..=MAX_WORKING_NODES as i64).contains(&count) => Ok(NodeLimit::Fixed(count as u32)),
            _ => Err(serde::de::Error::custom(format!("max_working_nodes is \"auto\" or a number from 1 to {MAX_WORKING_NODES}"))),
        }
    }
}

/// Where "New" looks for open issues. With no owners, the owners of the GitHub checkouts
/// under the projects root.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Default)]
#[serde(default, deny_unknown_fields)]
pub struct IssuesConfig {
    pub owners: Vec<String>,
    /// `owner/repo` whose issues are never offered or passed on.
    pub exclude: Vec<String>,
}

/// One option as a client draws it: every value with its label, and the default already merged.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Spec {
    pub id: String,
    pub label: String,
    pub kind: Kind,
    #[serde(default)]
    pub values: Vec<SpecValue>,
    pub default: Choice,
    #[serde(default)]
    pub builtin: bool,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct SpecValue {
    pub id: String,
    pub label: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

/// One option as chosen for a request, carrying the text it adds to the node's mission.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Picked {
    pub id: String,
    pub label: String,
    pub choice: Choice,
    pub value_label: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mission: Option<String>,
}

/// Everything a request fixed, resolved when it was sent so a later edit of the file cannot
/// change what the user asked for.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Resolved {
    #[serde(default)]
    pub project: Option<String>,
    pub auto_merge: bool,
    pub effort: String,
    #[serde(default)]
    pub flags: EffortFlags,
    #[serde(default)]
    pub picked: Vec<Picked>,
    /// `owner/repo#n` the work comes from, which the node's PR closes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub issue: Option<String>,
}

fn builtin_effort(level: &str) -> EffortFlags {
    let f = |m: &str, e: &str| EffortFlags { model: Some(m.into()), effort: Some(e.into()) };
    match level {
        "light" => f("sonnet", "medium"),
        "hard" => f("opus", "xhigh"),
        _ => EffortFlags::default(),
    }
}

fn builtins() -> [Spec; 2] {
    let v = |id: &str, label: &str, description: &str| SpecValue { id: id.into(), label: label.into(), description: Some(description.into()) };
    [
        Spec {
            id: PR.into(),
            label: "PR handling".into(),
            kind: Kind::Choice,
            values: vec![
                v(REVIEW, "review each PR", "Each PR the node opens is a card for you; Alondra merges it when you accept it"),
                v(AUTO_MERGE, "auto-merge", "Alondra merges the PR once its checks pass"),
            ],
            default: Choice::Value(REVIEW.into()),
            builtin: true,
        },
        Spec {
            id: EFFORT.into(),
            label: "Effort".into(),
            kind: Kind::Choice,
            values: vec![
                v(AUTO, "auto", "Alondra picks the model and effort from the task"),
                v("light", "light", "A quick, cheaper model for small, clear work"),
                v("normal", "normal", "Claude's default model and effort"),
                v("hard", "hard", "The strongest model and the most effort"),
            ],
            default: Choice::Value(AUTO.into()),
            builtin: true,
        },
    ]
}

impl UserOption {
    fn spec(&self) -> Spec {
        Spec {
            id: self.id.clone(),
            label: self.label.clone(),
            kind: self.kind,
            values: self
                .values
                .iter()
                .map(|v| SpecValue {
                    id: v.id.clone(),
                    label: v.label.clone(),
                    description: v.description.clone().or_else(|| Some(v.mission.trim().to_string()).filter(|m| !m.is_empty())),
                })
                .collect(),
            default: self.default.clone().unwrap_or_else(|| match self.kind {
                Kind::Toggle => Choice::Toggle(false),
                Kind::Choice => Choice::Value(self.values.first().map(|v| v.id.clone()).unwrap_or_default()),
            }),
            builtin: false,
        }
    }

    fn check(&self) -> Result<(), String> {
        let id = &self.id;
        if id.trim().is_empty() || self.label.trim().is_empty() {
            return Err("every option needs an id and a label".into());
        }
        match self.kind {
            Kind::Choice => {
                if self.values.len() < 2 {
                    return Err(format!("option {id}: a choice needs at least two values"));
                }
                if self.on.is_some() || self.off.is_some() {
                    return Err(format!("option {id}: on and off belong to a toggle; a choice uses values"));
                }
                let mut seen = BTreeSet::new();
                if let Some(dup) = self.values.iter().find(|v| !seen.insert(v.id.as_str())) {
                    return Err(format!("option {id}: value {} appears twice", dup.id));
                }
            }
            Kind::Toggle if !self.values.is_empty() => return Err(format!("option {id}: a toggle uses on and off, not values")),
            Kind::Toggle => {}
        }
        if let Some(d) = &self.default {
            self.spec().accepts(d).map_err(|e| format!("option {id}: default {e}"))?;
        }
        Ok(())
    }

    fn mission(&self, choice: &Choice) -> Option<String> {
        let text = match (self.kind, choice) {
            (Kind::Toggle, Choice::Toggle(true)) => self.on.clone(),
            (Kind::Toggle, Choice::Toggle(false)) => self.off.clone(),
            (Kind::Choice, Choice::Value(v)) => self.values.iter().find(|x| &x.id == v).map(|x| x.mission.clone()),
            _ => None,
        };
        text.map(|t| t.trim().to_string()).filter(|t| !t.is_empty())
    }
}

impl Spec {
    pub fn accepts(&self, c: &Choice) -> Result<(), String> {
        match (self.kind, c) {
            (Kind::Toggle, Choice::Toggle(_)) => Ok(()),
            (Kind::Choice, Choice::Value(v)) if self.values.iter().any(|x| &x.id == v) => Ok(()),
            (Kind::Choice, Choice::Value(v)) => Err(format!("{v} is not one of {}", self.values.iter().map(|x| x.id.as_str()).collect::<Vec<_>>().join(", "))),
            (Kind::Toggle, _) => Err("must be true or false".into()),
            (Kind::Choice, _) => Err("must be one of its values".into()),
        }
    }

    pub fn value_label(&self, c: &Choice) -> String {
        match c {
            Choice::Toggle(true) => "yes".into(),
            Choice::Toggle(false) => "no".into(),
            Choice::Value(v) => self.values.iter().find(|x| &x.id == v).map(|x| x.label.clone()).unwrap_or_else(|| v.clone()),
        }
    }

    /// The next value, wrapping, for a client that cycles through them.
    pub fn next(&self, c: &Choice) -> Choice {
        match c {
            Choice::Toggle(b) => Choice::Toggle(!b),
            Choice::Value(v) => {
                let i = self.values.iter().position(|x| &x.id == v).map_or(0, |i| (i + 1) % self.values.len().max(1));
                Choice::Value(self.values.get(i).map(|x| x.id.clone()).unwrap_or_default())
            }
        }
    }
}

/// `text` with `max_working_nodes` set to `limit`, every other line as it was: the key is
/// replaced where it is, or added above the first table, where top-level keys go.
pub fn with_node_limit(text: &str, limit: NodeLimit) -> Result<String, String> {
    let value = match limit {
        NodeLimit::Auto => format!("\"{AUTO}\""),
        NodeLimit::Fixed(count) => count.to_string(),
    };
    let line = format!("max_working_nodes = {value}");
    let mut lines: Vec<String> = text.lines().map(str::to_owned).collect();
    let first_table = lines.iter().position(|line| line.trim_start().starts_with('['));
    let top = &lines[..first_table.unwrap_or(lines.len())];
    let existing = top.iter().position(|line| {
        line.trim_start().strip_prefix("max_working_nodes").is_some_and(|rest| rest.trim_start().starts_with('='))
    });
    match (existing, first_table) {
        (Some(index), _) => lines[index] = line,
        (None, Some(index)) => {
            lines.insert(index, String::new());
            lines.insert(index, line);
        }
        (None, None) => lines.push(line),
    }
    let mut written = lines.join("\n");
    written.push('\n');
    let parsed = OptionsFile::parse(&written)?;
    if parsed.max_working_nodes != limit {
        return Err("max_working_nodes did not take the new value".into());
    }
    Ok(written)
}

impl OptionsFile {
    pub fn parse(text: &str) -> Result<Self, String> {
        let file: OptionsFile = toml::from_str(text).map_err(|e| e.to_string())?;
        file.check()?;
        Ok(file)
    }

    fn check(&self) -> Result<(), String> {
        for (level, flags) in &self.effort {
            if !MAPPED.contains(&level.as_str()) {
                return Err(format!("effort {level}: the levels with a mapping are light, normal and hard; auto leaves the choice to the orchestrator"));
            }
            if let Some(e) = flags.effort.as_deref().filter(|e| !EFFORT_LEVELS.contains(e)) {
                return Err(format!("effort {level}: {e} is not one of {}", EFFORT_LEVELS.join(", ")));
            }
        }
        for (alias, model) in &self.models {
            if alias.is_empty() || !alias.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-') {
                return Err(format!("models {alias}: an alias is lowercase letters, digits and dashes"));
            }
            if model.trim().is_empty() {
                return Err(format!("models {alias}: name the model it stands for"));
            }
        }
        let check_all = |options: &[&UserOption]| -> Result<(), String> {
            let mut seen: BTreeSet<&str> = [PR, EFFORT].into();
            for o in options {
                if !seen.insert(o.id.as_str()) {
                    return Err(format!("option {} is declared twice or reuses a built-in id", o.id));
                }
                o.check()?;
            }
            Ok(())
        };
        check_all(&self.option.iter().collect::<Vec<_>>())?;
        self.check_defaults(None, &self.defaults)?;
        for (name, p) in &self.projects {
            check_all(&self.option.iter().chain(&p.option).collect::<Vec<_>>()).map_err(|e| format!("project {name}: {e}"))?;
            self.check_defaults(Some(name), &p.defaults).map_err(|e| format!("project {name}: {e}"))?;
        }
        Ok(())
    }

    fn check_defaults(&self, project: Option<&str>, defaults: &BTreeMap<String, Choice>) -> Result<(), String> {
        let specs = self.specs_without_defaults(project);
        for (id, c) in defaults {
            let spec = specs.iter().find(|s| &s.id == id).ok_or_else(|| format!("default {id} names no option"))?;
            spec.accepts(c).map_err(|e| format!("default {id}: {e}"))?;
        }
        Ok(())
    }

    fn user_options<'a>(&'a self, project: Option<&str>) -> impl Iterator<Item = &'a UserOption> {
        let own = project.and_then(|p| self.projects.get(p)).map(|p| p.option.as_slice()).unwrap_or_default();
        self.option.iter().chain(own)
    }

    fn specs_without_defaults(&self, project: Option<&str>) -> Vec<Spec> {
        builtins().into_iter().chain(self.user_options(project).map(UserOption::spec)).collect()
    }

    /// What a client draws for this project: built-ins first, then the user's, in file order,
    /// each with its default (built-in or declared, then the file's, then the project's).
    pub fn catalog(&self, project: Option<&str>) -> Vec<Spec> {
        let project_defaults = project.and_then(|p| self.projects.get(p)).map(|p| &p.defaults);
        self.specs_without_defaults(project)
            .into_iter()
            .map(|mut s| {
                if let Some(d) = self.defaults.get(&s.id) {
                    s.default = d.clone();
                }
                if let Some(d) = project_defaults.and_then(|d| d.get(&s.id)) {
                    s.default = d.clone();
                }
                s
            })
            .collect()
    }

    pub fn model_aliases(&self) -> Vec<String> {
        let mut aliases: Vec<String> = MODEL_ALIASES.iter().map(|a| a.to_string()).collect();
        aliases.extend(self.models.keys().filter(|k| !MODEL_ALIASES.contains(&k.as_str())).cloned());
        aliases
    }

    /// What `--model` receives for an alias the orchestrator may pick; none for any other word.
    pub fn model_flag(&self, alias: &str) -> Option<String> {
        match self.models.get(alias) {
            Some(model) => Some(model.trim().to_string()),
            None => MODEL_ALIASES.contains(&alias).then(|| alias.to_string()),
        }
    }

    /// The family a node's reported model has to belong to for it to be the alias's model.
    pub fn model_family(&self, alias: &str) -> Option<String> {
        self.model_flag(alias).map(|model| crate::model_label::model_family(&model))
    }

    pub fn effort_flags(&self, level: &str) -> EffortFlags {
        let mut flags = builtin_effort(level);
        if let Some(own) = self.effort.get(level) {
            if own.model.is_some() {
                flags.model = own.model.clone();
            }
            if own.effort.is_some() {
                flags.effort = own.effort.clone();
            }
        }
        flags
    }

    /// The defaults with what the user chose on top; an unknown option or value is an error so
    /// a stale client cannot send something the node would never hear about.
    pub fn resolve(&self, project: Option<&str>, chosen: &BTreeMap<String, Choice>) -> Result<Resolved, String> {
        let catalog = self.catalog(project);
        for (id, c) in chosen {
            let spec = catalog.iter().find(|s| &s.id == id).ok_or_else(|| format!("unknown option {id}"))?;
            spec.accepts(c).map_err(|e| format!("option {id}: {e}"))?;
        }
        let pick = |s: &Spec| chosen.get(&s.id).cloned().unwrap_or_else(|| s.default.clone());
        let value = |id: &str| catalog.iter().find(|s| s.id == id).map(pick);
        let auto_merge = value(PR) == Some(Choice::Value(AUTO_MERGE.into()));
        let effort = match value(EFFORT) {
            Some(Choice::Value(e)) => e,
            _ => AUTO.into(),
        };
        let users: Vec<&UserOption> = self.user_options(project).collect();
        let picked = catalog
            .iter()
            .map(|s| {
                let choice = pick(s);
                let mission = users.iter().find(|o| o.id == s.id).and_then(|o| o.mission(&choice));
                Picked { id: s.id.clone(), label: s.label.clone(), value_label: s.value_label(&choice), choice, mission }
            })
            .collect();
        Ok(Resolved { project: project.map(str::to_string), auto_merge, flags: self.effort_flags(&effort), effort, picked, issue: None })
    }

    /// For a launch no user request stands behind: the defaults, but never auto-merge, since
    /// only the user may let a node merge without review.
    pub fn orchestrator_defaults(&self, project: Option<&str>, effort: Option<&str>) -> Resolved {
        let mut chosen = BTreeMap::from([(PR.to_string(), Choice::Value(REVIEW.into()))]);
        if let Some(e) = effort.filter(|e| MAPPED.contains(e)) {
            chosen.insert(EFFORT.into(), Choice::Value(e.into()));
        }
        self.resolve(project, &chosen).unwrap_or_else(|_| OptionsFile::default().resolve(None, &chosen).expect("built-ins resolve"))
    }

    pub fn projects_root(&self, home: &Path) -> PathBuf {
        match self.projects_root.as_deref() {
            Some(p) if p.starts_with("~/") => home.join(&p[2..]),
            Some(p) => PathBuf::from(p),
            None => home.join("Projects"),
        }
    }
}

/// The project a directory belongs to: the first folder under the projects root.
pub fn project_of(root: &Path, cwd: &Path) -> Option<String> {
    cwd.strip_prefix(root).ok()?.components().next().map(|c| c.as_os_str().to_string_lossy().to_string())
}

/// Every git checkout directly under the projects root, by name.
pub fn projects(root: &Path) -> Vec<(String, PathBuf)> {
    let mut out: Vec<(String, PathBuf)> = std::fs::read_dir(root)
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.join(".git").exists())
        .filter_map(|p| Some((p.file_name()?.to_string_lossy().to_string(), p)))
        .collect();
    out.sort_by_key(|(n, _)| n.to_lowercase());
    out
}

/// A project as the pickers list it: how recently and how often its nodes ran.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct ProjectEntry {
    pub name: String,
    pub path: PathBuf,
    #[serde(default)]
    pub last_used: Option<crate::Millis>,
    #[serde(default)]
    pub nodes: usize,
}

/// The checkouts, most recently used first, then the most used, then by name. `used` gives,
/// per project, when each of its nodes started.
pub fn ranked(projects: Vec<(String, PathBuf)>, used: &BTreeMap<String, Vec<crate::Millis>>) -> Vec<ProjectEntry> {
    let mut out: Vec<ProjectEntry> = projects
        .into_iter()
        .map(|(name, path)| {
            let starts = used.get(&name).map(Vec::as_slice).unwrap_or_default();
            ProjectEntry { last_used: starts.iter().copied().max(), nodes: starts.len(), name, path }
        })
        .collect();
    out.sort_by(|a, b| b.last_used.cmp(&a.last_used).then(b.nodes.cmp(&a.nodes)).then(a.name.to_lowercase().cmp(&b.name.to_lowercase())));
    out
}

/// What a project picker shows for what the user typed: every word in the name, in the ranked
/// order, with the suggested project (the one the client runs in) first.
pub fn filter_projects(ranked: &[String], query: &str, suggested: Option<&str>) -> Vec<String> {
    let q = query.to_lowercase();
    let mut out: Vec<String> = ranked.iter().filter(|n| q.split_whitespace().all(|w| n.to_lowercase().contains(w))).cloned().collect();
    if let Some(i) = suggested.and_then(|s| out.iter().position(|n| n == s)) {
        let s = out.remove(i);
        out.insert(0, s);
    }
    out
}

/// How a launched node is told to organise its work.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, Default, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Approach {
    #[default]
    Solo,
    Subagents,
    Workflow,
}

impl Approach {
    /// The `## How to work` section of a node's mission.
    pub fn how_to_work(self) -> Vec<String> {
        let mut lines = vec![
            "## How to work".to_string(),
            String::new(),
            "If this work has more than two steps, write them as tasks in Claude Code's task list (TaskCreate) before starting, mark each one in progress when you start it and completed when it is done (TaskUpdate), so the user can follow your progress.".into(),
        ];
        match self {
            Approach::Solo => {}
            Approach::Subagents => lines.push("Split independent parts across subagents (the Agent tool) and work on them in parallel. Give each subagent a precise brief: the files, the change and how to check it. Pass model sonnet only when the brief already names the files, the change and the expected tests, or the change is literal (copy, versions, renames); otherwise pass model opus. Review what each subagent returns before you rely on it.".into()),
            Approach::Workflow => lines.push("Use a workflow (the Workflow tool) to run this: its parts can run in parallel and each has a check. Name a model for each stage: sonnet only for stages whose files, change and expected tests are already written down, or whose change is literal (copy, versions, renames); opus for every other stage. Try it on a small slice first.".into()),
        }
        lines
    }
}

impl Resolved {
    /// One line for the orchestrator: what the user chose, and that Alondra writes the rules.
    pub fn summary(&self) -> String {
        let mut parts = vec![format!("project {}", self.project.as_deref().unwrap_or("not chosen (you decide)"))];
        parts.extend(self.picked.iter().map(|p| format!("{}: {}", p.label, p.value_label)));
        parts.join(" · ")
    }

    /// The section alondrad appends to a launched node's prompt, so no rule depends on the
    /// orchestrator copying it.
    pub fn mission_section(&self, request_id: Option<&str>, approach: Approach) -> String {
        let who = match request_id {
            Some(id) => format!("The user chose these for this work (request {id}):"),
            None => "No user request is behind this launch, so these are the defaults:".into(),
        };
        let mut lines = vec!["## Launch options".to_string(), String::new(), who];
        lines.push(if self.auto_merge {
            "- PR handling: auto-merge. Register the PR and deliver it with present_result as your final result: once its checks pass, Alondra merges it and tells you, or sends you what only you can fix (failed checks, conflicts). Do not merge it yourself or wait on its checks.".into()
        } else {
            "- PR handling: review each PR. Register the PR and deliver it with present_result as your final result: the user reviews it, and when they accept it Alondra merges it and tells you. Do not merge it yourself.".into()
        });
        for p in self.picked.iter().filter(|p| p.id != PR && p.id != EFFORT) {
            match &p.mission {
                Some(m) => lines.push(format!("- {}: {}. {m}", p.label, p.value_label)),
                None => lines.push(format!("- {}: {}.", p.label, p.value_label)),
            }
        }
        if let Some(issue) = &self.issue {
            lines.push(format!("- This work comes from issue {issue}: write `Fixes {issue}` in the body of the PR that solves it."));
        }
        lines.push(String::new());
        lines.extend(approach.how_to_work());
        lines.push(String::new());
        lines.push("When you reach this point, deliver with `present_result` and is_final true: Alondra closes this session when the task is done. If work remains after your PR merges (publish, install, deploy, verify), declare it with `post_merge_work` before that delivery: the task stays in progress after the merge until you say it ended.".into());
        lines.join("\n")
    }

    /// With effort auto the orchestrator picks it per launch; an effort the user chose is binding.
    pub fn with_orchestrator_effort(mut self, file: &OptionsFile, effort: Option<&str>) -> Resolved {
        if self.effort == AUTO
            && let Some(e) = effort.filter(|e| MAPPED.contains(e))
        {
            self.flags = file.effort_flags(e);
            self.effort = e.to_string();
        }
        self
    }

    /// With effort auto the orchestrator picks the model (and optionally the effort) per launch; an
    /// effort the user chose already sets the model and refuses the orchestrator's.
    pub fn with_orchestrator_model(mut self, file: &OptionsFile, model: Option<&str>, effort: Option<&str>) -> Result<Resolved, String> {
        if self.effort != AUTO {
            return match model {
                Some(m) => Err(format!("the user chose effort {} for this request, which sets the model: do not pass model ({m})", self.effort)),
                None => Ok(self),
            };
        }
        let aliases = file.model_aliases().join(", ");
        let alias = model.ok_or_else(|| format!("effort is auto for this request: pass model, one of {aliases}"))?;
        let flag = file.model_flag(alias).ok_or_else(|| format!("{alias} is not a model; use one of {aliases}"))?;
        if let Some(e) = effort.filter(|e| !EFFORT_LEVELS.contains(e)) {
            return Err(format!("{e} is not an effort; use one of {}", EFFORT_LEVELS.join(", ")));
        }
        self.flags = EffortFlags { model: Some(flag), effort: effort.map(str::to_string) };
        Ok(self)
    }

    /// The model the launch passes to Claude and who chose it; none when Claude's default stays.
    pub fn launch_model(&self) -> Option<TaskModel> {
        let model = self.flags.model.clone()?;
        let chosen_by = if self.effort == AUTO { ModelChooser::Orchestrator } else { ModelChooser::User };
        Some(TaskModel { model, chosen_by })
    }

    /// What the orchestrator is told about effort alongside the request.
    pub fn effort_note(&self, user: &str) -> String {
        if self.effort == AUTO {
            "Effort is auto: choose the model and effort per task after splitting. Opus whenever the task touches interface with interaction, a state machine, concurrency or races, session or security, includes a data migration, or its specification does not name the files, states and edge cases; sonnet only when the files, the changes and the expected tests are already written down, or the change is literal (copy, versions, renames); opus with effort medium for everything else; opus with effort high for architecture design, a hard bug or a migration with difficult invariants; fable only after an opus node failed at the task. When in doubt, use opus. Request size or importance does not raise every task.".into()
        } else {
            format!("Effort {} was chosen by {user} and is binding: do not pass model or effort.", self.effort)
        }
    }

    /// `claude` flags for the effort; a model the orchestrator named explicitly wins.
    pub fn launch_flags(&self, explicit_model: Option<&str>) -> Vec<String> {
        let mut out = vec![];
        if let Some(m) = explicit_model.or(self.flags.model.as_deref()) {
            out.extend(["--model".to_string(), m.to_string()]);
        }
        if let Some(e) = &self.flags.effort {
            out.extend(["--effort".to_string(), e.clone()]);
        }
        out
    }
}

pub fn path_in(home: &Path) -> PathBuf {
    home.join(FILE_NAME)
}

#[cfg(test)]
mod tests {
    use super::*;

    const EXAMPLE: &str = r#"
[effort]
hard = { model = "big", effort = "max" }

[defaults]
effort = "light"

[[option]]
id = "how_far"
label = "How far"
kind = "choice"
default = "merge"
values = [
  { id = "merge",   label = "merge",             mission = "" },
  { id = "install", label = "merge and install", mission = "After the merge, run the install target." },
]

[[option]]
id = "device"
label = "Try on device"
kind = "toggle"
on = "Ask the user to try it on a device."

[projects.App]
defaults = { how_far = "install", pr = "auto_merge" }
[[projects.App.option]]
id = "review_loop"
label = "Adversarial review"
kind = "toggle"
default = true
on = "Run /review-loop before the merge."
"#;

    fn file() -> OptionsFile {
        OptionsFile::parse(EXAMPLE).unwrap()
    }

    fn chosen(pairs: &[(&str, Choice)]) -> BTreeMap<String, Choice> {
        pairs.iter().map(|(k, v)| (k.to_string(), v.clone())).collect()
    }

    fn val(s: &str) -> Choice {
        Choice::Value(s.into())
    }

    #[test]
    fn an_absent_file_offers_only_the_generic_built_ins() {
        let ids: Vec<String> = OptionsFile::default().catalog(None).into_iter().map(|s| s.id).collect();
        assert_eq!(ids, [PR, EFFORT]);
        let r = OptionsFile::default().resolve(None, &BTreeMap::new()).unwrap();
        assert!(!r.auto_merge);
        assert_eq!(r.effort, AUTO);
        assert_eq!(r.launch_flags(None), Vec::<String>::new(), "auto passes no flags: the orchestrator decides");
    }

    #[test]
    fn broken_files_are_refused_with_the_reason() {
        let base = "[[option]]\nid = \"x\"\nlabel = \"X\"\n";
        for (what, text, needle) in [
            ("choice with one value", format!("{base}kind = \"choice\"\nvalues = [{{ id = \"a\", label = \"A\" }}]\n"), "at least two"),
            ("duplicate value", format!("{base}kind = \"choice\"\nvalues = [{{ id = \"a\", label = \"A\" }}, {{ id = \"a\", label = \"B\" }}]\n"), "twice"),
            ("default names no value", format!("{base}kind = \"choice\"\ndefault = \"z\"\nvalues = [{{ id = \"a\", label = \"A\" }}, {{ id = \"b\", label = \"B\" }}]\n"), "z is not one of"),
            ("toggle with values", format!("{base}kind = \"toggle\"\nvalues = [{{ id = \"a\", label = \"A\" }}]\n"), "toggle uses on and off"),
            ("toggle default not a bool", format!("{base}kind = \"toggle\"\ndefault = \"yes\"\n"), "true or false"),
            ("reuses a built-in id", "[[option]]\nid = \"pr\"\nlabel = \"P\"\nkind = \"toggle\"\n".into(), "built-in"),
            ("declared twice", format!("{base}kind = \"toggle\"\n{base}kind = \"toggle\"\n"), "twice"),
            ("default for no option", "[defaults]\nnope = true\n".into(), "names no option"),
            ("bad built-in default", "[defaults]\npr = \"yolo\"\n".into(), "yolo is not one of"),
            ("unknown effort level", "[effort]\nturbo = { model = \"m\" }\n".into(), "light, normal and hard"),
            ("unknown claude effort", "[effort]\nhard = { effort = \"extreme\" }\n".into(), "extreme"),
            ("project default for another project's option", "[projects.A]\ndefaults = { review_loop = true }\n".into(), "project A"),
            ("unknown key", "colour = \"red\"\n".into(), "unknown field"),
        ] {
            let e = OptionsFile::parse(&text).expect_err(what);
            assert!(e.contains(needle), "{what}: {e}");
        }
        assert!(OptionsFile::parse(EXAMPLE).is_ok());
    }

    #[test]
    fn defaults_merge_built_in_then_file_then_project_then_request() {
        let f = file();
        let default_of = |project: Option<&str>, id: &str| f.catalog(project).into_iter().find(|s| s.id == id).map(|s| s.default);
        for (project, id, expected) in [
            (None, PR, Some(val(REVIEW))),
            (None, EFFORT, Some(val("light"))),
            (None, "how_far", Some(val("merge"))),
            (None, "device", Some(Choice::Toggle(false))),
            (None, "review_loop", None),
            (Some("App"), PR, Some(val(AUTO_MERGE))),
            (Some("App"), "how_far", Some(val("install"))),
            (Some("App"), "review_loop", Some(Choice::Toggle(true))),
            (Some("Other"), "how_far", Some(val("merge"))),
        ] {
            assert_eq!(default_of(project, id), expected, "{project:?} {id}");
        }
        let r = f.resolve(Some("App"), &chosen(&[("how_far", val("merge")), (PR, val(REVIEW))])).unwrap();
        assert!(!r.auto_merge, "the request beats the project default");
        assert_eq!(r.picked.iter().find(|p| p.id == "how_far").unwrap().choice, val("merge"));
    }

    #[test]
    fn the_mission_says_how_to_work_for_each_approach() {
        let r = file().resolve(None, &chosen(&[])).unwrap();
        let section = |a| r.mission_section(None, a);
        let always = "write them as tasks in Claude Code's task list (TaskCreate)";
        for approach in [Approach::Solo, Approach::Subagents, Approach::Workflow] {
            let text = section(approach);
            assert!(text.contains("## How to work") && text.contains(always), "{approach:?}");
            assert!(text.find("## How to work") < text.find("`present_result`"), "{approach:?}");
        }
        assert!(!section(Approach::Solo).contains("subagents") && !section(Approach::Solo).contains("the Workflow tool"));
        assert!(section(Approach::Subagents).contains("Split independent parts across subagents (the Agent tool)"));
        assert!(section(Approach::Workflow).contains("Use a workflow (the Workflow tool)"));
        assert!(!section(Approach::Workflow).contains("subagents (the Agent tool)"));
    }

    #[test]
    fn a_request_naming_an_unknown_option_or_value_is_refused() {
        let f = file();
        for (what, c, needle) in [
            ("unknown option", chosen(&[("nope", Choice::Toggle(true))]), "unknown option nope"),
            ("project option elsewhere", chosen(&[("review_loop", Choice::Toggle(true))]), "unknown option review_loop"),
            ("bad value", chosen(&[("how_far", val("moon"))]), "moon"),
            ("bool for a choice", chosen(&[(EFFORT, Choice::Toggle(true))]), "one of its values"),
        ] {
            let e = f.resolve(None, &c).expect_err(what);
            assert!(e.contains(needle), "{what}: {e}");
        }
    }

    #[test]
    fn options_become_mission_lines_in_file_order() {
        let r = file().resolve(Some("App"), &chosen(&[("device", Choice::Toggle(true))])).unwrap();
        let s = r.mission_section(Some("r-7"), Approach::Solo);
        let expected = [
            "## Launch options",
            "",
            "The user chose these for this work (request r-7):",
            "- PR handling: auto-merge. Register the PR and deliver it with present_result as your final result: once its checks pass, Alondra merges it and tells you, or sends you what only you can fix (failed checks, conflicts). Do not merge it yourself or wait on its checks.",
            "- How far: merge and install. After the merge, run the install target.",
            "- Try on device: yes. Ask the user to try it on a device.",
            "- Adversarial review: yes. Run /review-loop before the merge.",
            "",
            "## How to work",
            "",
            "If this work has more than two steps, write them as tasks in Claude Code's task list (TaskCreate) before starting, mark each one in progress when you start it and completed when it is done (TaskUpdate), so the user can follow your progress.",
            "",
            "When you reach this point, deliver with `present_result` and is_final true: Alondra closes this session when the task is done. If work remains after your PR merges (publish, install, deploy, verify), declare it with `post_merge_work` before that delivery: the task stays in progress after the merge until you say it ended.",
        ];
        assert_eq!(s, expected.join("\n"));
        let off = file().resolve(None, &chosen(&[("how_far", val("merge"))])).unwrap().mission_section(None, Approach::Solo);
        assert!(off.contains("- PR handling: review each PR."), "{off}");
        assert!(off.contains("- How far: merge.\n"), "an empty mission text adds no sentence: {off}");
        assert!(off.contains("- Try on device: no.\n"), "a toggle off without text adds none: {off}");
        assert!(!off.contains("Effort"), "effort goes into flags, not the mission: {off}");
    }

    #[test]
    fn effort_maps_to_flags_and_an_explicit_model_wins() {
        let f = file();
        let flags = |effort: &str, model: Option<&str>| f.resolve(None, &chosen(&[(EFFORT, val(effort))])).unwrap().launch_flags(model);
        for (effort, model, expected) in [
            ("light", None, vec!["--model", "sonnet", "--effort", "medium"]),
            ("normal", None, vec![]),
            ("hard", None, vec!["--model", "big", "--effort", "max"]),
            ("hard", Some("haiku"), vec!["--model", "haiku", "--effort", "max"]),
            ("normal", Some("haiku"), vec!["--model", "haiku"]),
        ] {
            assert_eq!(flags(effort, model), expected, "{effort} {model:?}");
        }
    }

    #[test]
    fn projects_rank_by_recent_use_then_by_use_then_by_name() {
        let checkouts = ["alpha", "Beta", "gamma", "delta"].map(|n| (n.to_string(), PathBuf::from(format!("/p/{n}")))).to_vec();
        let used = BTreeMap::from([("gamma".to_string(), vec![100, 900]), ("delta".to_string(), vec![900]), ("alpha".to_string(), vec![50])]);
        let order: Vec<String> = ranked(checkouts, &used).into_iter().map(|p| p.name).collect();
        assert_eq!(order, ["gamma", "delta", "alpha", "Beta"], "same last use: more nodes first; never used: by name");
        for (query, suggested, shown) in [
            ("", None, vec!["gamma", "delta", "alpha", "Beta"]),
            ("", Some("Beta"), vec!["Beta", "gamma", "delta", "alpha"]),
            ("a", Some("Beta"), vec!["Beta", "gamma", "delta", "alpha"]),
            ("ta", None, vec!["delta", "Beta"]),
            ("BE", Some("gamma"), vec!["Beta"]),
            ("zz", Some("gamma"), vec![]),
        ] {
            assert_eq!(filter_projects(&order, query, suggested), shown, "{query:?} {suggested:?}");
        }
    }

    #[test]
    fn with_effort_auto_the_orchestrator_chooses_and_an_explicit_effort_binds_it() {
        let f = OptionsFile::parse("[effort]\nhard = { model = \"big\", effort = \"max\" }\n").unwrap();
        for (what, user, orchestrator, effort, flags) in [
            ("auto, orchestrator picks hard", None, Some("hard"), "hard", vec!["--model", "big", "--effort", "max"]),
            ("auto, orchestrator picks light", None, Some("light"), "light", vec!["--model", "sonnet", "--effort", "medium"]),
            ("auto, orchestrator says nothing", None, None, AUTO, vec![]),
            ("auto, orchestrator names no level", None, Some("auto"), AUTO, vec![]),
            ("user chose light", Some("light"), Some("hard"), "light", vec!["--model", "sonnet", "--effort", "medium"]),
            ("user chose normal", Some("normal"), Some("hard"), "normal", vec![]),
        ] {
            let chosen = user.map(|u| chosen(&[(EFFORT, val(u))])).unwrap_or_default();
            let r = f.resolve(None, &chosen).unwrap().with_orchestrator_effort(&f, orchestrator);
            assert_eq!((r.effort.as_str(), r.launch_flags(None)), (effort, flags.iter().map(|s| s.to_string()).collect::<Vec<_>>()), "{what}");
        }
        assert!(f.resolve(None, &BTreeMap::new()).unwrap().effort_note("Ana").starts_with("Effort is auto"));
        assert_eq!(f.resolve(None, &chosen(&[(EFFORT, val("hard"))])).unwrap().effort_note("Ana"), "Effort hard was chosen by Ana and is binding: do not pass model or effort.");
        assert!(OptionsFile::parse("[effort]\nauto = { model = \"m\" }\n").unwrap_err().contains("auto leaves the choice"));
    }

    #[test]
    fn with_effort_auto_the_orchestrator_names_the_model_and_a_users_effort_refuses_it() {
        let f = OptionsFile::parse("[models]\nbig = \"claude-opus-5-5[1m]\"\n[effort]\nhard = { model = \"big\", effort = \"max\" }\n").unwrap();
        let auto = || f.resolve(None, &BTreeMap::new()).unwrap();
        let flags = |r: Resolved| r.launch_flags(None).join(" ");

        assert_eq!(flags(auto().with_orchestrator_model(&f, Some("sonnet"), None).unwrap()), "--model sonnet");
        assert_eq!(flags(auto().with_orchestrator_model(&f, Some("haiku"), Some("low")).unwrap()), "--model haiku --effort low");
        assert_eq!(flags(auto().with_orchestrator_model(&f, Some("big"), Some("xhigh")).unwrap()), "--model claude-opus-5-5[1m] --effort xhigh");

        let missing = auto().with_orchestrator_model(&f, None, Some("high")).unwrap_err();
        assert!(missing.contains("effort is auto") && missing.contains("pass model") && missing.contains("haiku, sonnet, opus, fable, big"), "{missing}");
        assert!(auto().with_orchestrator_model(&f, Some("gpt"), None).unwrap_err().contains("gpt is not a model"));
        assert!(auto().with_orchestrator_model(&f, Some("sonnet"), Some("light")).unwrap_err().contains("light is not an effort"));

        let chosen = f.resolve(None, &chosen(&[(EFFORT, val("hard"))])).unwrap();
        let refused = chosen.clone().with_orchestrator_model(&f, Some("haiku"), None).unwrap_err();
        assert!(refused.contains("the user chose effort hard") && refused.contains("haiku"), "{refused}");
        assert_eq!(flags(chosen.with_orchestrator_model(&f, None, Some("low")).unwrap()), "--model big --effort max", "the user's mapping applies as before");
    }

    #[test]
    fn the_launch_model_says_who_chose_it() {
        let f = OptionsFile::default();
        let by_orchestrator = f.resolve(None, &BTreeMap::new()).unwrap().with_orchestrator_model(&f, Some("opus"), None).unwrap();
        assert_eq!(by_orchestrator.launch_model(), Some(TaskModel { model: "opus".into(), chosen_by: ModelChooser::Orchestrator }));
        let by_user = f.resolve(None, &chosen(&[(EFFORT, val("light"))])).unwrap();
        assert_eq!(by_user.launch_model(), Some(TaskModel { model: "sonnet".into(), chosen_by: ModelChooser::User }));
        assert_eq!(f.resolve(None, &chosen(&[(EFFORT, val("normal"))])).unwrap().launch_model(), None, "Claude's default model is nothing to record");
    }

    #[test]
    fn model_aliases_come_from_the_file_and_fit_by_family() {
        let f = OptionsFile::parse("[models]\nbig = \"claude-opus-5-5\"\nfable = \"claude-fable-5-1\"\n").unwrap();
        assert_eq!(f.model_aliases(), ["haiku", "sonnet", "opus", "fable", "big"]);
        assert_eq!((f.model_flag("fable").as_deref(), f.model_flag("opus").as_deref(), f.model_flag("nope")), (Some("claude-fable-5-1"), Some("opus"), None));
        assert_eq!((f.model_family("big").as_deref(), f.model_family("sonnet").as_deref()), (Some("opus"), Some("sonnet")));
        assert!(OptionsFile::parse("[models]\n\"Big Model\" = \"x\"\n").unwrap_err().contains("lowercase"));
        assert!(OptionsFile::parse("[models]\nbig = \" \"\n").unwrap_err().contains("name the model"));
    }

    #[test]
    fn a_launch_with_no_user_request_never_auto_merges() {
        let f = file();
        assert!(f.resolve(Some("App"), &BTreeMap::new()).unwrap().auto_merge, "the user's own request takes the project default");
        let r = f.orchestrator_defaults(Some("App"), Some("hard"));
        assert!(!r.auto_merge);
        assert_eq!(r.effort, "hard");
        assert_eq!(f.orchestrator_defaults(None, Some("warp")).effort, "light", "an unknown effort falls back to the default");
    }

    #[test]
    fn next_cycles_values_and_flips_toggles() {
        let spec = &file().catalog(None)[2];
        assert_eq!(spec.next(&val("merge")), val("install"));
        assert_eq!(spec.next(&val("install")), val("merge"));
        assert_eq!(spec.next(&Choice::Toggle(true)), Choice::Toggle(false));
    }

    #[test]
    fn projects_are_the_first_folder_under_the_root() {
        let scratch_directory = crate::test_support::Directory::new();
        let root = Path::new("/u/Projects");
        assert_eq!(project_of(root, Path::new("/u/Projects/App/.claude/worktrees/x")).as_deref(), Some("App"));
        assert_eq!(project_of(root, Path::new("/elsewhere/App")), None);
        let f = OptionsFile::parse("projects_root = \"~/code\"").unwrap();
        assert_eq!(f.projects_root(Path::new("/u")), PathBuf::from("/u/code"));
        assert_eq!(OptionsFile::default().projects_root(Path::new("/u")), PathBuf::from("/u/Projects"));

        let dir = scratch_directory.parent().unwrap().join(format!("alondra-projects-{}-{}", std::process::id(), crate::now_millis()));
        std::fs::create_dir_all(dir.join("b-repo/.git")).unwrap();
        std::fs::create_dir_all(dir.join("A-repo")).unwrap();
        std::fs::write(dir.join("A-repo/.git"), "gitdir: x").unwrap();
        std::fs::create_dir_all(dir.join("not-a-repo")).unwrap();
        let names: Vec<String> = projects(&dir).into_iter().map(|(n, _)| n).collect();
        assert_eq!(names, ["A-repo", "b-repo"]);
        let _ = std::fs::remove_dir_all(dir);
    }
}

#[cfg(test)]
mod node_limit_tests {
    use super::*;

    const GB: u64 = 1 << 30;

    #[test]
    fn auto_takes_the_smaller_of_half_the_cores_and_2_gb_per_node_over_4_gb() {
        assert_eq!(NodeLimit::Auto.resolve(11, 18 * GB), 6);
        assert_eq!(NodeLimit::Auto.resolve(16, 16 * GB), 6);
        assert_eq!(NodeLimit::Auto.resolve(2, 8 * GB), 1);
        assert_eq!(NodeLimit::Auto.resolve(4, 4 * GB), 1);
        assert_eq!(NodeLimit::Auto.resolve(64, 512 * GB), 12);
    }

    #[test]
    fn the_file_takes_auto_or_a_number_from_1_to_12() {
        assert_eq!(OptionsFile::parse("").unwrap().max_working_nodes, NodeLimit::Auto);
        assert_eq!(OptionsFile::parse("max_working_nodes = \"auto\"").unwrap().max_working_nodes, NodeLimit::Auto);
        assert_eq!(OptionsFile::parse("max_working_nodes = 3").unwrap().max_working_nodes, NodeLimit::Fixed(3));
        for wrong in ["max_working_nodes = 0", "max_working_nodes = 13", "max_working_nodes = \"many\""] {
            assert!(OptionsFile::parse(wrong).unwrap_err().contains("max_working_nodes"), "{wrong}");
        }
        assert_eq!(NodeLimit::Fixed(3).resolve(64, 512 * GB), 3);
    }

    #[test]
    fn setting_the_node_limit_keeps_every_other_line() {
        let text = "# my options\n[defaults]\neffort = \"light\" # keep\n";
        let fixed = with_node_limit(text, NodeLimit::Fixed(4)).unwrap();
        assert_eq!(fixed, "# my options\nmax_working_nodes = 4\n\n[defaults]\neffort = \"light\" # keep\n");
        let auto = with_node_limit(&fixed, NodeLimit::Auto).unwrap();
        assert_eq!(auto, "# my options\nmax_working_nodes = \"auto\"\n\n[defaults]\neffort = \"light\" # keep\n");
        assert!(OptionsFile::parse(&auto).unwrap().defaults.contains_key("effort"));
        assert_eq!(with_node_limit("", NodeLimit::Fixed(12)).unwrap(), "max_working_nodes = 12\n");
        assert!(with_node_limit("", NodeLimit::Fixed(13)).is_err());
        assert!(with_node_limit("[defaults\n", NodeLimit::Fixed(2)).is_err());
    }
}
