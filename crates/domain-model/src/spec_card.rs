//! Alondra's cards about a task's specs: a change to the approved specs, and a lasting decision
//! proposed on its own. Their body names what they are about, so every client words them in its
//! own language from the task's record.

use crate::specs::{self, SpecContent, SpecDirs, SpecFileKind, SpecFiles};
use crate::validation::{TaskValidation, ValidationState};
use crate::{CardKind, CardSpec, ContentKind, QuestionOption, Task, TaskSpecs, clip};
use alondra_core::text::Lang;
use alondra_core::text::{t, tf};
use crate::specs::engine;
use serde::{Deserialize, Serialize};

pub const AMENDMENT_PREFIX: &str = "spec-amendment-";
pub const DECISION_PREFIX: &str = "spec-decision-";
pub const VALIDATION_PREFIX: &str = "spec-validation-";
pub const APPROVE: &str = "approve";
pub const REJECT: &str = "reject";
/// The implementer goes on with the model of the task's author.
pub const STRONGER: &str = "stronger";
/// The task goes back to an author to change the specs the work cannot meet.
pub const CHANGE_SPECS: &str = "change_specs";
/// A new validator tries again what could not be run.
pub const VALIDATE_AGAIN: &str = "validate_again";
pub const CANCEL: &str = "cancel";

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SpecCard {
    /// A change to the approved specs the author drafted.
    Amendment { task: String, id: String },
    /// A lasting decision the author proposed on a card of its own.
    Decision { task: String, id: String },
    /// The validator sent the work back in the last round Alondra allows in a row.
    ValidationExhausted {
        task: String,
        round: u32,
        /// The task has approved specs an author can change.
        #[serde(default)]
        specs: bool,
    },
    /// Two validators in a row could not run what the work still needs.
    ValidationStuck { task: String, round: u32 },
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
            Self::ValidationExhausted { specs, .. } => [
                option(STRONGER, t!(lang, "Go on with the strong model", "Seguir con el modelo fuerte"),
                    t!(lang, "A new implementer takes the task with the model of its author and the validator's findings; the validator checks it again after",
                        "Un implementador nuevo sigue con el modelo del autor y los hallazgos del validador; después el validador lo vuelve a comprobar")),
                option(CHANGE_SPECS, t!(lang, "Change the specs", "Cambiar las specs"),
                    t!(lang, "An author drafts a change of the specs these scenarios rest on, and you decide on it before the work goes on",
                        "Un autor redacta un cambio de las specs en las que se apoyan estos escenarios, y decides sobre él antes de seguir")),
                option(CANCEL, t!(lang, "Cancel the task", "Cancelar la tarea"),
                    t!(lang, "The task is cancelled and its work stays in its worktree until it closes", "La tarea se cancela y su trabajo queda en su worktree hasta que se cierre")),
            ].into_iter().filter(|option| *specs || option.id != CHANGE_SPECS).collect(),
            Self::ValidationStuck { .. } => vec![
                option(VALIDATE_AGAIN, t!(lang, "Validate again", "Validar otra vez"),
                    t!(lang, "A new validator runs what is still owed; if it cannot either, this card comes back",
                        "Un validador nuevo ejecuta lo que falta; si tampoco puede, vuelve esta tarjeta")),
                option(CANCEL, t!(lang, "Cancel the task", "Cancelar la tarea"),
                    t!(lang, "The task is cancelled and its work stays in its worktree until it closes", "La tarea se cancela y su trabajo queda en su worktree hasta que se cierre")),
            ],
        }
    }

    /// The card, in `lang`, from what the task records of it.
    pub fn card(&self, task: &Task, lang: Lang) -> CardSpec {
        let need = |text: String| clip(&text, CardSpec::NEED_MAX);
        let why = |text: String| clip(&text, CardSpec::WHY_MAX);
        let (title, specs): (&str, &TaskSpecs) = (&task.title, &task.specs);
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
            Self::ValidationExhausted { round, .. } => {
                CardSpec::new(CardKind::Decide, need(tf!(lang, "The validator sent back “{title}” three times: how does it go on?", "El validador devolvió «{title}» tres veces: ¿cómo sigue?")))
                    .why(why(t!(lang, "A validator that did not write the work checked it against its specs, and these scenarios still fail. Alondra does not send it back a fourth time by itself.",
                        "Un validador que no escribió el trabajo lo comprobó contra sus specs, y estos escenarios siguen fallando. Alondra no lo devuelve una cuarta vez por su cuenta.").to_owned()))
                    .content(ContentKind::Report, failing(&task.validation, *round, lang))
            }
            Self::ValidationStuck { round, .. } => {
                CardSpec::new(CardKind::Decide, need(tf!(lang, "“{title}” cannot be validated: how does it go on?", "«{title}» no se puede validar: ¿cómo sigue?")))
                    .why(why(t!(lang, "Two validators in a row could not run what its specs still need. Nothing counts as passed or failed until it runs.",
                        "Dos validadores seguidos no pudieron ejecutar lo que sus specs aún necesitan. Nada cuenta como aprobado ni fallido hasta que se ejecute.").to_owned()))
                    .content(ContentKind::Diagnosis, failing(&task.validation, *round, lang))
            }
        }
    }
}

