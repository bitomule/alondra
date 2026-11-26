//! A message the user sends to a task never takes back a merge they authorized, and a merge they
//! press while the task still works on a message of theirs asks first instead of being refused.
use super::*;
use alondra_domain_model::{Actor, Command, DeliveryTarget, DomainError, Instruction, MergeMethod, Refusal, TaskId};
use proptest::prelude::*;
use alondra_domain_rules::Apply as _;

const MESSAGE: &str = "Rename the button too";

fn user() -> Actor {
    Actor::User { user_id: "u".into() }
}

fn run(state: &mut DomainState, actor: Actor, command: Command) -> Result<(), DomainError> {
    let envelope = alondra_domain_model::CommandEnvelope {
        protocol_version: 2, data_epoch: state.data_epoch.clone(),
        command_id: format!("c-{}", state.revision).into(), expected_revision: state.revision, command,
    };
    state.seed_mergeable_for_presented(&envelope.command);
    state.execute(&actor, &envelope).map(drop)
}

fn told_by_user(state: &mut DomainState, text: &str) -> Result<(), DomainError> {
    run(state, Actor::System, Command::TellNode { task_id: "t".into(), text: text.into(), from_user: true, answers: None })
}

fn authorize(id: &str, presentation: &str, despite_pending_message: bool) -> Command {
    Command::AuthorizeMerge { authorization_id: id.into(), presentation_id: presentation.into(), pr_id: "pr".into(),
        expected_head: "head1".into(), method: MergeMethod::Squash, despite_pending_message }
}

fn claim(id: &str) -> Command {
    Command::ClaimMergeAuthorization { authorization_id: id.into(), remote_head: "head1".into(), remote_base: "main".into(), base_verified: true }
}

fn merged() -> Command {
    Command::ObservePrState { pr_id: "pr".into(), head: "head1".into(), base: "main".into(), state: PrState::Merged, merged_head: Some("head1".into()) }
}

fn feedback(presentation: &str, instruction: Instruction) -> Command {
    Command::SubmitFeedback { attachments: vec![], presentation_id: presentation.into(), body: MESSAGE.into(), instruction, merge_failure: None,
        targets: vec![DeliveryTarget::PullRequest { pr_id: "pr".into(), head: "head1".into() }] }
}

fn told_texts(state: &DomainState) -> Vec<String> {
    state.lifecycle_journal[&TaskId::from("t")].iter().filter_map(|record| match record {
        alondra_domain_model::LifecycleRecord::Told { text: Some(text), .. } => Some(text.clone()),
        _ => None,
    }).collect()
}

fn merge_action(view: &View) -> Option<Action> {
    let card = card_of(view);
    card.actions.iter().chain(&card.more).find(|action| action.id.starts_with("merge:")).map(|action| Action::clone(action))
}

/// 2026-11-14 23:29: the user authorized the merge of a PR and a plain message to its task
/// arrived 0.6 s later. The merge step was refused, the authorization was lost without a word and
/// the delivery stayed open with no merge button until its node presented the same PR again.
#[test]
fn a_message_right_after_authorizing_a_merge_never_takes_the_merge_back() {
    let mut state = presented_pr();
    run(&mut state, user(), authorize("auth", "d", false)).unwrap();
    told_by_user(&mut state, MESSAGE).unwrap();
    assert!(state.merge_authorizations[&"auth".into()].valid, "the message does not take the authorization back");
    run(&mut state, Actor::System, claim("auth")).expect("Alondra merges what the user authorized");
    run(&mut state, Actor::System, merged()).unwrap();
    assert_eq!(state.pull_requests[&"pr".into()].state, PrState::Merged);
    assert!(told_texts(&state).iter().any(|text| text == MESSAGE), "the node still gets the message: {:?}", told_texts(&state));
    let task = &state.tasks[&TaskId::from("t")];
    assert!(!task.pending_user_inputs.is_empty(), "the message is still owed an answer after the merge");
    assert_ne!(task.state, alondra_domain_model::TaskState::Done, "the task does not close over an unanswered message");
}

