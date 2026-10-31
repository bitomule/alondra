use super::beta::{Beta, lgamma, probability};
use super::estimate::{priced, shrink};
use super::score::{milinepers, score};
use super::*;
#[test]
fn six_candidate_evidence_stays_compact_and_contains_only_the_queried_area() {
    let candidates = ["opus", "sonnet", "haiku", "fable"].map(candidate);
    let mut candidates = candidates.to_vec();
    for id in ["gpt-6.1-sol", "gpt-6-astra"] {
        candidates.push(Candidate {
            id: id.into(),
            provider: Provider::Codex,
            with_margin: true,
            sheet: sheet_for(Provider::Codex, id).cloned(),
        });
    }
    let mut i = input(&candidates, &[], Some(tags()));
    i.evidence.pool = stats(600, 80);
    for m in &mut i.evidence.models {
        m.global = Cell {
            n: 1200,
            score_points: 96000,
            cost_n: 1200,
            cost_log: 2400000,
            steps_n: 1200,
            steps_log: 4800000,
        };
        m.kind = Cell {
            n: 120,
            score_points: 9600,
            cost_n: 120,
            cost_log: 240000,
            steps_n: 120,
            steps_log: 480000,
        };
        m.areas.insert(
            TaskArea::Ui,
            Cell {
                n: 60,
                score_points: 4800,
                cost_n: 60,
                cost_log: 120000,
                steps_n: 60,
                steps_log: 240000,
            },
        );
    }
    let encoded = serde_json::to_vec(&i.evidence).unwrap();
    assert!(encoded.len() < 1400, "{} bytes", encoded.len());
    let replay: Evidence = serde_json::from_slice(&encoded).unwrap();
    assert_eq!(replay, i.evidence);
    let mut sample = sample("sonnet");
    sample.tags.as_mut().unwrap().areas = vec![TaskArea::Ui, TaskArea::Daemon, TaskArea::Data];
    let i = input(&candidates, &[sample], Some(tags()));
    for m in &i.evidence.models {
        assert!(m.areas.keys().all(|a| *a == TaskArea::Ui));
    }
}
use crate::{
    accounts::Provider,
    metrics::fold::Outcome,
    model_sheets::{sheet_for, sheets},
    task_tags::{Impact, TaskArea, TaskKind, TaskRisk, TaskSize, TaskTags},
};

pub(super) fn tags() -> TaskTags {
    TaskTags {
        kind: TaskKind::Docs,
        size: None,
        areas: vec![TaskArea::Ui],
        risks: vec![],
    }
}

pub(super) fn candidate(id: &str) -> Candidate {
    Candidate {
        id: id.into(),
        provider: Provider::Claude,
        with_margin: true,
        sheet: sheet_for(Provider::Claude, id).cloned(),
    }
}

pub(super) fn sample(id: &str) -> TaskSample {
    TaskSample {
        model: Some(ModelKey::new(Provider::Claude, id)),
        tags: Some(tags()),
        quality: Quality {
            outcome: Outcome::Done,
            change_rounds: 0,
            reviewer_rounds: 0,
            interventions: 0,
            reopened: false,
            had_pr: false,
            pr_merged: false,
        },
        steps: 0,
        subagent_steps: 0,
        usage: vec![],
    }
}

pub(super) fn input(
    candidates: &[Candidate],
    tasks: &[TaskSample],
    tags: Option<TaskTags>,
) -> DeciderInput {
    let rules = Rules::default();
    let evidence = aggregate(candidates, tasks, tags.as_ref(), sheets(), &rules);
    DeciderInput {
        tags,
        bound: None,
        rules,
        evidence,
    }
}

pub(super) fn equal_candidates() -> Vec<Candidate> {
    let a = candidate("sonnet");
    let mut b = a.clone();
    b.id = "competitor".into();
    vec![a, b]
}

pub(super) fn stats(n: i64, points: i64) -> Cell {
    Cell {
        n: n * 12,
        score_points: n * 12 * points,
        ..Cell::default()
    }
}

fn close(a: f64, b: f64, tolerance: f64) {
    assert!(
        (a - b).abs() <= tolerance,
        "{a} != {b}, tolerance {tolerance}"
    );
}

