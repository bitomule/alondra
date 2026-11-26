//! Alondra's cards about a task's specs: a change to the approved specs, and a lasting decision
//! proposed on its own. Their body names what they are about, so every client words them in its
//! own language from the task's record.

use crate::specs::{self, SpecDirs, SpecFileKind, SpecFiles};
use crate::{CardKind, CardSpec, ContentKind, QuestionOption, TaskSpecs, clip};
use alondra_core::text::Lang;
use alondra_core::text::{t, tf};
use crate::specs::engine;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

pub const AMENDMENT_PREFIX: &str = "spec-amendment-";
pub const DECISION_PREFIX: &str = "spec-decision-";
pub const APPROVE: &str = "approve";
pub const REJECT: &str = "reject";

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SpecCard {
    /// A change to the approved specs the author drafted.
    Amendment { task: String, id: String },
    /// A lasting decision the author proposed on a card of its own.
    Decision { task: String, id: String },
}

impl SpecCard {
    pub fn to_body(&self) -> String {
        serde_json::to_string(self).unwrap_or_default()
    }

    pub fn from_body(body: &str) -> Option<Self> {
        serde_json::from_str(body).ok()
    }

    pub fn options(&self, lang: Lang) -> Vec<QuestionOption> {
        let option = |id: &str, label: &str, description: &str| QuestionOption { id: id.into(), label: label.into(), description: Some(description.into()) };
        match self {
            Self::Amendment { .. } => vec![
                option(APPROVE, t!(lang, "Approve the change", "Aprobar el cambio"),
                    t!(lang, "The approved specs take this change, Alondra writes it into the task's worktree and the implementer commits it",
                        "Las specs aprobadas incorporan este cambio, Alondra lo escribe en el worktree de la tarea y el implementador lo comitea")),
                option(REJECT, t!(lang, "Keep the approved specs", "Mantener las specs aprobadas"),
                    t!(lang, "Nothing changes in the specs; the implementer is told, with what you write here",
                        "Las specs no cambian; el implementador se entera, con lo que escribas aquí")),
            ],
            Self::Decision { .. } => vec![
                option(APPROVE, t!(lang, "Accept the decision", "Aceptar la decisión"),
                    t!(lang, "It is written to the project's decisions as accepted, and any decision it replaces is marked as replaced by it",
                        "Se escribe en las decisiones del proyecto como aceptada, y la que sustituya queda marcada como sustituida por ella")),
                option(REJECT, t!(lang, "Reject the decision", "Rechazar la decisión"),
                    t!(lang, "Nothing is written; Alondra keeps that you rejected it, with what you write here",
                        "No se escribe nada; Alondra guarda que la rechazaste, con lo que escribas aquí")),
            ],
        }
    }

    /// The card, in `lang`, from what the task records of it.
    pub fn card(&self, title: &str, specs: &TaskSpecs, lang: Lang) -> CardSpec {
        let need = |text: String| clip(&text, CardSpec::NEED_MAX);
        let why = |text: String| clip(&text, CardSpec::WHY_MAX);
        match self {
            Self::Amendment { id, .. } => {
                let amendment = specs.amendment(id);
                let reason = amendment.map(|a| a.reason.as_str()).unwrap_or_default();
                let dirs = specs.approved.as_ref().map(|a| a.dirs.clone()).unwrap_or_default();
                let mut body = tf!(lang, "**Why the implementer asks for it:** {reason}", "**Por qué lo pide el implementador:** {reason}", reason = reason.trim());
                if let Some(amendment) = amendment {
                    body.push_str("\n\n");
                    body.push_str(&changes(&amendment.files, &amendment.before, &dirs, lang));
                }
                CardSpec::new(CardKind::Decide, need(tf!(lang, "Change the specs of “{title}”?", "¿Cambiar las specs de «{title}»?")))
                    .why(why(t!(lang, "Its implementer found something the approved specs do not cover. Until you decide, it carries on with what does not depend on it.",
                        "Su implementador encontró algo que las specs aprobadas no cubren. Mientras decides, sigue con lo que no depende de ello.").to_owned()))
                    .content(ContentKind::Document, body)
            }
            Self::Decision { id, .. } => {
                let proposed = specs.proposed_decisions.iter().find(|d| d.id == *id);
                let decision_title = proposed.map(|d| d.title.as_str()).unwrap_or(id);
                let body = proposed.map(|d| changes(&d.files, &d.before, &d.dirs, lang)).unwrap_or_default();
                CardSpec::new(CardKind::Decide, need(tf!(lang, "Accept the decision “{decision_title}”?", "¿Aceptar la decisión «{decision_title}»?")))
                    .why(why(tf!(lang, "The author of “{title}” proposes it as a lasting decision of the project: specs may rest on it once you accept it.",
                        "El autor de «{title}» la propone como decisión duradera del proyecto: las specs pueden apoyarse en ella cuando la aceptes.")))
                    .content(ContentKind::Document, body)
            }
        }
    }
}