/// Pressing merge while the task still works on a message of the user asks first; confirmed, the
/// domain records that the user merged anyway.
#[test]
fn merging_while_a_message_is_unanswered_asks_first_and_records_the_override() {
    let mut state = presented_pr();
    told_by_user(&mut state, MESSAGE).unwrap();
    let before = state.clone();
    let refused = run(&mut state, user(), authorize("auth", "d", false)).unwrap_err();
    assert_eq!((refused.code.as_str(), refused.why()), ("unanswered_message", Refusal::UnansweredMessage));
    assert_eq!(state, before, "a merge the user did not confirm changes nothing");

    let mut live = LiveSnapshot::default();
    live.message_times.insert("d".into(), "2026-11-14T23:29:40".into());
    for client in [Client::Ios, Client::Mac, Client::Tui] {
        for (lang, cancel, line) in [(Lang::Es, "Esperar", "Atendiendo tu mensaje de las 23:29"), (Lang::En, "Wait", "Working on your message from 23:29")] {
            let view = render(&state, &live, client, lang, "card:d", "");
            let merge = merge_action(&view).unwrap_or_else(|| panic!("{client:?} {lang:?}: the merge stays offered"));
            assert!(merge.id.contains(crate::action_id::MERGE_ANYWAY), "{client:?}: the confirmed merge says the user knew");
            let confirm = merge.confirm.unwrap_or_else(|| panic!("{client:?} {lang:?}: the merge asks first"));
            assert_eq!(confirm.cancel.as_deref(), Some(cancel));
            let why = card_of(&view).why.clone().unwrap_or_default();
            assert!(why.starts_with(line), "{client:?} {lang:?}: {why}");
        }
    }
    run(&mut state, user(), authorize("auth", "d", true)).unwrap();
    assert!(state.merge_authorizations[&"auth".into()].despite_pending_message, "the override is recorded");
}

/// A merge pressed with nothing unanswered asks nothing and records no override.
#[test]
fn merging_with_nothing_unanswered_asks_nothing() {
    let mut state = presented_pr();
    let view = render(&state, &LiveSnapshot::default(), Client::Ios, Lang::Es, "card:d", "");
    let merge = merge_action(&view).expect("the merge is offered");
    assert!(merge.confirm.is_none() && !merge.id.contains(crate::action_id::MERGE_ANYWAY));
    run(&mut state, user(), authorize("auth", "d", true)).unwrap();
    assert!(!state.merge_authorizations[&"auth".into()].despite_pending_message, "nothing was overridden");
}

/// «Request changes» after authorizing is still the user changing their mind about that PR.
#[test]
fn a_change_request_after_authorizing_still_stops_the_merge() {
    let mut state = presented_pr();
    run(&mut state, user(), authorize("auth", "d", false)).unwrap();
    run(&mut state, user(), feedback("d", Instruction::Changes)).unwrap();
    assert!(!state.merge_authorizations[&"auth".into()].valid, "the change request takes the merge back");
    assert!(run(&mut state, Actor::System, claim("auth")).is_err());
}

#[derive(Clone, Debug)]
enum Act {
    Tell,
    Reply,
    Changes,
    Authorize { despite: bool },
    Claim,
    Merged,
    Present,
    TurnStarts,
    TurnEnds,
    Pause,
}

fn act() -> impl Strategy<Value = Act> {
    prop_oneof![
        3 => Just(Act::Tell),
        2 => Just(Act::Reply),
        1 => Just(Act::Changes),
        3 => any::<bool>().prop_map(|despite| Act::Authorize { despite }),
        2 => Just(Act::Claim),
        1 => Just(Act::Merged),
        2 => Just(Act::Present),
        2 => Just(Act::TurnStarts),
        2 => Just(Act::TurnEnds),
        1 => Just(Act::Pause),
    ]
}

struct World {
    state: DomainState,
    authorizations: usize,
}

impl World {
    fn presentation(&self) -> String {
        self.state.tasks[&TaskId::from("t")].final_presentation.as_ref().map_or_else(|| "d".into(), |id| id.0.clone())
    }

    fn perform(&mut self, act: &Act, n: usize) {
        let presentation = self.presentation();
        let state = &mut self.state;
        let _ = match act {
            Act::Tell => told_by_user(state, MESSAGE),
            Act::Reply => run(state, user(), feedback(&presentation, Instruction::Reply)),
            Act::Changes => run(state, user(), feedback(&presentation, Instruction::Changes)),
            Act::Authorize { despite } => {
                self.authorizations += 1;
                run(state, user(), authorize(&format!("auth-{}", self.authorizations), &presentation, *despite))
            }
            Act::Claim => run(state, Actor::System, claim(&format!("auth-{}", self.authorizations))),
            Act::Merged => run(state, Actor::System, merged()),
            Act::Present => run(state, Actor::Agent { agent_id: "a".into() }, Command::PresentResult {
                checklist: vec![], attachments: vec![], task_id: "t".into(), interaction_id: format!("d-{n}").into(), body: "Ready".into(),
                targets: vec![DeliveryTarget::PullRequest { pr_id: "pr".into(), head: "head1".into() }], is_final: true,
                nothing_to_review: false, card: alondra_domain_model::CardSpec::delivery("Review the result", &[], true),
            }),
            Act::TurnStarts => {
                let lifecycle = state.lifecycle_of(&"t".into()).ok();
                let start = lifecycle.as_ref().and_then(|lifecycle| Some((lifecycle.pending_inputs().first()?.as_str().to_owned(), lifecycle.execution()?.generation())));
                match start {
                    Some((input, generation)) => run(state, Actor::System, Command::RecordRuntimeFacts { task_id: "t".into(), generation,
                        facts: vec![alondra_domain_model::RuntimeObservation::ResourceAvailable, alondra_domain_model::RuntimeObservation::TurnStarted { turn: format!("turn-{n}"), input }] }),
                    None => Ok(()),
                }
            }
            Act::TurnEnds => {
                let lifecycle = state.lifecycle_of(&"t".into()).ok();
                let end = lifecycle.as_ref().and_then(|lifecycle| Some((lifecycle.turn()?.as_str().to_owned(), lifecycle.execution()?.generation())));
                match end {
                    Some((turn, generation)) => run(state, Actor::System, Command::RecordTurnBoundary { task_id: "t".into(), generation, turn, failure: None, failed: None }),
                    None => Ok(()),
                }
            }
            Act::Pause => {
                let pause = alondra_domain_model::Pause { reason: alondra_domain_model::PauseReason::Memory, since: 2, cut: None, turn_since: Some(1),
                    cleanup: Default::default(), released: false };
                run(state, Actor::System, Command::MoveNode { agent_id: "a".into(), event: alondra_domain_model::NodeEvent::Paused { pause } })
            }
        };
    }

