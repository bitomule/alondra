//! Integer sufficient statistics and hierarchical estimates.
use super::score::{milinepers, score};
use super::{
    Candidate, CandidateEvidence, Cell, Evidence, ModelEvidence, ModelKey, Rules, SheetEvidence,
    TaskSample,
};
use crate::{
    model_sheets::ModelSheet,
    task_tags::{TaskArea, TaskTags},
};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct CellEstimate {
    pub alpha: f64,
    pub beta: f64,
    pub own: f64,
    pub cost: f64,
    pub steps: f64,
}

impl CellEstimate {
    pub fn quality(self) -> f64 {
        self.alpha / (self.alpha + self.beta)
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ModelEstimate {
    pub global: CellEstimate,
    pub kind: Option<CellEstimate>,
    pub area: Option<CellEstimate>,
}

impl ModelEstimate {
    pub fn task(&self) -> CellEstimate {
        self.area.or(self.kind).unwrap_or(self.global)
    }
}

impl Cell {
    fn add(&mut self, weight: i64, points: i64, cost: Option<f64>, steps: u64) {
        self.n += weight;
        self.score_points += weight * points;
        if let Some(cost) = cost.filter(|cost| *cost > 0.0) {
            self.cost_n += weight;
            self.cost_log += weight * milinepers(cost);
        }
        if steps > 0 {
            self.steps_n += weight;
            self.steps_log += weight * milinepers(steps as f64);
        }
    }
}

/// Resolve absent sheets conservatively against the supplied sheet snapshot.
pub fn prices<'a>(
    key: &ModelKey,
    snapshot: &'a [ModelSheet],
    tokens: &[f64; 4],
) -> Option<&'a ModelSheet> {
    let bare = crate::model_table::measured_key(key.provider, &key.model);
    let comparable = bare.strip_prefix("claude-").unwrap_or(&bare);
    snapshot
        .iter()
        .find(|s| s.provider == key.provider && comparable.starts_with(&s.prefix))
        .or_else(|| {
            snapshot
                .iter()
                .filter(|s| s.provider == key.provider)
                .max_by(|a, b| {
                    priced(tokens, &a.usd_per_mtok)
                        .total_cmp(&priced(tokens, &b.usd_per_mtok))
                        .then_with(|| a.prefix.cmp(&b.prefix))
                })
        })
}

pub fn priced(tokens: &[f64; 4], prices: &[f64; 4]) -> f64 {
    tokens
        .iter()
        .zip(prices)
        .map(|(n, price)| n * price / 1_000_000.0)
        .sum()
}

