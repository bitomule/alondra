//! A delivery whose PR merged is settled, and a pause of Alondra's own that nobody answers (the
//! usage limit, the resource gate) cannot hold it: the task closes once its node is over, and a
//! move of a task whose node still works after the merge moves it instead of closing it.
use super::lifecycle_tests::{assert_aggregate_rules, exec, kinds, must, node, user};
use super::move_tests::{claude, moved_and_deferred, on_codex};
use super::*;
use alondra_domain::{Capacity, Owner, TaskPhase, WaitReason};
use proptest::prelude::*;

fn limit() -> SdkApiFailure {
    SdkApiFailure { error: "rate_limit".into(), status: Some(429), resets_at: None, ..Default::default() }
}

fn pr_targets() -> Vec<DeliveryTarget> {
    vec![DeliveryTarget::PullRequest { pr_id: "pr".into(), head: "head1".into() }]
}

/// Task `t` on Codex delivered its PR, its turn ended on the usage limit, and the PR merged.
pub(super) fn merged_on_the_limit(post_merge: bool) -> DomainState {
    let mut s = on_codex();
    must(&mut s, node("a"), Command::RegisterPr {
        task_id: "t".into(), pr_id: "pr".into(), host: "github.com".into(), repository: "owner/repo".into(), number: 1,
        head: "head1".into(), base: "main".into(), allowed_methods: vec![MergeMethod::Squash],
    });
    if post_merge {
        must(&mut s, node("a"), Command::DeclarePostMergeWork { task_id: "t".into(), reason: Some("Publish the release".into()), waits_for: vec![] });
    }
    must(&mut s, node("a"), Command::PresentResult {
        checklist: vec![], attachments: vec![], task_id: "t".into(), interaction_id: "result".into(), body: "Done".into(),
        targets: pr_targets(), is_final: true, nothing_to_review: false, card: CardSpec::delivery("Review the result", &[], true),
    });
    must(&mut s, Actor::System, Command::RecordTurnBoundary { task_id: "t".into(), generation: 1, turn: "turn".into(), failure: Some(limit()), failed: None });
    must(&mut s, Actor::System, Command::ObservePrState {
        pr_id: "pr".into(), head: "head1".into(), base: "main".into(), state: PrState::Merged, merged_head: Some("head1".into()),
    });
    let lifecycle = s.lifecycle_of(&"t".into()).unwrap();
    assert_eq!(lifecycle.phase(), TaskPhase::Delivered);
    assert!(matches!(lifecycle.cards().as_slice(), [(_, WaitReason::Pause)]), "the usage limit holds the delivered task: {:?}", lifecycle.cards());
    assert_eq!(lifecycle.card_owner(lifecycle.cards()[0].0), Some(Owner::Capacity(Capacity::Usage)));
    s
}

/// 2026-11-14: a task whose PR merged while the usage limit held it could not close. Each time
/// Alondra recorded the end of its node, the task's end was refused, and the close tried again
/// every five minutes for hours.
#[test]
fn a_merged_delivery_the_usage_limit_held_closes_when_its_node_ends() {
    let mut s = merged_on_the_limit(false);
    must(&mut s, Actor::System, Command::RecordNodeEnd { task_id: "t".into(), end: NodeEnd::Closed, agent_id: Some("a".into()) });
    assert_eq!(s.tasks[&TaskId::from("t")].state, TaskState::Done);
    let journal = kinds(&s, "t");
    let tail: Vec<&str> = journal.iter().rev().take(4).map(String::as_str).collect();
    assert_eq!(tail, ["drained", "finished", "accepted", "pause_ended"], "the hold ends, then acceptance and end, in one decision");
    assert!(s.claude_held().is_empty(), "nothing waits for usage on a closed task");
    assert_aggregate_rules(&s);
}

/// 2026-11-14: moving to another account a task whose PR merged and whose node still had work to
/// do after the merge was aborted: the move let go of its node, the task read as finished, and its
/// end was refused. The move goes through, and the work after the merge stays the task's.
#[test]
fn a_merged_delivery_with_work_after_the_merge_moves_and_keeps_that_work() {
    let mut s = merged_on_the_limit(true);
    moved_and_deferred(&mut s);
    let t = &s.tasks[&TaskId::from("t")];
    assert!(t.agent_id.is_none(), "the move let go of its node");
    assert!(t.pending_selection.is_none(), "the move finished");
    assert!(t.wrap_up.post_merge_work.is_some(), "the work after the merge is still owed");
    assert_eq!(t.ending, None, "nothing closes it");
    let lifecycle = s.lifecycle_of(&"t".into()).unwrap();
    assert_eq!(lifecycle.phase(), TaskPhase::Delivered);
    assert!(lifecycle.post_merge().is_some());
    assert_eq!(t.selection.as_ref().and_then(|selection| selection.account.clone()), Some(claude()), "its next node works on the account it moved to");
    assert_aggregate_rules(&s);
}