    /// Every open delivery card of a PR offers its merge or says why it cannot, on every client
    /// and in both languages.
    fn every_open_delivery_offers_or_explains(&self) -> Result<(), String> {
        let index = alondra_domain_model::ReadIndex::build(&self.state);
        let live = LiveSnapshot::default();
        for card in index.cards().iter().filter(|card| card.pending && card.kind == "delivery") {
            let alondra_domain_model::InboxSubject::Interaction { interaction_id, .. } = &card.subject;
            if self.state.pull_requests[&"pr".into()].state != PrState::Open {
                continue;
            }
            for client in [Client::Ios, Client::Mac, Client::Tui] {
                for lang in [Lang::Es, Lang::En] {
                    let view = render(&self.state, &live, client, lang, &format!("card:{interaction_id}"), "");
                    if view.view != "card" {
                        continue;
                    }
                    let shown = card_of(&view);
                    let merge = merge_action(&view);
                    let explained = shown.context.iter().any(|line| line.icon == Icon::Warning) || !shown.blocked_by.is_empty()
                        || merge.as_ref().is_some_and(|merge| !merge.enabled && merge.disabled_reason.is_some());
                    if merge.is_none() && !explained {
                        return Err(format!("{client:?} {lang:?}: {interaction_id} offers no merge and says nothing about why: {shown:?}"));
                    }
                }
            }
        }
        Ok(())
    }
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 256, failure_persistence: None, ..ProptestConfig::default() })]

    #[test]
    fn an_open_delivery_always_offers_its_merge_or_says_why_and_no_message_takes_a_merge_back(acts in prop::collection::vec(act(), 1..30)) {
        let mut world = World { state: presented_pr(), authorizations: 0 };
        for (n, act) in acts.iter().enumerate() {
            let held: Vec<_> = world.state.merge_authorizations.values().filter(|a| a.valid).map(|a| a.id.clone()).collect();
            world.perform(act, n);
            if matches!(act, Act::Tell | Act::Reply) {
                for id in &held {
                    prop_assert!(world.state.merge_authorizations[id].valid, "after {:?} (step {}) in {:?}: {:?} was taken back", act, n, acts, id);
                }
            }
            if let Err(why) = world.every_open_delivery_offers_or_explains() {
                prop_assert!(false, "after {:?} (step {}) in {:?}: {}", act, n, acts, why);
            }
        }
    }
}

/// The card of a PR whose task still works on a message of the user, as the apps draw it.
#[test]
fn the_card_of_a_pr_with_an_unanswered_message_keeps_its_merge_and_says_what_the_task_does() {
    let mut state = presented_pr();
    told_by_user(&mut state, MESSAGE).unwrap();
    let mut live = LiveSnapshot::default();
    live.message_times.insert("d".into(), "2026-11-14T23:29:40".into());
    for (filename, client, lang, expected) in [
        ("card-merge-unanswered.json", Client::Ios, Lang::En, include_str!("../../../fixtures/views/cards/card-merge-unanswered.json")),
        ("card-merge-unanswered-es.json", Client::Ios, Lang::Es, include_str!("../../../fixtures/views/cards/card-merge-unanswered-es.json")),
        ("card-merge-unanswered-mac.json", Client::Mac, Lang::En, include_str!("../../../fixtures/views/cards/card-merge-unanswered-mac.json")),
        ("card-merge-unanswered-es-mac.json", Client::Mac, Lang::Es, include_str!("../../../fixtures/views/cards/card-merge-unanswered-es-mac.json")),
        ("card-merge-unanswered-tui.json", Client::Tui, Lang::En, include_str!("../../../fixtures/views/cards/card-merge-unanswered-tui.json")),
        ("card-merge-unanswered-es-tui.json", Client::Tui, Lang::Es, include_str!("../../../fixtures/views/cards/card-merge-unanswered-es-tui.json")),
    ] {
        assert_golden(filename, &render(&state, &live, client, lang, "card:d", ""), expected);
    }
}