#[test]
fn score_measures_user_rework_without_double_counting_and_ignores_cancelled() {
    let r = Rules::default();
    let mut task = sample("sonnet").quality;
    assert_eq!(score(&task, &r), Some(100));
    for (rounds, expected) in [(1, 70), (2, 40), (3, 10), (4, 10)] {
        task.change_rounds = rounds;
        task.interventions = rounds;
        assert_eq!(score(&task, &r), Some(expected));
    }
    task = sample("sonnet").quality;
    task.reopened = true;
    assert_eq!(score(&task, &r), Some(70));
    task.reopened = false;
    for (questions, expected) in [(1, 95), (2, 90), (4, 80), (20, 80)] {
        task.interventions = questions;
        assert_eq!(score(&task, &r), Some(expected));
    }
    task.change_rounds = 9;
    task.reopened = true;
    assert_eq!(score(&task, &r), Some(0));
    task = sample("sonnet").quality;
    task.change_rounds = 2;
    task.reviewer_rounds = 2;
    assert_eq!(score(&task, &r), Some(100));
    task.had_pr = true;
    assert_eq!(score(&task, &r), Some(0));
    task.pr_merged = true;
    assert_eq!(score(&task, &r), Some(100));
    task.outcome = Outcome::Cancelled;
    assert_eq!(score(&task, &r), None);
}

#[test]
fn final_model_gets_one_sample_with_all_attempts_and_subagents_cost_and_steps() {
    let mut t = sample("sonnet");
    t.usage = vec![
        Usage {
            model: ModelKey::new(Provider::Claude, "opus"),
            cost_micros: Some(2_000_000),
            tokens: None,
        },
        Usage {
            model: ModelKey::new(Provider::Codex, "gpt-6.1-sol"),
            cost_micros: None,
            tokens: Some([1_000_000, 1_000_000, 1_000_000, 1_000_000]),
        },
    ];
    t.steps = 10;
    t.subagent_steps = 5;
    let candidates = [candidate("opus"), candidate("sonnet")];
    let i = input(&candidates, &[t.clone()], Some(tags()));
    assert_eq!(
        i.evidence.models[0].global.n, 0,
        "abandoned model gets no sample"
    );
    let c = i.evidence.models[1].global;
    assert_eq!(
        (c.n, c.score_points, c.cost_n, c.steps_n),
        (12, 1200, 12, 12)
    );
    assert_eq!(c.cost_log, 12 * milinepers(16.6));
    assert_eq!(c.steps_log, 12 * milinepers(15.0));
    t.quality.outcome = Outcome::Cancelled;
    let cancelled = input(&candidates, &[t], Some(tags())).evidence;
    assert_eq!(cancelled.pool, Cell::default());
    assert_eq!(cancelled.tokens, TokenPool::default());
    assert!(cancelled.models.iter().all(|m| m.global.n == 0));
}

#[test]
fn missing_and_zero_cost_steps_and_token_components_do_not_supply_logs() {
    let a = sample("sonnet");
    let mut b = a.clone();
    b.usage.push(Usage {
        model: b.model.clone().unwrap(),
        cost_micros: Some(0),
        tokens: Some([0; 4]),
    });
    let i = input(&[candidate("sonnet")], &[a, b], Some(tags()));
    assert_eq!(
        (
            i.evidence.pool.n,
            i.evidence.pool.cost_n,
            i.evidence.pool.steps_n
        ),
        (24, 0, 0)
    );
    assert_eq!(i.evidence.tokens.measured, 1);
    assert_eq!(i.evidence.tokens.n, [0; 4]);
    assert_eq!(estimate::token_mix(&i.evidence, &i.rules), [0.0; 4]);
    let zero_prior = decide(&i).ranking[0].estimate.task().cost;
    close(
        zero_prior,
        priced(
            &i.rules.fallback_tokens.map(|n| n as f64),
            &i.evidence.models[0].usd_per_mtok,
        ),
        1e-12,
    );
    assert!(zero_prior > 0.0 && zero_prior.is_finite());
    let mut t = sample("sonnet");
    t.usage.push(Usage {
        model: t.model.clone().unwrap(),
        cost_micros: None,
        tokens: Some([100, 10, 0, 0]),
    });
    let i = input(&[candidate("sonnet")], &[t], Some(tags()));
    let mix = estimate::token_mix(&i.evidence, &i.rules);
    assert!(mix[0] > 99.9 && mix[1] > 9.99);
    assert_eq!(&mix[2..], &[0.0, 0.0]);
}