/// What a round left failing or not run, each with what the validator said of it, then its findings.
pub fn failing(validation: &TaskValidation, number: u32, lang: Lang) -> String {
    let Some(round) = validation.rounds.iter().find(|round| round.number == number) else { return String::new() };
    let mut out = vec![];
    for item in round.items.iter().filter(|item| matches!(item.state, ValidationState::Unmet | ValidationState::Incomplete | ValidationState::Running)) {
        let what = match item.state {
            ValidationState::Unmet => t!(lang, "fails", "falla"),
            _ => t!(lang, "could not be run", "no se pudo ejecutar"),
        };
        let said = validation.record(&item.subject, item.kind, &item.inputs)
            .and_then(|record| record.reasoning.as_deref().or(record.output.as_deref()))
            .or(item.reason.as_deref())
            .map(|said| format!(": {}", said.trim()))
            .unwrap_or_default();
        out.push(format!("- `{}` ({}) {what}{said}", item.subject, kind_name(item.kind, lang)));
    }
    if let Some(findings) = round.findings.as_deref().filter(|findings| !findings.trim().is_empty()) {
        out.push(format!("\n**{}**\n\n{}", t!(lang, "The validator's findings", "Hallazgos del validador"), findings.trim()));
    }
    out.join("\n")
}

/// A kind of validation in plain words.
pub fn kind_name(kind: crate::validation::ValidationKind, lang: Lang) -> &'static str {
    use crate::validation::ValidationKind;
    match kind {
        ValidationKind::Suite => t!(lang, "test suite", "suite de tests"),
        ValidationKind::Adequate => t!(lang, "its test checks it · judgment", "su test lo comprueba · juicio"),
        ValidationKind::Judged => t!(lang, "judgment on its evidence", "juicio sobre su evidencia"),
        ValidationKind::Reference => t!(lang, "reference still true · judgment", "referencia vigente · juicio"),
    }
}

/// Each change of `files` in plain words, with what it held before: a spec as its title,
/// explanation and scenarios, never its raw syntax.
pub fn changes(files: &SpecFiles, before: &SpecFiles, dirs: &SpecDirs, lang: Lang) -> String {
    let mut out = vec![];
    for (path, after) in files {
        let old = before.get(path).cloned().flatten();
        let what = match (&old, after) {
            (None, Some(_)) => t!(lang, "new", "nuevo"),
            (Some(_), None) => t!(lang, "removed", "eliminado"),
            _ => t!(lang, "changed", "cambiado"),
        };
        out.push(format!("### `{path}` ({what})"));
        let shown = |content: &SpecContent| match content {
            SpecContent::Text(text) => describe(path, text, dirs, lang),
            SpecContent::Mockup(image) => tf!(lang, "Mockup {width}×{height}, {bytes} bytes", "Mockup {width}×{height}, {bytes} bytes", width = image.width, height = image.height, bytes = image.bytes),
        };
        let same_words = matches!((&old, after), (Some(old), Some(after)) if shown(old) == shown(after));
        let show = |content: &SpecContent| match content.text() {
            Some(text) if same_words => fenced(text),
            _ => shown(content),
        };
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
