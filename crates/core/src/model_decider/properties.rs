use super::estimate::shrink;
use super::tests::{candidate, equal_candidates, input, sample, stats, tags, threshold_input};
use super::*;
use crate::task_tags::{TaskArea, TaskRisk, TaskSize};
use proptest::prelude::*;

fn parent(q: f64) -> CellEstimate {
    CellEstimate {
        alpha: q,
        beta: 1.0 - q,
        own: 0.0,
        cost: 2.0,
        steps: 56.0,
    }
}

fn no_less(a: f64, b: f64) {
    assert!(a + 1e-12 >= b, "{a} < {b}");
}

proptest! {
    #![proptest_config(ProptestConfig { cases:128, failure_persistence:None, ..ProptestConfig::default() })]

    #[test]
    fn increasing_a_task_score_never_reduces_its_cell_or_descendants(
        points in 0i64..100, increase in 0i64..101, n in 1i64..400, q in 0.05f64..0.95
    ) {
        let r=Rules::default();
        let original=stats(n,points);
        let mut raised=original;
        raised.score_points+=12*increase.min(100-points);
        let a=shrink(original,parent(q),r.global_kappa,&r);
        let b=shrink(raised,parent(q),r.global_kappa,&r);
        no_less(b.quality(),a.quality());
        let child=stats(3,50);
        let ac=shrink(child,a,r.kind_kappa,&r);let bc=shrink(child,b,r.kind_kappa,&r);
        no_less(bc.quality(),ac.quality());
        no_less(shrink(child,bc,r.area_kappa,&r).quality(),shrink(child,ac,r.area_kappa,&r).quality());
    }

    #[test]
    fn increasing_cost_or_steps_never_reduces_their_estimates(
        base in 1u64..1_000_000, extra in 0u64..1_000_000
    ) {
        let mut a=sample("sonnet");a.steps=base;
        a.usage.push(Usage { model:a.model.clone().unwrap(),cost_micros:Some(base),tokens:None });
        let mut b=a.clone();b.steps+=extra;b.usage[0].cost_micros=Some(base+extra);
        let candidates=[candidate("sonnet")];
        let a=input(&candidates,&[a],Some(tags()));let b=input(&candidates,&[b],Some(tags()));
        let a=estimate(&a.evidence.models[0],&a.evidence,a.tags.as_ref(),&a.rules);
        let b=estimate(&b.evidence.models[0],&b.evidence,b.tags.as_ref(),&b.rules);
        for (a,b) in [(a.global,b.global),(a.kind.unwrap(),b.kind.unwrap()),(a.task(),b.task())] {
            no_less(b.cost,a.cost);no_less(b.steps,a.steps);
        }
    }

    #[test]
    fn at_most_one_sample_stays_within_the_parent_bound(
        q in 0.001f64..0.999, points in 0i64..101, twelfths in 0i64..13, k in prop_oneof![Just(5.0),Just(10.0)]
    ) {
        let r=Rules::default();
        let c=Cell { n:twelfths,score_points:twelfths*points,..Cell::default() };
        let e=shrink(c,parent(q),k,&r);
        prop_assert!((e.quality()-q).abs()<=1.0/(k+1.0)+1e-12);
    }

    #[test]
    fn own_samples_converge_at_the_analytical_rate(
        q in 0.001f64..0.999, points in 0i64..101, n in 200i64..2000,
        k in prop_oneof![Just(5.0),Just(10.0)]
    ) {
        let r=Rules::default();let e=shrink(stats(n,points),parent(q),k,&r);
        let error=(e.quality()-points as f64/100.0).abs();
        prop_assert!(error<=k/(n as f64+k)+1e-12);
        if k==5.0 || n>=324 { prop_assert!(error<=0.03+1e-12); }
    }

    #[test]
    fn permuting_tasks_candidates_areas_and_usages_is_bit_identical(
        scores in proptest::collection::vec(0u8..5,0..40), rotate in 0usize..100
    ) {
        let mut tasks:Vec<_>=scores.iter().enumerate().map(|(i,rounds)| {
            let mut t=sample(if i%2==0 {"sonnet"} else {"opus"});
            t.quality.change_rounds=i64::from(*rounds);t.quality.interventions=i64::from(*rounds);
            t.tags.as_mut().unwrap().areas=vec![TaskArea::Ui,TaskArea::Daemon,TaskArea::Build];
            t.usage=vec![Usage { model:t.model.clone().unwrap(),cost_micros:Some(12345),tokens:Some([123,5,0,1]) },
                Usage { model:t.model.clone().unwrap(),cost_micros:Some(4321),tokens:Some([0,50,1000,0]) }];
            t.steps=3+i as u64;t
        }).collect();
        let mut candidates=vec![candidate("sonnet"),candidate("opus")];
        let mut query=tags();query.areas=vec![TaskArea::Ui,TaskArea::Daemon,TaskArea::Build];
        let a=input(&candidates,&tasks,Some(query.clone()));
        if !tasks.is_empty() { let len=tasks.len(); tasks.rotate_left(rotate%len); }
        tasks.reverse();candidates.reverse();query.areas.reverse();
        for t in &mut tasks {t.tags.as_mut().unwrap().areas.reverse();t.usage.reverse();}
        let b=input(&candidates,&tasks,Some(query));
        prop_assert_eq!(serde_json::to_vec(&a.evidence).unwrap(),serde_json::to_vec(&b.evidence).unwrap());
        prop_assert_eq!(serde_json::to_vec(&decide(&a)).unwrap(),serde_json::to_vec(&decide(&b)).unwrap());
    }

    #[test]
    fn identical_evidence_never_pays_for_a_statistical_upgrade(n in 10i64..200, points in 0i64..101) {
        let candidates=equal_candidates();let mut i=input(&candidates,&[],Some(tags()));
        for m in &mut i.evidence.models {
            m.global=stats(n,points);m.kind=stats(n,points);
            m.candidate.sheet.as_mut().unwrap().benchmark=None;
        }
        let r=decide(&i);
        prop_assert_eq!(r.rule,Rule::CheapestApt);
        prop_assert_eq!(r.recommended.as_deref(),Some("claude/competitor"));
    }

    #[test]
    fn a_candidate_dominating_quality_and_cost_wins_with_equal_priors_and_steps(
        low in 0i64..100, improvement in 1i64..101,n in 10i64..100
    ) {
        let mut i=threshold_input(0,0,n);
        for m in &mut i.evidence.models {
            let better=m.candidate.id=="competitor";
            m.kind=stats(n,if better {(low+improvement).min(100)} else {low});
            m.usd_per_mtok=if better {[1.0;4]} else {[2.0;4]};
        }
        let result=decide(&i);
        prop_assert_eq!(result.recommended.as_deref(),Some("claude/competitor"));
    }

    #[test]
    fn ordinary_choice_is_eligible_apt_and_has_a_permitted_reason(
        margin_a in any::<bool>(), margin_b in any::<bool>(),points in 0i64..101,n in 0i64..30
    ) {
        let mut candidates=equal_candidates();
        candidates[0].with_margin=margin_a;candidates[1].with_margin=margin_b;
        candidates[0].sheet.as_mut().unwrap().good_for.clear();
        let mut i=input(&candidates,&[],Some(tags()));
        for m in &mut i.evidence.models { m.kind=stats(n,points); }
        let r=decide(&i);
        let selected=r.ranking.iter().find(|row|Some(&row.model)==r.recommended.as_ref()).unwrap();
        prop_assert!(selected.eligible);
        prop_assert!(selected.apt);
        prop_assert!(matches!(r.rule,Rule::CheapestApt|Rule::ClearlyBetter|Rule::OnlyOne|Rule::NoMargin));
        if margin_a || margin_b {
            let id=r.recommended.as_ref().unwrap();
            prop_assert!(candidates.iter().any(|c|c.with_margin && &c.wire_id()==id));
        }
    }

    #[test]
    fn high_impact_always_picks_at_least_the_normal_choices_quality(
        first in 0i64..101,second in 0i64..101,n in 0i64..60,transition in 0usize..4,
        area_scores in proptest::array::uniform4(0i64..101)
    ) {
        let mut i=input(&equal_candidates(),&[],Some(tags()));
        for m in &mut i.evidence.models {
            let p=if m.candidate.id=="sonnet"{first}else{second};
            m.global=stats(n,p);m.kind=stats(n,p);
            for (area,points) in [TaskArea::Ui,TaskArea::Daemon,TaskArea::Build,TaskArea::Data].into_iter().zip(area_scores) {
                m.areas.insert(area,stats(n,if m.candidate.id=="sonnet" {points} else {100-points}));
            }
        }
        match transition {
            0=>i.tags.as_mut().unwrap().risks.push(TaskRisk::Security),
            1=>i.tags.as_mut().unwrap().size=Some(TaskSize::Large),
            2=>i.tags.as_mut().unwrap().areas=vec![TaskArea::Ui,TaskArea::Daemon,TaskArea::Build],
            _=>i.tags.as_mut().unwrap().areas.push(TaskArea::Data),
        }
        let high=decide(&i);
        let mut normal_input=i.clone();
        normal_input.tags.as_mut().unwrap().risks.clear();
        normal_input.tags.as_mut().unwrap().size=None;
        normal_input.rules.broad_area_count=usize::MAX;
        normal_input.rules.broad_areas.clear();
        let normal=decide(&normal_input);
        prop_assert_eq!(high.rule,Rule::HighImpact);
        let chosen_q=|r:&Recommendation|r.ranking.iter().find(|row|Some(&row.model)==r.recommended.as_ref()).unwrap().estimate.task().quality();
        no_less(chosen_q(&high),chosen_q(&normal));
    }
}