#[test]
fn every_sheet_has_verified_prices_and_unknown_model_takes_priciest_provider_sheet() {
    for s in sheets() {
        assert!(s.usd_per_mtok.iter().all(|p| *p > 0.0));
        assert!(s.evidence.contains("https://") && s.evidence.contains("2026-10-31"));
    }
    assert_eq!(
        sheet_for(Provider::Codex, "gpt-unknown")
            .unwrap()
            .usd_per_mtok,
        sheet_for(Provider::Codex, "gpt-6.1-sol")
            .unwrap()
            .usd_per_mtok
    );
    let i = input(&[candidate("unknown")], &[], None);
    assert!(i.evidence.models[0].candidate.sheet.is_none());
    assert_eq!(
        i.evidence.models[0].usd_per_mtok,
        sheet_for(Provider::Claude, "fable").unwrap().usd_per_mtok
    );
    let mut changed = i.clone();
    changed.evidence.models[0].usd_per_mtok = [1.0; 4];
    assert_ne!(
        decide(&i),
        decide(&changed),
        "replay reads the saved prices"
    );
}

#[test]
fn empty_cells_shrink_to_their_parent_and_unsupported_kind_has_bounded_penalty() {
    let mut c = candidate("sonnet");
    let i = input(&[c.clone()], &[], Some(tags()));
    let e = estimate(
        &i.evidence.models[0],
        &i.evidence,
        i.tags.as_ref(),
        &i.rules,
    );
    close(e.global.quality(), e.kind.unwrap().quality(), 1e-15);
    close(e.kind.unwrap().quality(), e.area.unwrap().quality(), 1e-15);
    c.sheet.as_mut().unwrap().good_for.clear();
    let mut i = input(&[c], &[], Some(tags()));
    let e = estimate(
        &i.evidence.models[0],
        &i.evidence,
        i.tags.as_ref(),
        &i.rules,
    );
    close(e.kind.unwrap().quality(), e.global.quality() - 0.05, 1e-15);
    i.evidence.models[0].global = stats(1000, 0);
    let e = estimate(
        &i.evidence.models[0],
        &i.evidence,
        i.tags.as_ref(),
        &i.rules,
    );
    close(e.kind.unwrap().quality(), 0.05, 1e-15);
    assert!(e.area.unwrap().alpha > 0.0 && e.area.unwrap().beta > 0.0);
}

#[test]
fn fractional_areas_have_exact_integer_weights_and_average_parameters() {
    let mut t = sample("sonnet");
    t.tags.as_mut().unwrap().areas = vec![TaskArea::Ui, TaskArea::Daemon, TaskArea::Build];
    t.steps = 15;
    let mut query = tags();
    query.areas = t.tags.as_ref().unwrap().areas.clone();
    let i = input(&[candidate("sonnet")], &[t], Some(query));
    let m = &i.evidence.models[0];
    for c in m.areas.values() {
        assert_eq!(c.n, 4);
        assert_eq!(c.score_points, 400);
        assert_eq!(c.steps_n, 4);
    }
    let e = estimate(m, &i.evidence, i.tags.as_ref(), &i.rules);
    close(e.area.unwrap().own, 1.0 / 3.0, 1e-15);
    let mut query = tags();
    query.areas = vec![TaskArea::Ui, TaskArea::Data];
    let e = estimate(m, &i.evidence, Some(&query), &i.rules);
    let parent = e.kind.unwrap();
    let a = shrink(m.areas[&TaskArea::Ui], parent, i.rules.area_kappa, &i.rules);
    let b = shrink(Cell::default(), parent, i.rules.area_kappa, &i.rules);
    close(e.area.unwrap().alpha, (a.alpha + b.alpha) / 2.0, 1e-15);
    close(e.area.unwrap().beta, (a.beta + b.beta) / 2.0, 1e-15);
}

#[test]
fn untagged_and_retired_models_only_contribute_to_pool_and_global() {
    let mut t = sample("retired");
    t.tags = None;
    let i = input(
        &[candidate("sonnet"), candidate("retired")],
        &[t],
        Some(tags()),
    );
    assert_eq!(i.evidence.pool.n, 12);
    let m = i
        .evidence
        .models
        .iter()
        .find(|m| m.candidate.id == "retired")
        .unwrap();
    assert_eq!(m.global.n, 12);
    assert_eq!(m.kind.n, 0);
    assert!(m.areas.is_empty());
    let e = estimate(m, &i.evidence, None, &i.rules);
    assert!(e.kind.is_none() && e.area.is_none());
    let a = ModelKey::new(Provider::Claude, "claude/claude-opus-5-5");
    assert_eq!(a, ModelKey::new(Provider::Claude, "opus"));
    assert_eq!(
        ModelKey::new(Provider::Claude, "sonnet[1m]"),
        ModelKey::new(Provider::Claude, "sonnet")
    );
}

