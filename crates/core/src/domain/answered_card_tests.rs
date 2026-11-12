//! A node that asked the user after presenting its delivery owes the step the answer asks for.
//! 3 November, beta-go: the node presented, declared its work after the merge, asked the user
//! whether to publish the beta and ended its turn on that card. The user answered, the answer
//! reached the node, and its turn ended with nothing new: in a delivery a wake was refused, so
//! nothing woke it and no case reached the orchestrator, and it sat idle for 2 h 10 min.
//! Resting on such an answer wakes the node with it once; resting again is a case for the
//! orchestrator, never a card for the user, who still sees only the delivery.
use super::lifecycle_tests::{assert_aggregate_rules, exec, last, must, node, sdk_turn, user, world};
use super::*;
use proptest::prelude::*;
use std::collections::BTreeMap;

const SAID: &str = "Publish it to the testers";

fn orchestrator() -> Actor {
    Actor::Orchestrator { generation: 0 }
}
fn lifecycle(s: &DomainState) -> std::sync::Arc<alondra_domain::TaskLifecycle> {
    s.lifecycle_of(&"t".into()).unwrap()
}
fn end_empty(s: &mut DomainState, turn: &str) -> CommandOutcome {
    must(s, Actor::System, Command::RecordTurnBoundary { task_id: "t".into(), generation: 1, turn: turn.into(), failure: None, failed: None })
}
/// Starts `turn` with the first input waiting for the node.
fn start(s: &mut DomainState, turn: &str) {
    let input = lifecycle(s).pending_inputs().first().map(|input| input.as_str().to_owned()).expect("something to start the turn with");
    must(s, Actor::System, Command::RecordRuntimeFacts { task_id: "t".into(), generation: 1, facts: vec![
        RuntimeObservation::TurnStarted { turn: turn.into(), input },
    ] });
}
fn present(s: &mut DomainState) {
    must(s, node("a"), Command::PresentResult {
        attachments: vec![], task_id: "t".into(), interaction_id: "r".into(), body: "Beta 61 is ready".into(), is_final: true, nothing_to_review: false,
        targets: vec![DeliveryTarget::Text { text: "Beta 61".into() }], checklist: vec![], card: CardSpec::delivery("Review beta 61", &[], true),
    });
}
fn ask(s: &mut DomainState, id: &str) {
    let mut card = CardSpec::new(CardKind::Decide, "Decide whether to publish the beta").why("The testers get it once it is published");
    card.blocks = vec![CardTarget::Delivery { interaction_id: "r".into() }];
    must(s, node("a"), Command::AskQuestion {
        attachments: vec![], interaction_id: id.into(), scope: Scope::Task { task_id: "t".into() }, recipient: Recipient::User,
        body: "Publish beta 61 to the testers now?".into(), blocking: true,
        options: vec![
            QuestionOption { id: "go".into(), label: "Publish".into(), description: None },
            QuestionOption { id: "hold".into(), label: "Hold it".into(), description: None },
        ],
        card,
    });
}
fn answer_text(s: &mut DomainState, id: &str) {
    must(s, user(), Command::AnswerQuestion { attachments: vec![], interaction_id: id.into(), option_id: String::new(),
        inputs: BTreeMap::from([(ANSWER_TEXT.to_owned(), InputValue::Text(SAID.into()))]) });
}
/// What the daemon's outbox does once the user answered: the node is told the answer, linked to its card.
fn deliver_answer(s: &mut DomainState, id: &str) {
    let text = format!("Answer to your card {id} (Decide whether to publish the beta): {SAID}.");
    must(s, Actor::System, Command::TellNode { task_id: "t".into(), text, from_user: true, answers: Some(id.into()) });
}
fn open_case(s: &DomainState) -> Option<InteractionId> {
    s.interactions.values().find(|i| is_stall_case(i) && s.open(i)).map(|i| i.id.clone())
}
fn pending(s: &DomainState) -> Vec<String> {
    super::read_index::inbox_items(s).into_iter().filter(|item| item.pending)
        .filter_map(|item| item.id.strip_prefix("interaction:").map(str::to_owned)).collect()
}
fn notifies_user(outcome: &CommandOutcome) -> bool {
    outcome.effects.iter().any(|e| matches!(e, DomainEffect::NotifyUser { .. }))
}
fn unfinished(s: &DomainState) -> usize {
    s.lifecycle_journal[&TaskId::from("t")].iter().filter(|record| matches!(record, LifecycleRecord::Unfinished { .. })).count()
}
/// The node, woken once on what it owes, rests again on each of its other wakes: they wake it
/// again with the same step.
fn rest_through_wakes(s: &mut DomainState) {
    for n in 2..=alondra_domain::EMPTY_TURN_WAKES {
        let turn = format!("turn-woken-{n}");
        start(s, &turn);
        end_empty(s, &turn);
        assert!(matches!(last(s, "t"), LifecycleRecord::Unfinished { wake: Some(_), .. }), "wake {n} in a row: {:?}", last(s, "t"));
    }
}
/// The node presented in `turn-1`, asked `beta-go` and ended the turn on it; the user answered
/// and the answer reached the node, which took it up in `turn-2`.
fn answer_taken() -> DomainState {
    let mut s = world();
    sdk_turn(&mut s, "t", "a");
    present(&mut s);
    ask(&mut s, "beta-go");
    end_empty(&mut s, "turn-1");
    assert_eq!(lifecycle(&s).awaits(), Some(&alondra_domain::Await::User("beta-go".into())));
    answer_text(&mut s, "beta-go");
    deliver_answer(&mut s, "beta-go");
    start(&mut s, "turn-2");
    s
}