#[derive(Clone, Debug)]
enum Act {
    RegisterPr,
    DeclarePostMerge,
    Present { pr: bool },
    StartTurn,
    EndTurn { on_the_limit: bool },
    Pause { by_user: bool },
    Resume,
    Merge,
    Accept,
    NodeEnd { gone: bool },
    Move,
}

fn act() -> impl Strategy<Value = Act> {
    prop_oneof![
        1 => Just(Act::RegisterPr),
        1 => Just(Act::DeclarePostMerge),
        2 => any::<bool>().prop_map(|pr| Act::Present { pr }),
        2 => Just(Act::StartTurn),
        3 => any::<bool>().prop_map(|on_the_limit| Act::EndTurn { on_the_limit }),
        1 => any::<bool>().prop_map(|by_user| Act::Pause { by_user }),
        1 => Just(Act::Resume),
        2 => Just(Act::Merge),
        1 => Just(Act::Accept),
        1 => any::<bool>().prop_map(|gone| Act::NodeEnd { gone }),
        1 => Just(Act::Move),
    ]
}

struct World {
    s: DomainState,
    turn: Option<String>,
}

impl World {
    /// Runs `command`. A refusal is an answer, except the lifecycle refusing a command that may
    /// settle the delivery: the merge, the acceptance, the end of the node and the move.
    fn run(&mut self, actor: Actor, command: Command) -> Result<bool, String> {
        let settles = matches!(command, Command::ObservePrState { .. } | Command::AcceptDelivery { .. } | Command::RecordNodeEnd { .. } | Command::DeferTaskMove { .. });
        let shown = format!("{command:?}");
        match exec(&mut self.s, actor, command) {
            Ok(_) => Ok(true),
            Err(error) if settles && error.message.starts_with("The lifecycle of task") => Err(format!("{shown} was refused by the lifecycle: {}", error.message)),
            Err(_) => Ok(false),
        }
    }