#[test]
fn advertised_aliases_share_statistics_without_stealing_another_alias_area_cells() {
    let i = input(
        &[candidate("sonnet"), candidate("claude-sonnet-5-5")],
        &[sample("sonnet[1m]")],
        Some(tags()),
    );
    assert_eq!(i.evidence.pool.n, 12);
    assert_eq!(i.evidence.models.len(), 2);
    assert_eq!(i.evidence.models[0].global, i.evidence.models[1].global);
    assert_eq!(i.evidence.models[0].kind, i.evidence.models[1].kind);
    assert_eq!(i.evidence.models[0].areas, i.evidence.models[1].areas);
    assert_eq!(i.evidence.models[0].areas[&TaskArea::Ui].n, 12);
}

fn log_b(a: f64, b: f64) -> f64 {
    lgamma(a) + lgamma(b) - lgamma(a + b)
}

fn miller(a: u32, b: u32, c: u32, d: u32) -> f64 {
    (0..a)
        .map(|i| {
            let i = i as f64;
            let (b, c, d) = (b as f64, c as f64, d as f64);
            (log_b(c + i, b + d) - (b + i).ln() - log_b(1.0 + i, b) - log_b(c, d)).exp()
        })
        .sum()
}

#[test]
fn beta_comparison_matches_independent_integer_formula_and_handles_fractional_endpoints() {
    for (a, b, c, d) in [
        (1, 1, 1, 1),
        (100, 1, 1, 1),
        (1, 100, 2, 3),
        (20, 70, 40, 30),
        (90, 10, 75, 25),
        (500, 500, 490, 510),
    ] {
        let p = probability(
            Beta {
                alpha: a as f64,
                beta: b as f64,
            },
            Beta {
                alpha: c as f64,
                beta: d as f64,
            },
            1e-9,
        );
        close(p, miller(a, b, c, d), 1e-6);
    }
    for (a, b) in [(0.01, 0.01), (0.25, 3.5), (100.0, 1.0), (3000.0, 3000.0)] {
        assert_eq!(
            probability(Beta { alpha: a, beta: b }, Beta { alpha: a, beta: b }, 1e-9),
            0.5
        );
    }
    close(
        probability(
            Beta {
                alpha: 0.5,
                beta: 0.5,
            },
            Beta {
                alpha: 1.0,
                beta: 1.0,
            },
            1e-9,
        ),
        0.5,
        1e-6,
    );
    close(
        probability(
            Beta {
                alpha: 0.25,
                beta: 0.25,
            },
            Beta {
                alpha: 2.0,
                beta: 2.0,
            },
            1e-9,
        ),
        0.5,
        1e-6,
    );
}

#[test]
fn lanczos_matches_reference_gamma_values() {
    for (x, expected) in [
        (0.5, 0.5723649429247001),
        (1.0, 0.0),
        (2.0, 0.0),
        (3.0, 0.6931471805599453),
        (0.1, 2.252712651734206),
        (10.0, 12.801827480081469),
        (100.0, 359.1342053695754),
    ] {
        close(lgamma(x), expected, 1e-11);
    }
}

#[test]
fn fractional_and_concentrated_betas_match_their_exact_mean_against_uniform() {
    let uniform = Beta {
        alpha: 1.0,
        beta: 1.0,
    };
    for (alpha, beta) in [
        (0.001, 0.002),
        (0.01, 10.0),
        (1000.0, 0.001),
        (0.25, 0.75),
        (0.01, 1000.0),
        (1500.0, 1400.0),
    ] {
        let c = Beta { alpha, beta };
        let p = probability(c, uniform, 1e-9);
        close(p, alpha / (alpha + beta), 1e-6);
        close(p + probability(uniform, c, 1e-9), 1.0, 1e-15);
    }
}