#[test]
fn a_delivered_node_that_rests_on_the_answer_to_its_card_is_woken_with_that_answer() {
    let mut s = answer_taken();
    let ended = end_empty(&mut s, "turn-2");
    let LifecycleRecord::Unfinished { wake: Some(wake), text: Some(text), .. } = last(&s, "t") else {
        panic!("the rest on the answer wakes the node: {:?}", last(&s, "t"));
    };
    assert!(text.contains("beta-go") && text.contains(SAID) && text.contains("present_result"), "{text}");
    assert_eq!(lifecycle(&s).pending_inputs(), vec![&alondra_domain::InputId::new(wake).unwrap()], "the wake waits for the node");
    assert_eq!(lifecycle(&s).phase(), alondra_domain::TaskPhase::Delivered, "the delivery stays");
    assert!(!notifies_user(&ended) && open_case(&s).is_none(), "nobody is asked yet");
    assert_eq!(pending(&s), ["r"], "the user sees only the delivery");
    assert_aggregate_rules(&s);
}

#[test]
fn an_answer_taken_after_a_delivery_that_did_not_read_it_is_woken_when_the_node_rests_on_it() {
    let mut s = world();
    sdk_turn(&mut s, "t", "a");
    present(&mut s);
    must(&mut s, Actor::System, Command::TellNode { task_id: "t".into(), text: "Keep going".into(), from_user: true, answers: None });
    ask(&mut s, "beta-go");
    end_empty(&mut s, "turn-1");
    answer_text(&mut s, "beta-go");
    deliver_answer(&mut s, "beta-go");
    let keep_going = lifecycle(&s).pending_inputs()[0].as_str().to_owned();
    must(&mut s, Actor::System, Command::RecordRuntimeFacts { task_id: "t".into(), generation: 1, facts: vec![
        RuntimeObservation::TurnStarted { turn: "turn-2".into(), input: keep_going },
    ] });
    must(&mut s, node("a"), Command::PresentResult {
        attachments: vec![], task_id: "t".into(), interaction_id: "r2".into(), body: "Beta 62 is ready".into(), is_final: true, nothing_to_review: false,
        targets: vec![DeliveryTarget::Text { text: "Beta 62".into() }], checklist: vec![], card: CardSpec::delivery("Review beta 62", &[], true),
    });
    end_empty(&mut s, "turn-2");
    start(&mut s, "turn-3");
    end_empty(&mut s, "turn-3");
    assert!(matches!(last(&s, "t"), LifecycleRecord::Unfinished { wake: Some(_), .. }), "woken with the answer: {:?}", last(&s, "t"));
    assert_aggregate_rules(&s);
}