/// The caller filters the 90-day window. This helper also serves as the SQL aggregation oracle.
pub fn aggregate(
    candidates: &[Candidate],
    tasks: &[TaskSample],
    tags: Option<&TaskTags>,
    snapshot: &[ModelSheet],
    rules: &Rules,
) -> Evidence {
    let mut evidence = Evidence::default();
    // Token mix is per completed task, not per usage row or attempt.
    for task in tasks
        .iter()
        .filter(|task| score(&task.quality, rules).is_some())
    {
        let mut total = [0u64; 4];
        let mut measured = false;
        for usage in &task.usage {
            if let Some(tokens) = usage.tokens {
                measured = true;
                for i in 0..4 {
                    total[i] += tokens[i];
                }
            }
        }
        if measured {
            evidence.tokens.measured += 1;
            for (i, value) in total.into_iter().enumerate().filter(|(_, n)| *n > 0) {
                evidence.tokens.n[i] += 1;
                evidence.tokens.logs[i] += milinepers(value as f64);
            }
        }
    }
    let mix = token_mix(&evidence, rules);
    let pricing_mix = if mix.iter().all(|n| *n == 0.0) {
        rules.fallback_tokens.map(|n| n as f64)
    } else {
        mix
    };
    let mut global: BTreeMap<ModelKey, Cell> = BTreeMap::new();
    let mut kind: BTreeMap<ModelKey, Cell> = BTreeMap::new();
    let mut areas: BTreeMap<ModelKey, BTreeMap<TaskArea, Cell>> = BTreeMap::new();
    for task in tasks {
        let Some(points) = score(&task.quality, rules) else {
            continue;
        };
        // Picodollars keep even sub-microdollar token prices additive before taking a log.
        let cost: u128 = task
            .usage
            .iter()
            .filter_map(|usage| {
                usage
                    .cost_micros
                    .map(|c| u128::from(c) * 1_000_000)
                    .or_else(|| {
                        let tokens = usage.tokens?;
                        let rates = prices(&usage.model, snapshot, &pricing_mix)?.usd_per_mtok;
                        Some(
                            tokens
                                .iter()
                                .zip(rates)
                                .map(|(n, rate)| {
                                    u128::from(*n) * (rate * 1_000_000.0).round() as u128
                                })
                                .sum(),
                        )
                    })
            })
            .sum();
        let cost = cost as f64 / 1_000_000_000_000.0;
        let cost = (cost > 0.0).then_some(cost);
        let steps = task.steps + task.subagent_steps;
        evidence.pool.add(12, points, cost, steps);
        let Some(key) = task.model.as_ref() else {
            continue;
        };
        let key = ModelKey::new(key.provider, &key.model);
        global
            .entry(key.clone())
            .or_default()
            .add(12, points, cost, steps);
        if let (Some(wanted), Some(actual)) = (tags, &task.tags) {
            if actual.kind != wanted.kind {
                continue;
            }
            kind.entry(key.clone())
                .or_default()
                .add(12, points, cost, steps);
            let unique: std::collections::BTreeSet<_> = actual.areas.iter().copied().collect();
            if !unique.is_empty() {
                for area in &unique {
                    areas
                        .entry(key.clone())
                        .or_default()
                        .entry(*area)
                        .or_default()
                        .add(12 / unique.len() as i64, points, cost, steps);
                }
            }
        }
    }
    let mut ordered = candidates.to_vec();
    ordered.sort_by_key(Candidate::wire_id);
    evidence.models = ordered
        .into_iter()
        .map(|candidate| {
            let key = candidate.key();
            let usd_per_mtok = candidate
                .sheet
                .as_ref()
                .or_else(|| prices(&key, snapshot, &pricing_mix))
                .expect("each candidate's provider needs a pricing sheet")
                .usd_per_mtok;
            ModelEvidence {
                candidate: CandidateEvidence {
                    provider: candidate.provider,
                    id: candidate.id,
                    with_margin: candidate.with_margin,
                    sheet: candidate.sheet.as_ref().map(|sheet| SheetEvidence {
                        suits: tags.is_none_or(|tags| sheet.suits(tags.kind)),
                        benchmark: sheet.benchmark,
                    }),
                },
                usd_per_mtok,
                global: global.get(&key).copied().unwrap_or_default(),
                kind: kind.get(&key).copied().unwrap_or_default(),
                areas: areas
                    .get(&key)
                    .into_iter()
                    .flat_map(|cells| cells.iter())
                    .filter(|(area, _)| tags.is_some_and(|tags| tags.areas.contains(area)))
                    .map(|(area, cell)| (*area, *cell))
                    .collect(),
            }
        })
        .collect();
    evidence
}

pub fn token_mix(evidence: &Evidence, rules: &Rules) -> [f64; 4] {
    std::array::from_fn(|i| {
        if evidence.tokens.measured == 0 {
            rules.fallback_tokens[i] as f64
        } else if evidence.tokens.n[i] == 0 {
            0.0
        } else {
            (evidence.tokens.logs[i] as f64 / (1000.0 * evidence.tokens.n[i] as f64)).exp()
        }
    })
}

