//! Selection has no sampling: each winner must have one of the recorded reasons.
use super::beta::{Beta, probability};
use super::estimate::{CellEstimate, estimate};
use super::{DeciderInput, RankedModel, Recommendation, Rule};
use crate::task_tags::Impact;
use std::cmp::Ordering;

fn cheapest(a: &RankedModel, b: &RankedModel) -> Ordering {
    let (a_est, b_est) = (a.estimate.task(), b.estimate.task());
    a_est
        .cost
        .total_cmp(&b_est.cost)
        .then_with(|| a_est.steps.total_cmp(&b_est.steps))
        .then_with(|| b_est.quality().total_cmp(&a_est.quality()))
        .then_with(|| a.model.cmp(&b.model))
}

fn utility_order(a: &RankedModel, b: &RankedModel) -> Ordering {
    b.utility
        .total_cmp(&a.utility)
        .then_with(|| a.estimate.task().cost.total_cmp(&b.estimate.task().cost))
        .then_with(|| a.model.cmp(&b.model))
}

fn beta(cell: CellEstimate) -> Beta {
    Beta {
        alpha: cell.alpha,
        beta: cell.beta,
    }
}

fn clearly_better(c: &RankedModel, d: &RankedModel, input: &DeciderInput) -> bool {
    if c.utility <= d.utility {
        return false;
    }
    let candidates = [
        (c.estimate.area, d.estimate.area),
        (c.estimate.kind, d.estimate.kind),
        (Some(c.estimate.global), Some(d.estimate.global)),
    ];
    for (c, d) in candidates {
        if let (Some(c), Some(d)) = (c, d) {
            if c.own >= input.rules.minimum_samples && d.own >= input.rules.minimum_samples {
                return probability(beta(c), beta(d), input.rules.beta_tolerance)
                    >= input.rules.probability;
            }
        }
    }
    false
}

pub fn decide(input: &DeciderInput) -> Recommendation {
    let no_margin = !input
        .evidence
        .models
        .iter()
        .any(|m| m.candidate.with_margin);
    let mut rows: Vec<RankedModel> = input
        .evidence
        .models
        .iter()
        .map(|model| {
            let estimate = estimate(model, &input.evidence, input.tags.as_ref(), &input.rules);
            let apt = input
                .tags
                .as_ref()
                .map_or(model.candidate.sheet.is_some(), |_| {
                    model
                        .candidate
                        .sheet
                        .as_ref()
                        .is_some_and(|sheet| sheet.suits)
                        || estimate
                            .kind
                            .is_some_and(|cell| cell.own >= input.rules.minimum_samples)
                });
            RankedModel {
                model: model.candidate.wire_id(),
                eligible: no_margin || model.candidate.with_margin,
                apt,
                estimate,
                utility: 0.0,
            }
        })
        .collect();
    rows.sort_by(|a, b| a.model.cmp(&b.model));
    if !rows.iter().any(|r| r.eligible && r.apt) {
        let with_sheet: std::collections::BTreeSet<_> = input
            .evidence
            .models
            .iter()
            .filter(|m| m.candidate.sheet.is_some())
            .map(|m| m.candidate.wire_id())
            .collect();
        let any_sheet = rows
            .iter()
            .any(|r| r.eligible && with_sheet.contains(&r.model));
        for row in &mut rows {
            row.apt = !any_sheet || with_sheet.contains(&row.model);
        }
    }
    let min_cost = rows
        .iter()
        .filter(|r| r.eligible)
        .map(|r| r.estimate.task().cost)
        .min_by(f64::total_cmp)
        .unwrap_or(1.0);
    let min_steps = rows
        .iter()
        .filter(|r| r.eligible)
        .map(|r| r.estimate.task().steps)
        .min_by(f64::total_cmp)
        .unwrap_or(1.0);
    for row in &mut rows {
        let est = row.estimate.task();
        row.utility = est.quality()
            - input.rules.cost_weight * (est.cost / min_cost).log2()
            - input.rules.steps_weight * (est.steps / min_steps).log2();
    }
    let high = input.tags.as_ref().is_some_and(|tags| {
        tags.impact_with(input.rules.broad_area_count, &input.rules.broad_areas) == Impact::High
    });
    let available: Vec<_> = rows.iter().filter(|r| r.eligible).collect();
    let (chosen, rule) = if let Some(bound) = &input.bound {
        (Some(bound.clone()), Rule::User)
    } else if available.is_empty() {
        (None, Rule::None)
    } else if available.len() == 1 {
        (
            Some(available[0].model.clone()),
            if no_margin {
                Rule::NoMargin
            } else {
                Rule::OnlyOne
            },
        )
    } else if high {
        let chosen = available
            .iter()
            .min_by(|a, b| {
                b.estimate
                    .task()
                    .quality()
                    .total_cmp(&a.estimate.task().quality())
                    .then_with(|| a.estimate.task().cost.total_cmp(&b.estimate.task().cost))
                    .then_with(|| a.model.cmp(&b.model))
            })
            .unwrap();
        (
            Some(chosen.model.clone()),
            if no_margin {
                Rule::NoMargin
            } else {
                Rule::HighImpact
            },
        )
    } else {
        let default = *available
            .iter()
            .filter(|r| r.apt)
            .min_by(|a, b| cheapest(a, b))
            .expect("apt fallback");
        let chosen = *available
            .iter()
            .filter(|r| r.apt && (r.model == default.model || clearly_better(r, default, input)))
            .min_by(|a, b| utility_order(a, b))
            .expect("default included");
        let rule = if no_margin {
            Rule::NoMargin
        } else if chosen.model == default.model {
            Rule::CheapestApt
        } else {
            Rule::ClearlyBetter
        };
        (Some(chosen.model.clone()), rule)
    };
    rows.sort_by(utility_order);
    let why = chosen
        .as_ref()
        .and_then(|id| rows.iter().find(|row| &row.model == id))
        .map(|row| {
            let e = row.estimate.task();
            format!(
                "{}: {}; quality {:.4}, cost USD {:.4}, steps {:.2}, own {:.2}, utility {:.4}",
                row.model,
                rule.reason(),
                e.quality(),
                e.cost,
                e.steps,
                e.own,
                row.utility
            )
        })
        .unwrap_or_else(|| match &chosen {
            Some(id) => format!("{id}: user choice"),
            None => "No advertised models".into(),
        });
    Recommendation {
        recommended: chosen,
        rule,
        why,
        ranking: rows,
    }
}