#[test]
fn the_rest_after_the_wakes_goes_to_the_orchestrator_never_the_user() {
    let mut s = answer_taken();
    end_empty(&mut s, "turn-2");
    rest_through_wakes(&mut s);
    start(&mut s, "turn-3");
    let ended = end_empty(&mut s, "turn-3");
    assert!(matches!(last(&s, "t"), LifecycleRecord::Unfinished { wake: None, .. }), "no fourth wake: {:?}", last(&s, "t"));
    let case = open_case(&s).expect("the rest after the wakes is a case");
    assert_eq!(case, stalled_card_id(&"t".into(), 1, "turn-3"));
    assert!(ended.effects.iter().any(|e| matches!(e, DomainEffect::NotifyOrchestrator { interaction_id: Some(id), .. } if *id == case)), "the orchestrator hears of it");
    assert!(!notifies_user(&ended), "the user hears of nothing");
    let body = &s.interactions[&case].body;
    assert!(body.contains("beta-go") && body.contains(SAID) && body.contains("delivery"), "{body}");
    assert_eq!(pending(&s), ["r"], "the user sees only the delivery");
    assert_eq!(lifecycle(&s).live(), alondra_domain::Several::new([InteractionId::from("r")]).map(alondra_domain::LiveState::WaitingUser));
    assert_aggregate_rules(&s);
}

#[test]
fn a_post_merge_turn_that_rests_with_its_work_owed_is_woken() {
    let mut s = world();
    sdk_turn(&mut s, "t", "a");
    let targets = || vec![DeliveryTarget::PullRequest { pr_id: "pr".into(), head: "head1".into() }];
    must(&mut s, node("a"), Command::RegisterPr {
        task_id: "t".into(), pr_id: "pr".into(), host: "github.com".into(), repository: "owner/repo".into(), number: 1,
        head: "head1".into(), base: "main".into(), allowed_methods: vec![MergeMethod::Squash],
    });
    must(&mut s, node("a"), Command::DeclarePostMergeWork { task_id: "t".into(), reason: Some("publish the release".into()), waits_for: vec![] });
    must(&mut s, node("a"), Command::PresentResult {
        checklist: Vec::new(), attachments: vec![], task_id: "t".into(), interaction_id: "final".into(), body: "Done".into(),
        targets: targets(), is_final: true, nothing_to_review: false, card: CardSpec::delivery("Review the result", &[], true),
    });
    must(&mut s, user(), Command::AcceptDelivery { presentation_id: "final".into(), targets: targets() });
    must(&mut s, Actor::System, Command::ObservePrState {
        pr_id: "pr".into(), head: "head1".into(), base: "main".into(), state: PrState::Merged, merged_head: Some("head1".into()),
    });
    end_empty(&mut s, "turn-1");
    assert_eq!(unfinished(&s), 0, "the turn that presented is not the work after the merge");
    must(&mut s, Actor::System, Command::TellNode { task_id: "t".into(), text: "Your PR merged: publish the release.".into(), from_user: false, answers: None });
    start(&mut s, "turn-2");
    let ended = end_empty(&mut s, "turn-2");
    let LifecycleRecord::Unfinished { wake: Some(_), text: Some(text), .. } = last(&s, "t") else {
        panic!("the rest with the work owed wakes the node: {:?}", last(&s, "t"));
    };
    assert_eq!(text, EmptyTurn::AfterMerge { reason: "publish the release".into() }.wake_text());
    assert!(text.contains("publish the release") && text.contains("cleanup_done"), "{text}");
    assert!(!notifies_user(&ended) && pending(&s).is_empty(), "{:?}", pending(&s));
    rest_through_wakes(&mut s);
    start(&mut s, "turn-3");
    let ended = end_empty(&mut s, "turn-3");
    let case = open_case(&s).expect("resting again is a case");
    assert!(s.interactions[&case].body.contains("publish the release"), "{}", s.interactions[&case].body);
    assert!(!notifies_user(&ended) && pending(&s).is_empty(), "{:?}", pending(&s));
    assert_aggregate_rules(&s);
}

#[test]
fn a_delivery_resting_after_presenting_is_left_to_the_user() {
    let mut s = world();
    sdk_turn(&mut s, "t", "a");
    present(&mut s);
    end_empty(&mut s, "turn-1");
    must(&mut s, Actor::System, Command::TellNode { task_id: "t".into(), text: "Looks good, I am reviewing it".into(), from_user: true, answers: None });
    start(&mut s, "turn-2");
    end_empty(&mut s, "turn-2");
    assert_eq!(unfinished(&s), 0, "{:?}", s.lifecycle_journal[&TaskId::from("t")]);
    assert!(lifecycle(&s).pending_inputs().is_empty() && open_case(&s).is_none());
    assert_eq!(pending(&s), ["r"]);
    assert_aggregate_rules(&s);
}