#[test]
fn default_is_cheapest_apt_and_all_high_impact_causes_choose_opus_without_history() {
    let candidates = [candidate("haiku"), candidate("sonnet"), candidate("opus")];
    let i = input(&candidates, &[], Some(tags()));
    assert_eq!(decide(&i).recommended.as_deref(), Some("claude/haiku"));
    for variant in 0..4 {
        let mut t = tags();
        t.kind = TaskKind::Feature;
        match variant {
            0 => t.risks.push(TaskRisk::Concurrency),
            1 => t.size = Some(TaskSize::Large),
            2 => t.areas = vec![TaskArea::Ui, TaskArea::Daemon, TaskArea::Build],
            _ => t.areas = vec![TaskArea::Data],
        }
        let result = decide(&input(&candidates, &[], Some(t)));
        assert_eq!(result.rule, Rule::HighImpact);
        assert_eq!(result.recommended.as_deref(), Some("claude/opus"));
    }
    for kind in [TaskKind::Fix, TaskKind::Docs] {
        let mut t = tags();
        t.kind = kind;
        t.risks.push(TaskRisk::Concurrency);
        let result = decide(&input(&candidates, &[], Some(t)));
        assert_eq!(result.rule, Rule::HighImpact);
        assert_eq!(result.recommended.as_deref(), Some("claude/sonnet"));
        for (model, quality) in [("claude/sonnet", 0.8105), ("claude/opus", 0.7905)] {
            let row = result.ranking.iter().find(|row| row.model == model).unwrap();
            close(row.estimate.task().quality(), quality, 1e-12);
        }
    }
    let mut t = tags();
    t.size = Some(TaskSize::Small);
    assert_eq!(
        decide(&input(&candidates, &[], Some(t)))
            .recommended
            .as_deref(),
        Some("claude/haiku")
    );
}

#[test]
fn binding_margin_single_none_apt_fallback_and_id_ties() {
    let candidates = equal_candidates();
    let mut i = input(&candidates, &[], Some(tags()));
    assert_eq!(decide(&i).recommended.as_deref(), Some("claude/competitor"));
    i.bound = Some("codex/not-advertised".into());
    assert_eq!(decide(&i).rule, Rule::User);
    assert_eq!(decide(&i).recommended, i.bound);
    i.bound = None;
    i.evidence.models[0].candidate.with_margin = false;
    let r = decide(&i);
    assert_eq!(r.rule, Rule::OnlyOne);
    assert!(
        r.ranking
            .iter()
            .find(|row| Some(&row.model) == r.recommended.as_ref())
            .unwrap()
            .eligible
    );
    i.evidence.models[1].candidate.with_margin = false;
    assert_eq!(decide(&i).rule, Rule::NoMargin);
    assert_eq!(decide(&input(&[], &[], None)).rule, Rule::None);
    let mut c = candidate("unknown");
    let r = decide(&input(&[c.clone()], &[], Some(tags())));
    assert!(r.ranking[0].apt);
    c.sheet = sheet_for(Provider::Claude, "opus").cloned();
    let mut t = tags();
    t.kind = TaskKind::Release;
    let r = decide(&input(&[c], &[], Some(t)));
    assert!(r.ranking[0].apt);
}

/// Isolate the threshold at the kind level with the design's 85% parent and kappa=5.
pub(super) fn threshold_input(success_d: i64, success_c: i64, n: i64) -> DeciderInput {
    let candidates = equal_candidates();
    let mut t = tags();
    t.areas.clear();
    let mut i = input(&candidates, &[], Some(t));
    i.rules.empty_quality = 0.85;
    for m in &mut i.evidence.models {
        m.candidate.sheet.as_mut().unwrap().benchmark = None;
        m.usd_per_mtok = [1.0; 4];
        m.kind = Cell {
            n: n * 12,
            score_points: 1200
                * if m.candidate.id == "sonnet" {
                    success_d
                } else {
                    success_c
                },
            ..Cell::default()
        };
    }
    // D is cheaper by less than one point of utility.
    i.evidence
        .models
        .iter_mut()
        .find(|m| m.candidate.id == "competitor")
        .unwrap()
        .usd_per_mtok = [1.05; 4];
    i
}

