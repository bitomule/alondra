//! Pure routing from a snapshot: the caller owns accounts, the 90-day window and persistence.
pub mod beta;
pub mod choice;
pub mod estimate;
pub mod score;

use crate::{
    accounts::Provider,
    model_sheets::ModelSheet,
    task_tags::{TaskArea, TaskTags},
};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

pub use choice::decide;
pub use estimate::{CellEstimate, ModelEstimate, aggregate, estimate};
pub use score::{Quality, TaskSample, Usage};

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct ModelKey {
    pub provider: Provider,
    pub model: String,
}

impl ModelKey {
    pub fn new(provider: Provider, model: &str) -> Self {
        Self {
            provider,
            model: crate::model_table::measured_key(provider, model),
        }
    }
    pub fn wire_id(&self) -> String {
        format!("{}/{}", self.provider.as_str(), self.model)
    }
}

/// Twelve units are one task; score_points contains weighted integer scores, not fractions.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(from = "[i64; 6]", into = "[i64; 6]")]
pub struct Cell {
    pub n: i64,
    pub score_points: i64,
    pub cost_n: i64,
    pub cost_log: i64,
    pub steps_n: i64,
    pub steps_log: i64,
}

impl From<[i64; 6]> for Cell {
    fn from([n, score_points, cost_n, cost_log, steps_n, steps_log]: [i64; 6]) -> Self {
        Self {
            n,
            score_points,
            cost_n,
            cost_log,
            steps_n,
            steps_log,
        }
    }
}