#[test]
fn the_users_word_after_the_answer_ends_the_owed_step() {
    let mut s = answer_taken();
    end_empty(&mut s, "turn-2");
    rest_through_wakes(&mut s);
    start(&mut s, "turn-3");
    end_empty(&mut s, "turn-3");
    let case = open_case(&s).expect("the rest after the wakes is a case");
    must(&mut s, Actor::System, Command::TellNode { task_id: "t".into(), text: "Never mind, keep it as it is".into(), from_user: true, answers: None });
    assert!(s.interactions[&case].withdrawn, "what the user wrote settles the case");
    start(&mut s, "turn-4");
    end_empty(&mut s, "turn-4");
    assert_eq!(unfinished(&s), 1 + usize::from(alondra_domain::EMPTY_TURN_WAKES), "nothing is owed after the user's word");
    assert!(open_case(&s).is_none() && lifecycle(&s).pending_inputs().is_empty());
    assert_eq!(pending(&s), ["r"]);
    assert_aggregate_rules(&s);
}

#[test]
fn alondras_unsettled_case_on_a_delivery_never_cancels_it() {
    let mut s = answer_taken();
    end_empty(&mut s, "turn-2");
    rest_through_wakes(&mut s);
    start(&mut s, "turn-3");
    end_empty(&mut s, "turn-3");
    let case = open_case(&s).expect("the rest after the wakes is a case");
    must(&mut s, Actor::System, Command::SettleStall { case_id: case, task_id: "t".into(), outcome: StallOutcome::Unsettled });
    let t = &s.tasks[&TaskId::from("t")];
    assert!(!t.over() && t.stall_closing.is_none(), "{t:?}");
    assert_eq!(lifecycle(&s).phase(), alondra_domain::TaskPhase::Delivered);
    assert!(matches!(last(&s, "t"), LifecycleRecord::Answered { text: Some(text), .. } if text == UNSETTLED_DELIVERY), "{:?}", last(&s, "t"));
    start(&mut s, "turn-4");
    end_empty(&mut s, "turn-4");
    assert_eq!(unfinished(&s), 1 + usize::from(alondra_domain::EMPTY_TURN_WAKES), "the node waits for the user now");
    assert!(open_case(&s).is_none());
    assert_eq!(pending(&s), ["r"]);
    assert_eq!(lifecycle(&s).live(), alondra_domain::Several::new([InteractionId::from("r")]).map(alondra_domain::LiveState::WaitingUser));
    assert_aggregate_rules(&s);
}