pub fn shrink(cell: Cell, parent: CellEstimate, kappa: f64, rules: &Rules) -> CellEstimate {
    let n = cell.n as f64 / 12.0;
    let successes = cell.score_points as f64 / 1200.0;
    let q = parent.quality();
    CellEstimate {
        alpha: kappa * q + successes,
        beta: kappa * (1.0 - q) + n - successes,
        own: n,
        cost: log_estimate(cell.cost_log, cell.cost_n, parent.cost, rules.log_kappa),
        steps: log_estimate(cell.steps_log, cell.steps_n, parent.steps, rules.log_kappa),
    }
}

fn log_estimate(sum: i64, count: i64, prior: f64, kappa: f64) -> f64 {
    if count == 0 {
        return prior;
    }
    ((kappa * prior.ln() + sum as f64 / 12000.0) / (kappa + count as f64 / 12.0)).exp()
}

pub fn estimate(
    model: &ModelEvidence,
    evidence: &Evidence,
    tags: Option<&TaskTags>,
    rules: &Rules,
) -> ModelEstimate {
    let pool = evidence.pool;
    let pool_q = if pool.n == 0 {
        rules.empty_quality
    } else {
        (1.0 + pool.score_points as f64 / 1200.0) / (2.0 + pool.n as f64 / 12.0)
    };
    let benchmark = model
        .candidate
        .sheet
        .as_ref()
        .and_then(|s| s.benchmark)
        .unwrap_or(rules.benchmark_mean);
    let q = (pool_q + (benchmark - rules.benchmark_mean) / 100.0)
        .clamp(rules.prior_min, rules.prior_max);
    let mix = token_mix(evidence, rules);
    // A snapshot with only zeros has no positive log cost; keep a positive numerical root.
    let fallback_mix = rules.fallback_tokens.map(|n| n as f64);
    let root_mix = if mix.iter().all(|n| *n == 0.0) {
        &fallback_mix
    } else {
        &mix
    };
    let price = priced(root_mix, &model.usd_per_mtok);
    let steps = if pool.steps_n == 0 {
        rules.fallback_steps as f64
    } else {
        (pool.steps_log as f64 / (1000.0 * pool.steps_n as f64)).exp()
    };
    let parent = CellEstimate {
        alpha: q,
        beta: 1.0 - q,
        own: 0.0,
        cost: price,
        steps,
    };
    let global = shrink(model.global, parent, rules.global_kappa, rules);
    let Some(tags) = tags else {
        return ModelEstimate {
            global,
            kind: None,
            area: None,
        };
    };
    let mut parent = global;
    if !model.candidate.sheet.as_ref().is_some_and(|s| s.suits) {
        let q =
            (parent.quality() - rules.unsupported_penalty).clamp(rules.prior_min, rules.prior_max);
        parent.alpha = q;
        parent.beta = 1.0 - q;
    }
    let kind = shrink(model.kind, parent, rules.kind_kappa, rules);
    let unique: std::collections::BTreeSet<_> = tags.areas.iter().copied().collect();
    let area = if unique.is_empty() {
        None
    } else {
        let cells: Vec<_> = unique
            .iter()
            .map(|a| {
                shrink(
                    model.areas.get(a).copied().unwrap_or_default(),
                    kind,
                    rules.area_kappa,
                    rules,
                )
            })
            .collect();
        // Quality averages alpha/beta; log quantities average their log estimates.
        let count = unique.len() as f64;
        let cost = cells.iter().map(|c| c.cost.ln()).sum::<f64>();
        let steps = cells.iter().map(|c| c.steps.ln()).sum::<f64>();
        Some(CellEstimate {
            alpha: cells.iter().map(|c| c.alpha).sum::<f64>() / count,
            beta: cells.iter().map(|c| c.beta).sum::<f64>() / count,
            own: cells.iter().map(|c| c.own).sum::<f64>() / count,
            cost: (cost / count).exp(),
            steps: (steps / count).exp(),
        })
    };
    ModelEstimate {
        global,
        kind: Some(kind),
        area,
    }
}