#[test]
fn threshold_requires_probability_samples_and_positive_net_utility() {
    let i = threshold_input(6, 9, 10);
    let r = decide(&i);
    assert_eq!(r.rule, Rule::ClearlyBetter);
    assert_eq!(r.recommended.as_deref(), Some("claude/competitor"));
    assert_eq!(decide(&threshold_input(7, 9, 10)).rule, Rule::CheapestApt);
    assert_eq!(decide(&threshold_input(0, 9, 9)).rule, Rule::CheapestApt);
    let mut i = threshold_input(6, 9, 10);
    i.evidence
        .models
        .iter_mut()
        .find(|m| m.candidate.id == "competitor")
        .unwrap()
        .usd_per_mtok = [1024.0; 4];
    assert_eq!(decide(&i).rule, Rule::CheapestApt);
    let mut i = threshold_input(6, 9, 10);
    let c = i
        .evidence
        .models
        .iter_mut()
        .find(|m| m.candidate.id == "competitor")
        .unwrap();
    c.kind.steps_n = 120;
    c.kind.steps_log = 120 * milinepers(1e12);
    assert_eq!(decide(&i).rule, Rule::CheapestApt);
    let mut i = threshold_input(8, 8, 10);
    for m in &mut i.evidence.models {
        m.usd_per_mtok = [1.0; 4];
        m.kind.steps_n = 120;
        m.kind.steps_log = 120
            * milinepers(if m.candidate.id == "sonnet" {
                100.0
            } else {
                50.0
            });
    }
    let r = decide(&i);
    let u = |id| r.ranking.iter().find(|r| r.model == id).unwrap().utility;
    assert!(u("claude/competitor") > u("claude/sonnet"));
    assert_eq!(r.recommended.as_deref(), Some("claude/competitor"));
}

#[test]
fn deepest_common_ten_samples_are_used_and_shallow_evidence_is_not_double_counted() {
    let mut i = threshold_input(6, 9, 10);
    i.tags.as_mut().unwrap().areas = vec![TaskArea::Ui];
    for m in &mut i.evidence.models {
        m.areas.insert(TaskArea::Ui, stats(9, 75));
    }
    assert_eq!(
        decide(&i).rule,
        Rule::ClearlyBetter,
        "nine area samples must fall back to kind"
    );
    for m in &mut i.evidence.models {
        m.areas.insert(
            TaskArea::Ui,
            stats(10, if m.candidate.id == "sonnet" { 100 } else { 0 }),
        );
    }
    assert_ne!(decide(&i).rule, Rule::ClearlyBetter);
}

#[test]
fn equal_quality_cannot_buy_an_upgrade_just_because_steps_raise_utility() {
    let mut i = threshold_input(8, 8, 10);
    for m in &mut i.evidence.models {
        m.kind.steps_n = 120;
        m.kind.steps_log = 120
            * milinepers(if m.candidate.id == "competitor" {
                1.0
            } else {
                1000.0
            });
    }
    let r = decide(&i);
    let competitor = r
        .ranking
        .iter()
        .find(|row| row.model == "claude/competitor")
        .unwrap();
    let default = r
        .ranking
        .iter()
        .find(|row| row.model == "claude/sonnet")
        .unwrap();
    assert!(competitor.utility > default.utility);
    assert_eq!(r.rule, Rule::CheapestApt);
    assert_eq!(r.recommended.as_deref(), Some("claude/sonnet"));
}

#[test]
fn saved_input_replays_without_current_sheets_and_rules_affect_the_decision() {
    let i = threshold_input(6, 9, 10);
    let bytes = serde_json::to_vec(&i).unwrap();
    let replay: DeciderInput = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(decide(&i), decide(&replay));
    let mut changed = replay;
    changed.rules.probability = 1.0;
    assert_eq!(decide(&changed).rule, Rule::CheapestApt);
    assert_ne!(
        serde_json::to_vec(&i.rules).unwrap(),
        serde_json::to_vec(&changed.rules).unwrap()
    );
    assert!(priced(&[1e6; 4], &[1.0; 4]) == 4.0);
}

#[test]
fn old_tags_round_trip_without_size_and_unknown_sizes_are_refused() {
    let old: TaskTags = serde_json::from_str(r#"{"kind":"fix","areas":["ui"]}"#).unwrap();
    assert_eq!(old.size, None);
    assert_eq!(old.impact(), Impact::Normal);
    assert!(!serde_json::to_string(&old).unwrap().contains("size"));
    assert!(serde_json::from_str::<TaskTags>(r#"{"kind":"fix","size":"huge"}"#).is_err());
    for s in TaskSize::ALL {
        assert_eq!(TaskSize::parse(s.as_str()), Some(*s));
    }
    for risk in [
        TaskRisk::Concurrency,
        TaskRisk::Security,
        TaskRisk::DataMigration,
    ] {
        let mut t = old.clone();
        t.risks.push(risk);
        assert_eq!(t.impact(), Impact::High);
    }
    for risk in [TaskRisk::UnclearSpec, TaskRisk::UserVisible] {
        let mut t = old.clone();
        t.risks.push(risk);
        assert_eq!(t.impact(), Impact::Normal);
    }
}