/// The beta-go journal as the store kept it before answers named their card: the told answer
/// carries no `answers`, and the delivery rests with no card. It replays as it was and reads
/// back byte for byte, and nothing Alondra derives afterwards reopens it.
#[test]
fn the_beta_go_journal_recorded_before_this_change_replays_as_it_was() {
    let mut s = world();
    sdk_turn(&mut s, "t", "a");
    must(&mut s, node("a"), Command::DeclarePostMergeWork { task_id: "t".into(), reason: Some("publish the beta".into()), waits_for: vec![] });
    present(&mut s);
    ask(&mut s, "beta-go");
    end_empty(&mut s, "turn-1");
    answer_text(&mut s, "beta-go");
    let input = s.next_input(&"t".into());
    let mut lines: Vec<String> = s.lifecycle_journal[&TaskId::from("t")].iter().map(|record| serde_json::to_string(record).unwrap()).collect();
    lines.extend([
        format!(r#"{{"kind":"told","input":"{input}","text":"{SAID}"}}"#),
        format!(r#"{{"kind":"turn_started","turn":"turn-2","input":"{input}"}}"#),
        r#"{"kind":"turn_ended","turn":"turn-2","unheard_card":"stalled-t-1-turn-2","stalled":true}"#.to_owned(),
    ]);
    let journal: Vec<LifecycleRecord> = lines.iter().map(|line| serde_json::from_str(line).expect("a journal of an earlier build decodes")).collect();
    let replayed = replay(&"t".into(), &journal).expect("it replays");
    assert_eq!(replayed.phase(), alondra_domain::TaskPhase::Delivered);
    assert_eq!(replayed.activity(), alondra_domain::ActivityView::Resting);
    assert!(replayed.cards().is_empty() && replayed.pending_inputs().is_empty(), "{replayed:?}");
    let again: Vec<String> = journal.iter().map(|record| serde_json::to_string(record).unwrap()).collect();
    assert_eq!(again, lines, "it reads back byte for byte");
    s.lifecycle_journal.insert("t".into(), journal);
    let mut s: DomainState = serde_json::from_str(&serde_json::to_string(&s).unwrap()).unwrap();
    must(&mut s, user(), Command::CreateRequest { request_id: "r2".into(), body: "Something else".into(), auto_merge: false, maintenance: false, message: false });
    assert_eq!(unfinished(&s), 0, "nothing reopens a rest recorded before");
    assert!(open_case(&s).is_none());
}

#[test]
fn the_beta_go_sequence_under_the_new_rules_wakes_then_cases() {
    let mut s = world();
    sdk_turn(&mut s, "t", "a");
    must(&mut s, node("a"), Command::DeclarePostMergeWork { task_id: "t".into(), reason: Some("publish the beta".into()), waits_for: vec![] });
    present(&mut s);
    ask(&mut s, "beta-go");
    end_empty(&mut s, "turn-1");
    s = serde_json::from_str(&serde_json::to_string(&s).unwrap()).unwrap();
    answer_text(&mut s, "beta-go");
    deliver_answer(&mut s, "beta-go");
    start(&mut s, "turn-2");
    end_empty(&mut s, "turn-2");
    assert!(matches!(last(&s, "t"), LifecycleRecord::Unfinished { wake: Some(_), .. }), "woken with the answer: {:?}", last(&s, "t"));
    s = serde_json::from_str(&serde_json::to_string(&s).unwrap()).unwrap();
    rest_through_wakes(&mut s);
    start(&mut s, "turn-3");
    let ended = end_empty(&mut s, "turn-3");
    assert!(matches!(last(&s, "t"), LifecycleRecord::Unfinished { wake: None, .. }), "{:?}", last(&s, "t"));
    assert!(open_case(&s).is_some() && !notifies_user(&ended));
    settle(&mut s, "Present beta 61 again as it is");
    start(&mut s, "turn-4");
    end_empty(&mut s, "turn-4");
    assert_eq!(unfinished(&s), 1 + usize::from(alondra_domain::EMPTY_TURN_WAKES), "the instruction is what the node owes now");
    assert!(open_case(&s).is_none() && lifecycle(&s).pending_inputs().is_empty());
    assert_aggregate_rules(&s);
}
fn settle(s: &mut DomainState, text: &str) {
    let case = open_case(s).expect("an open case");
    must(s, orchestrator(), Command::SettleStall { case_id: case, task_id: "t".into(), outcome: StallOutcome::Instruct { text: text.into() } });
}
#[derive(Clone, Debug)]
enum Step {
    Present,
    AskBlocking,
    AnswerText,
    AnswerOption,
    /// The daemon's outbox tells the node the answers the user gave.
    DeliverOutbox,
    Start,
    EndEmpty,
    Instruct,
    UserWrites,
    /// The daemon restarts: the state is read back and every lifecycle replayed from its journal.
    Restart,
}
fn step() -> impl Strategy<Value = Step> {
    prop_oneof![
        1 => Just(Step::Present),
        2 => Just(Step::AskBlocking),
        2 => Just(Step::AnswerText),
        1 => Just(Step::AnswerOption),
        3 => Just(Step::DeliverOutbox),
        4 => Just(Step::Start),
        4 => Just(Step::EndEmpty),
        2 => Just(Step::Instruct),
        1 => Just(Step::UserWrites),
        1 => Just(Step::Restart),
    ]
}
struct Run {
    s: DomainState,
    turns: u32,
    cards: u32,
    deliveries: u32,
    outbox: Vec<InteractionId>,
    answered: Vec<InteractionId>,
}
impl Run {
    fn open_question(&self) -> Option<InteractionId> {
        (1..=self.cards).rev().map(|n| InteractionId::from(format!("ask-{n}").as_str()))
            .find(|id| self.s.interactions.get(id).is_some_and(|i| self.s.open(i) && !DomainState::answered_question(i)))
    }
    /// Whether the turn that just ended started with the answer to one of the node's cards and
    /// did nothing else: nothing but its end came after its start.
    fn took_an_answer(&self) -> bool {
        let journal = &self.s.lifecycle_journal[&TaskId::from("t")];
        let mut since = journal.iter().rev().skip_while(|record| matches!(record, LifecycleRecord::TurnEnded { .. } | LifecycleRecord::Unfinished { .. }));
        let Some(LifecycleRecord::TurnStarted { input: taken, .. }) = since.next() else { return false };
        journal.iter().any(|record| matches!(record, LifecycleRecord::Told { input, answers: Some(_), .. } if input == taken))
    }
    fn delivery(&self) -> String {
        format!("r-{}", self.deliveries)
    }
    fn apply(&mut self, step: &Step) {
        let running = lifecycle(&self.s).turn().map(|turn| turn.as_str().to_owned());
        match step {
            Step::Present if running.is_some() => {
                self.deliveries += 1;
                let id = self.delivery();
                let _ = exec(&mut self.s, node("a"), Command::PresentResult {
                    attachments: vec![], task_id: "t".into(), interaction_id: id.as_str().into(), body: "Beta".into(), is_final: true, nothing_to_review: false,
                    targets: vec![DeliveryTarget::Text { text: "Beta".into() }], checklist: vec![], card: CardSpec::delivery("Review the beta", &[], true),
                });
            }
            Step::AskBlocking if running.is_some() => {
                self.cards += 1;
                let mut card = CardSpec::new(CardKind::Decide, "Decide whether to publish the beta").why("The testers get it once it is published");
                card.blocks = vec![CardTarget::Delivery { interaction_id: self.delivery().as_str().into() }];
                let _ = exec(&mut self.s, node("a"), Command::AskQuestion {
                    attachments: vec![], interaction_id: format!("ask-{}", self.cards).as_str().into(), scope: Scope::Task { task_id: "t".into() },
                    recipient: Recipient::User, body: "Publish the beta to the testers now?".into(), blocking: true,
                    options: vec![
                        QuestionOption { id: "go".into(), label: "Publish".into(), description: None },
                        QuestionOption { id: "hold".into(), label: "Hold it".into(), description: None },
                    ],
                    card,
                });
            }
            Step::AnswerText | Step::AnswerOption => {
                let Some(card) = self.open_question() else { return };
                let (option_id, inputs) = match step {
                    Step::AnswerText => (String::new(), BTreeMap::from([(ANSWER_TEXT.to_owned(), InputValue::Text(SAID.into()))])),
                    _ => ("go".to_owned(), BTreeMap::new()),
                };
                if exec(&mut self.s, user(), Command::AnswerQuestion { attachments: vec![], interaction_id: card.clone(), option_id, inputs }).is_ok() {
                    self.outbox.push(card.clone());
                    self.answered.push(card);
                }
            }
            Step::DeliverOutbox => {
                for card in std::mem::take(&mut self.outbox) {
                    let tell = Command::TellNode { task_id: "t".into(), text: format!("Answer to your card {card}: {SAID}."), from_user: true, answers: Some(card.clone()) };
                    if exec(&mut self.s, Actor::System, tell).is_err() {
                        self.outbox.push(card);
                    }
                }
            }
            Step::Start if running.is_none() && !lifecycle(&self.s).pending_inputs().is_empty() => {
                self.turns += 1;
                let input = lifecycle(&self.s).pending_inputs()[0].as_str().to_owned();
                let _ = exec(&mut self.s, Actor::System, Command::RecordRuntimeFacts { task_id: "t".into(), generation: 1, facts: vec![
                    RuntimeObservation::TurnStarted { turn: format!("turn-{}", self.turns), input },
                ] });
            }
            Step::EndEmpty => {
                let Some(turn) = running else { return };
                let owed_before = unfinished(&self.s);
                if exec(&mut self.s, Actor::System, Command::RecordTurnBoundary { task_id: "t".into(), generation: 1, turn, failure: None, failed: None }).is_ok() {
                    let lifecycle = lifecycle(&self.s);
                    let resting_on_delivery = lifecycle.phase() == alondra_domain::TaskPhase::Delivered;
                    if resting_on_delivery && unfinished(&self.s) > owed_before {
                        assert!(!lifecycle.pending_inputs().is_empty() || open_case(&self.s).is_some(),
                            "an owed rest is woken or cased: {lifecycle:?}");
                    }
                    let rests = resting_on_delivery && lifecycle.activity() == alondra_domain::ActivityView::Resting && lifecycle.awaits().is_none();
                    if rests && self.took_an_answer() {
                        assert!(!lifecycle.pending_inputs().is_empty() || open_case(&self.s).is_some(),
                            "a delivery resting on the answer it took is woken or cased: {lifecycle:?}");
                    }
                    if self.answered.is_empty() && resting_on_delivery {
                        assert_eq!(unfinished(&self.s), 0, "a delivery that took no answer owes nothing");
                    }
                }
            }
            Step::Instruct => {
                if let Some(case) = open_case(&self.s) {
                    let _ = exec(&mut self.s, orchestrator(), Command::SettleStall { case_id: case, task_id: "t".into(), outcome: StallOutcome::Instruct { text: "Present it again".into() } });
                }
            }
            Step::UserWrites => {
                let _ = exec(&mut self.s, Actor::System, Command::TellNode { task_id: "t".into(), text: "Keep going".into(), from_user: true, answers: None });
            }
            Step::Restart => self.s = serde_json::from_str(&serde_json::to_string(&self.s).unwrap()).unwrap(),
            _ => {}
        }
    }
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 128, failure_persistence: None, ..ProptestConfig::default() })]

    /// Whatever the node presents, asks and is answered, in any order with turns, instructions, the
    /// user's words and restarts: an answer always reaches its node, a rest on it is woken or a
    /// case for the orchestrator, and no card asks the user how the task goes on.
    #[test]
    fn an_answer_always_reaches_its_node_and_a_rest_on_it_is_woken_or_cased(steps in prop::collection::vec(step(), 1..60)) {
        let mut run = Run { s: world(), turns: 1, cards: 0, deliveries: 0, outbox: vec![], answered: vec![] };
        sdk_turn(&mut run.s, "t", "a");
        for step in &steps {
            run.apply(step);
            let asked: Vec<_> = run.s.interactions.values()
                .filter(|i| i.recipient == Recipient::User && (i.id.0.starts_with(TURN_ENDED_PREFIX) || i.id.0.starts_with(STALLED_PREFIX)))
                .map(|i| i.id.clone()).collect();
            prop_assert!(asked.is_empty(), "after {:?} a card asks the user how the task goes on: {:?}", step, asked);
            if matches!(step, Step::DeliverOutbox) {
                let awaits = lifecycle(&run.s).awaits().cloned();
                prop_assert!(!matches!(&awaits, Some(alondra_domain::Await::User(card)) if run.answered.contains(card) && !run.outbox.contains(card)),
                    "the node still waits on a card it was told the answer of: {:?}", awaits);
            }
        }
    }
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 256, failure_persistence: None, ..ProptestConfig::default() })]

    /// Every end of a turn, whatever the node presented, asked and was answered before it, leaves
    /// the task with a closed result, an input on its way to the node, or a case someone settles.
    #[test]
    fn every_turn_end_leaves_a_closed_outcome_an_input_or_a_case(steps in prop::collection::vec(step(), 1..60)) {
        let mut run = Run { s: world(), turns: 1, cards: 0, deliveries: 0, outbox: vec![], answered: vec![] };
        sdk_turn(&mut run.s, "t", "a");
        for step in &steps {
            let before = run.s.lifecycle_journal[&TaskId::from("t")].len();
            run.apply(step);
            let ended = run.s.lifecycle_journal[&TaskId::from("t")].iter().skip(before).find_map(|record| match record {
                LifecycleRecord::TurnEnded { unheard_card, .. } | LifecycleRecord::AwaitEnded { unheard_card, .. } => Some(unheard_card.clone()),
                _ => None,
            });
            if let Some(unheard) = ended {
                let after = super::stall_tests::after_the_end(&run.s, &unheard);
                prop_assert!(after.is_ok(), "after {:?}: {}", step, after.unwrap_err());
            }
        }
    }
}

/// The steps of beta-go, through the same harness the property drives.
#[test]
fn the_property_harness_reaches_the_beta_go_rest() {
    let mut run = Run { s: world(), turns: 1, cards: 0, deliveries: 0, outbox: vec![], answered: vec![] };
    sdk_turn(&mut run.s, "t", "a");
    for step in [Step::Present, Step::AskBlocking, Step::EndEmpty, Step::AnswerText, Step::DeliverOutbox, Step::Start, Step::EndEmpty] {
        run.apply(&step);
    }
    assert_eq!(run.answered, [InteractionId::from("ask-1")]);
    assert!(run.outbox.is_empty(), "the answer reached the node");
    assert!(matches!(last(&run.s, "t"), LifecycleRecord::Unfinished { wake: Some(_), .. }), "{:?}", last(&run.s, "t"));
}