    fn perform(&mut self, act: &Act, n: usize) -> Result<(), String> {
        let task = || TaskId::from("t");
        match act {
            Act::RegisterPr => drop(self.run(node("a"), Command::RegisterPr {
                task_id: task(), pr_id: "pr".into(), host: "github.com".into(), repository: "owner/repo".into(), number: 1,
                head: "head1".into(), base: "main".into(), allowed_methods: vec![MergeMethod::Squash],
            })?),
            Act::DeclarePostMerge => drop(self.run(node("a"), Command::DeclarePostMergeWork { task_id: task(), reason: Some("Publish the release".into()), waits_for: vec![] })?),
            Act::Present { pr } => {
                let targets = if *pr { pr_targets() } else { vec![DeliveryTarget::Text { text: "Done".into() }] };
                drop(self.run(node("a"), Command::PresentResult {
                    checklist: vec![], attachments: vec![], task_id: task(), interaction_id: format!("result-{n}").into(), body: "Done".into(),
                    targets, is_final: true, nothing_to_review: false, card: CardSpec::delivery("Review the result", &[], true),
                })?);
            }
            Act::StartTurn if self.turn.is_none() => {
                let turn = format!("turn-{n}");
                let pending = self.s.lifecycle_of(&task()).ok().and_then(|l| l.pending_inputs().first().map(|input| input.as_str().to_owned()));
                let fact = match pending {
                    Some(input) => RuntimeObservation::TurnStarted { turn: turn.clone(), input },
                    None => RuntimeObservation::TurnUnprompted { turn: turn.clone() },
                };
                if self.run(Actor::System, Command::RecordRuntimeFacts { task_id: task(), generation: 1, facts: vec![fact] })? {
                    self.turn = Some(turn);
                }
            }
            Act::StartTurn => {}
            Act::EndTurn { on_the_limit } => if let Some(turn) = self.turn.clone() {
                let failure = on_the_limit.then(limit);
                if self.run(Actor::System, Command::RecordTurnBoundary { task_id: task(), generation: 1, turn, failure, failed: None })? {
                    self.turn = None;
                }
            },
            Act::Pause { by_user } => {
                let reason = if *by_user { PauseReason::ByUser } else { PauseReason::Disk };
                let pause = Pause { reason, since: n as Millis, cut: None, turn_since: None, cleanup: PauseCleanup::Done, released: false };
                drop(self.run(Actor::System, Command::MoveNode { agent_id: "a".into(), event: NodeEvent::Paused { pause } })?);
            }
            Act::Resume => drop(self.run(Actor::System, Command::MoveNode { agent_id: "a".into(), event: NodeEvent::Resumed })?),
            Act::Merge => drop(self.run(Actor::System, Command::ObservePrState {
                pr_id: "pr".into(), head: "head1".into(), base: "main".into(), state: PrState::Merged, merged_head: Some("head1".into()),
            })?),
            Act::Accept => if let Some(delivery) = self.s.tasks[&task()].final_presentation.clone()
                && let Some(InteractionPayload::Delivery { targets, .. }) = self.s.interactions.get(&delivery).map(|card| card.payload.clone())
            {
                drop(self.run(user(), Command::AcceptDelivery { presentation_id: delivery, targets })?);
            },
            Act::NodeEnd { gone } => {
                let end = if *gone { NodeEnd::Gone } else { NodeEnd::Closed };
                if self.run(Actor::System, Command::RecordNodeEnd { task_id: task(), end, agent_id: Some("a".into()) })? {
                    self.turn = None;
                }
            }
            Act::Move => {
                let Some(stamp) = self.s.task_selection_stamp(&task()) else { return Ok(()) };
                if !self.run(user(), Command::MoveTask { task_id: task(), expected_selection: stamp, account: claude() })? {
                    return Ok(());
                }
                if self.s.lifecycle_of(&task()).is_ok_and(|l| l.execution().is_some())
                    && self.run(Actor::System, Command::RecordHostEnded { task_id: task(), generation: 1, resumes: false, by_alondra: true, ended_work: vec![] })?
                {
                    self.turn = None;
                }
                if self.s.tasks[&task()].pending_selection.is_some()
                    && !self.run(Actor::System, Command::DeferTaskMove { task_id: task(), account: claude() })?
                {
                    return Err("the move of a task whose host ended was refused".into());
                }
            }
        }
        Ok(())
    }

    /// Delivered implies a live delivery: the task's own final delivery, still standing, that
    /// something still holds (the user's review, its node, work after its merge, a question).
    fn holds(&self) -> Result<(), String> {
        let t = &self.s.tasks[&TaskId::from("t")];
        let Ok(lifecycle) = self.s.lifecycle_of(&t.id) else { return Ok(()) };
        if lifecycle.phase() != TaskPhase::Delivered {
            return Ok(());
        }
        let delivery = lifecycle.delivery().expect("a delivered task has its delivery");
        if t.final_presentation.as_ref() != Some(delivery) {
            return Err(format!("delivered {delivery:?} while the task's final delivery is {:?}", t.final_presentation));
        }
        if self.s.interactions.get(delivery).is_none_or(|card| card.withdrawn) {
            return Err(format!("delivered {delivery:?}, which is withdrawn or gone"));
        }
        let unheld = self.s.finished(t) && !self.s.unanswered(&t.id) && !self.s.blockers(&t.id) && !self.s.wraps_up(t)
            && !self.s.works_after_merge(t) && self.s.checklist_owed(t).is_empty();
        if unheld {
            return Err(format!("delivered {delivery:?}, settled, with nothing that holds it: {:?}", lifecycle.cards()));
        }
        Ok(())
    }
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 512, failure_persistence: None, ..ProptestConfig::default() })]

    #[test]
    fn a_delivered_task_always_has_a_live_delivery_and_its_end_or_move_is_never_refused(acts in prop::collection::vec(act(), 1..40)) {
        let mut world = World { s: on_codex(), turn: Some("turn".into()) };
        for (n, act) in acts.iter().enumerate() {
            if let Err(why) = world.perform(act, n + 2).and_then(|()| world.holds()) {
                prop_assert!(false, "after {:?} (step {}) in {:?}: {}", act, n + 2, acts, why);
            }
        }
    }
}