impl From<Cell> for [i64; 6] {
    fn from(c: Cell) -> Self {
        [
            c.n,
            c.score_points,
            c.cost_n,
            c.cost_log,
            c.steps_n,
            c.steps_log,
        ]
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TokenPool {
    pub measured: i64,
    pub n: [i64; 4],
    pub logs: [i64; 4],
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Candidate {
    /// Advertised id, preserved for the launch; statistics use its measured key.
    pub id: String,
    pub provider: Provider,
    /// Whether choose_account(Margin) found an account serving this model.
    pub with_margin: bool,
    pub sheet: Option<ModelSheet>,
}

impl Candidate {
    pub fn key(&self) -> ModelKey {
        ModelKey::new(self.provider, &self.id)
    }
    pub fn wire_id(&self) -> String {
        format!("{}/{}", self.provider.as_str(), self.id)
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(from = "(bool, Option<f64>)", into = "(bool, Option<f64>)")]
pub struct SheetEvidence {
    pub suits: bool,
    pub benchmark: Option<f64>,
}

impl From<(bool, Option<f64>)> for SheetEvidence {
    fn from((suits, benchmark): (bool, Option<f64>)) -> Self {
        Self { suits, benchmark }
    }
}

impl From<SheetEvidence> for (bool, Option<f64>) {
    fn from(s: SheetEvidence) -> Self {
        (s.suits, s.benchmark)
    }
}

type CandidateParts = (Provider, String, bool, Option<SheetEvidence>);

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(from = "CandidateParts", into = "CandidateParts")]
pub struct CandidateEvidence {
    pub provider: Provider,
    pub id: String,
    pub with_margin: bool,
    pub sheet: Option<SheetEvidence>,
}

impl CandidateEvidence {
    pub fn wire_id(&self) -> String {
        format!("{}/{}", self.provider.as_str(), self.id)
    }
}

impl From<CandidateParts> for CandidateEvidence {
    fn from((provider, id, with_margin, sheet): CandidateParts) -> Self {
        Self {
            provider,
            id,
            with_margin,
            sheet,
        }
    }
}

impl From<CandidateEvidence> for CandidateParts {
    fn from(c: CandidateEvidence) -> Self {
        (c.provider, c.id, c.with_margin, c.sheet)
    }
}

type ModelParts = (
    CandidateEvidence,
    [f64; 4],
    Cell,
    Cell,
    BTreeMap<TaskArea, Cell>,
);

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(from = "ModelParts", into = "ModelParts")]
pub struct ModelEvidence {
    pub candidate: CandidateEvidence,
    /// Applied prices, including the conservative fallback when no sheet matched.
    pub usd_per_mtok: [f64; 4],
    pub global: Cell,
    pub kind: Cell,
    pub areas: BTreeMap<TaskArea, Cell>,
}

impl From<ModelParts> for ModelEvidence {
    fn from((candidate, usd_per_mtok, global, kind, areas): ModelParts) -> Self {
        Self {
            candidate,
            usd_per_mtok,
            global,
            kind,
            areas,
        }
    }
}

impl From<ModelEvidence> for ModelParts {
    fn from(m: ModelEvidence) -> Self {
        (m.candidate, m.usd_per_mtok, m.global, m.kind, m.areas)
    }
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Evidence {
    pub pool: Cell,
    pub tokens: TokenPool,
    pub models: Vec<ModelEvidence>,
}

/// All constants affecting a recorded decision travel beside its evidence.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Rules {
    pub change_penalty: i64,
    pub change_cap: i64,
    pub reopen_penalty: i64,
    pub interaction_penalty: i64,
    pub interaction_cap: i64,
    pub empty_quality: f64,
    pub benchmark_mean: f64,
    pub global_kappa: f64,
    pub kind_kappa: f64,
    pub area_kappa: f64,
    pub log_kappa: f64,
    pub unsupported_penalty: f64,
    pub prior_min: f64,
    pub prior_max: f64,
    pub minimum_samples: f64,
    pub probability: f64,
    pub cost_weight: f64,
    pub steps_weight: f64,
    pub broad_area_count: usize,
    pub broad_areas: Vec<TaskArea>,
    pub large_lines: u64,
    pub fallback_tokens: [u64; 4],
    pub fallback_steps: u64,
    pub fallback_date: String,
    pub beta_tolerance: f64,
}

impl Default for Rules {
    fn default() -> Self {
        Self {
            change_penalty: score::CHANGE_PENALTY,
            change_cap: score::CHANGE_CAP,
            reopen_penalty: score::REOPEN_PENALTY,
            interaction_penalty: score::INTERACTION_PENALTY,
            interaction_cap: score::INTERACTION_CAP,
            empty_quality: 0.80,
            benchmark_mean: 60.75,
            global_kappa: 10.0,
            kind_kappa: 5.0,
            area_kappa: 5.0,
            log_kappa: 3.0,
            unsupported_penalty: 0.05,
            prior_min: 0.05,
            prior_max: 0.95,
            minimum_samples: 10.0,
            probability: 0.90,
            cost_weight: 0.05,
            steps_weight: 0.05,
            broad_area_count: 3,
            broad_areas: vec![TaskArea::Data],
            large_lines: 1000,
            fallback_tokens: [4300, 49000, 12400000, 305000],
            fallback_steps: 56,
            fallback_date: "2026-10-27".into(),
            beta_tolerance: 1e-9,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct DeciderInput {
    pub tags: Option<TaskTags>,
    pub bound: Option<String>,
    pub rules: Rules,
    pub evidence: Evidence,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Rule {
    User,
    HighImpact,
    CheapestApt,
    ClearlyBetter,
    OnlyOne,
    NoMargin,
    None,
}

impl Rule {
    pub fn reason(self) -> &'static str {
        match self {
            Self::User => "user choice",
            Self::HighImpact => "highest expected quality for high impact",
            Self::CheapestApt => "cheapest suitable model",
            Self::ClearlyBetter => "statistically better with higher utility",
            Self::OnlyOne => "only eligible model",
            Self::NoMargin => "no account has margin; launch will wait for capacity",
            Self::None => "no advertised models",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RankedModel {
    pub model: String,
    pub eligible: bool,
    pub apt: bool,
    pub estimate: ModelEstimate,
    pub utility: f64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Recommendation {
    pub recommended: Option<String>,
    pub rule: Rule,
    pub why: String,
    pub ranking: Vec<RankedModel>,
}

#[cfg(test)]
mod properties;
#[cfg(test)]
mod tests;