/// Each change of `files` in plain words, with what it held before: a spec as its title,
/// explanation and scenarios, never its raw syntax.
pub fn changes(files: &SpecFiles, before: &BTreeMap<String, Option<String>>, dirs: &SpecDirs, lang: Lang) -> String {
    let mut out = vec![];
    for (path, after) in files {
        let old = before.get(path).cloned().flatten();
        let what = match (&old, after) {
            (None, Some(_)) => t!(lang, "new", "nuevo"),
            (Some(_), None) => t!(lang, "removed", "eliminado"),
            _ => t!(lang, "changed", "cambiado"),
        };
        out.push(format!("### `{path}` ({what})"));
        let shown = |text: &str| describe(path, text, dirs, lang);
        let same_words = matches!((&old, after), (Some(old), Some(after)) if shown(old) == shown(after));
        let show = |text: &str| if same_words { fenced(text) } else { shown(text) };
        if let Some(old) = &old {
            out.push(format!("**{}**\n\n{}", t!(lang, "Before", "Antes"), show(old)));
        }
        if let Some(after) = after {
            out.push(format!("**{}**\n\n{}", t!(lang, "After", "Después"), show(after)));
        }
    }
    out.join("\n\n")
}

/// A spec file in plain words: its title and explanation, then each scenario with its steps; any
/// other file as it is written.
pub fn describe(path: &str, text: &str, dirs: &SpecDirs, lang: Lang) -> String {
    match dirs.kind(path) {
        Some(SpecFileKind::Spec) => match engine::read_spec(path, text.as_bytes()) {
            Ok(spec) => spec_card(&spec, lang),
            Err(_) => fenced(text),
        },
        Some(SpecFileKind::Decision) => {
            let fields = specs::decision_fields(text);
            let status = specs::frontmatter(text, "status").unwrap_or_default();
            let mut out = format!("**{}** (`{}`, {status})\n\n{}\n\n**{}:** {}\n\n**{}:** {}\n\n**{}:** {}",
                fields.title, fields.id, fields.decision,
                t!(lang, "Context", "Contexto"), fields.context, t!(lang, "Consequences", "Consecuencias"), fields.consequences,
                t!(lang, "How it is checked", "Cómo se comprueba"), fields.confirmation);
            if !fields.supersedes.is_empty() {
                out.push_str(&tf!(lang, "\n\n**Replaces:** {ids}", "\n\n**Sustituye a:** {ids}", ids = fields.supersedes.join(", ")));
            }
            if let Some(by) = specs::frontmatter(text, "superseded_by").filter(|by| !by.is_empty()) {
                out.push_str(&tf!(lang, "\n\n**Replaced by:** {by}", "\n\n**Sustituida por:** {by}"));
            }
            out
        }
        _ => fenced(text),
    }
}

/// A spec as a card shows it: its title and explanation first, what it rests on, then its
/// scenarios.
pub fn spec_card(spec: &engine::Spec, lang: Lang) -> String {
    let mut out = vec![format!("**{}** · `{}`", spec.title, spec.id)];
    if !spec.description.is_empty() {
        out.push(spec.description.join(" "));
    }
    if !spec.decisions.is_empty() {
        out.push(tf!(lang, "Rests on the decisions {ids}", "Se apoya en las decisiones {ids}", ids = spec.decisions.iter().map(|id| format!("`{id}`")).collect::<Vec<_>>().join(", ")));
    }
    if !spec.background.is_empty() {
        out.push(format!("**{}**\\\n{}", t!(lang, "Always first", "Siempre antes"), steps_text(&spec.background)));
    }
    for scenario in &spec.scenarios {
        out.push(scenario_text(&spec.id, scenario, lang));
    }
    out.join("\n\n")
}

/// One scenario: its title and identifier, whether it is judged and on what, its steps with their
/// tables and texts, and its examples.
pub fn scenario_text(spec: &str, scenario: &engine::Scenario, lang: Lang) -> String {
    let judged = match &scenario.judgment {
        Some(judgment) => match (&judgment.pattern, judgment.evidence) {
            (Some(pattern), _) => tf!(lang, " · judged on `{pattern}`", " · de juicio sobre `{pattern}`"),
            (None, engine::EvidenceKind::Capture) => t!(lang, " · judged on captures", " · de juicio sobre capturas").to_owned(),
            (None, engine::EvidenceKind::Video) => t!(lang, " · judged on a video", " · de juicio sobre un vídeo").to_owned(),
            (None, engine::EvidenceKind::None) => t!(lang, " · judged", " · de juicio").to_owned(),
        },
        None => String::new(),
    };
    let mut lines = vec![format!("▸ **{}** · `{spec}/{}`{judged}", scenario.title, scenario.id), steps_text(&scenario.steps)];
    if !scenario.examples.is_empty() {
        lines.push(format!("· {}", t!(lang, "Examples:", "Ejemplos:")));
        lines.extend(scenario.examples.iter().map(|row| format!("  `{}`", row.join(" | "))));
    }
    lines.join("\\\n")
}

/// Steps one per line, with their tables and texts under them, in one paragraph: a comment on it
/// is a comment on the whole of them.
pub fn steps_text(steps: &[engine::Step]) -> String {
    let mut lines = vec![];
    for step in steps {
        lines.push(format!("· {} {}", step.keyword.as_str(), step.text));
        lines.extend(step.table.iter().map(|row| format!("  `{}`", row.join(" | "))));
        if let Some(doc) = &step.doc {
            lines.extend(doc.lines().map(|line| format!("  `{line}`")));
        }
    }
    lines.join("\\\n")
}

fn fenced(text: &str) -> String {
    format!("```\n{}\n```", text.trim_end())
}
