//! A spec and a decision as every client draws them, never as raw Gherkin: a spec's explanation,
//! the decisions it rests on, its diagrams and mockups, and each scenario with its steps in three
//! lanes; in a delivery, how each scenario was validated. The daemon words every label in the
//! viewer's language; a client only lays them out. A client that does not know these items draws
//! the row asking to update.
use super::{Action, Icon, Link, Tone};
use serde::{Deserialize, Serialize};

/// How a thing compares with what the project already has.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SheetChange {
    New,
    Changed,
    Removed,
    #[serde(other)]
    Unchanged,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SheetMark {
    pub change: SheetChange,
    pub label: String,
}

/// What a comment on a part of a sheet quotes to say which part it is about.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SheetAnchor {
    pub id: String,
    pub quote: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SheetReference {
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub link: Option<Link>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SheetReferences {
    pub label: String,
    pub items: Vec<SheetReference>,
}

/// An image Alondra keeps, read like an attachment from `url`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SheetPicture {
    pub url: String,
    /// In pixels; 0 when unknown.
    pub width: u32,
    pub height: u32,
    pub bytes: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SheetPartKind {
    Diagram,
    Mockup,
    #[serde(other)]
    Other,
}

/// A diagram or a mockup of a spec or of one of its scenarios.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SheetPart {
    pub id: String,
    pub kind: SheetPartKind,
    /// The file name the tag cites.
    pub name: String,
    /// «Diagram “Refund flow”», «Mockup “Refund sheet”».
    pub title: String,
    /// What a screen reader says of it: the part and what it illustrates.
    pub alt: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mark: Option<SheetMark>,
    /// A diagram's Mermaid text.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub code: Option<String>,
    /// A mockup's image.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub picture: Option<SheetPicture>,
    /// Why it cannot be shown, when it cannot.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unavailable: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub anchor: Option<SheetAnchor>,
    /// The screen that edits it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub edit: Option<Link>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SheetStep {
    pub text: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub table: Vec<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub doc: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SheetLaneKind {
    Given,
    When,
    Then,
    #[serde(other)]
    Other,
}

/// The steps of one of the three lanes; `And` and `But` join the lane before them.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SheetLane {
    pub kind: SheetLaneKind,
    pub label: String,
    pub steps: Vec<SheetStep>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SheetTable {
    pub label: String,
    /// The header row first.
    pub rows: Vec<Vec<String>>,
}

/// A line of a result that carries its own icon and tone, so it never depends on color alone.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SheetLine {
    pub icon: Icon,
    pub tone: Tone,
    pub text: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

/// A validator's verdict: always a judgment, never a fact a test showed.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SheetVerdict {
    pub line: SheetLine,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub nearest_label: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub nearest_wrong: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub evidence: Vec<SheetPart>,
}

/// How a scenario of a delivery was validated.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SheetResult {
    pub state: SheetLine,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub checks: Vec<SheetLine>,
    /// Each test that declares it, by file and line, with its suites and their results.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tests: Vec<SheetLine>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub verdicts: Vec<SheetVerdict>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SheetScenario {
    pub id: String,
    pub title: String,
    /// «test», «judgment on captures».
    pub kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mark: Option<SheetMark>,
    /// Drawn closed until the reader opens it.
    #[serde(default)]
    pub collapsed: bool,
    pub lanes: Vec<SheetLane>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub examples: Option<SheetTable>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub parts: Vec<SheetPart>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<SheetResult>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub anchor: Option<SheetAnchor>,
    /// The screen that edits it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub edit: Option<Link>,
    /// The screens that add a diagram or a mockup to it.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub add: Vec<SheetReference>,
}

/// One spec.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SpecSheet {
    pub id: String,
    /// The identifier of the spec.
    pub spec: String,
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mark: Option<SheetMark>,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub explanation: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub decisions: Option<SheetReferences>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub parts: Vec<SheetPart>,
    /// The steps every scenario starts with.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub background: Option<SheetBackground>,
    pub scenarios: Vec<SheetScenario>,
    /// The scenarios the change leaves as they are, folded into one line.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unchanged: Option<SheetUnchanged>,
    /// Said when it has no scenario.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub empty: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub anchor: Option<SheetAnchor>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub edit: Option<Link>,
    /// The screens that add a scenario, a diagram or a mockup to it.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub add: Vec<SheetReference>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub actions: Vec<Action>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SheetBackground {
    pub label: String,
    pub lanes: Vec<SheetLane>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SheetUnchanged {
    /// «12 unchanged scenarios».
    pub label: String,
    pub scenarios: Vec<SheetScenario>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SheetSection {
    pub label: String,
    pub text: String,
}

/// A lasting decision.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct DecisionSheet {
    pub id: String,
    /// The identifier of the decision.
    pub decision: String,
    pub title: String,
    pub status: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mark: Option<SheetMark>,
    /// Context, decision, consequences and how it is checked.
    pub sections: Vec<SheetSection>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub supersedes: Option<SheetReferences>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub superseded_by: Option<SheetReferences>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub anchor: Option<SheetAnchor>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub edit: Option<Link>,
}

impl SpecSheet {
    /// Every scenario it holds, the folded ones included.
    pub fn all_scenarios(&self) -> impl Iterator<Item = &SheetScenario> {
        self.scenarios.iter().chain(self.unchanged.iter().flat_map(|unchanged| &unchanged.scenarios))
    }

    /// Every picture it draws or opens.
    pub fn pictures(&self) -> impl Iterator<Item = &SheetPicture> {
        let evidence = self.all_scenarios()
            .flat_map(|scenario| &scenario.result)
            .flat_map(|result| &result.verdicts)
            .flat_map(|verdict| &verdict.evidence);
        self.parts.iter().chain(self.all_scenarios().flat_map(|scenario| &scenario.parts)).chain(evidence).filter_map(|part| part.picture.as_ref())
    }
}
