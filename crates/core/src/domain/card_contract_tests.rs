//! Every card type keeps its contract: a card that fits it is accepted, one that breaks a rule is
//! refused naming the rule and the field, and the real cases of the card audit can no longer be
//! made.
use super::*;
use std::collections::BTreeSet;

fn exec(s: &mut DomainState, a: Actor, c: Command) -> Result<CommandOutcome, DomainError> {
    let envelope = CommandEnvelope {
        protocol_version: 2,
        data_epoch: s.data_epoch.clone(),
        command_id: format!("c-{}", s.revision).into(),
        expected_revision: s.revision,
        command: c,
    };
    s.execute(&a, &envelope)
}
fn ok(s: &mut DomainState, a: Actor, c: Command) {
    exec(s, a, c).unwrap();
}
fn refused(s: &mut DomainState, a: Actor, c: Command) -> DomainError {
    let before = s.clone();
    let error = exec(s, a, c).unwrap_err();
    assert_eq!(*s, before, "a refused command changed the state");
    error
}
fn agent() -> Actor {
    Actor::Agent { agent_id: "a".into() }
}
fn orchestrator() -> Actor {
    Actor::Orchestrator { generation: 0 }
}
fn fixture() -> DomainState {
    let mut s = DomainState::new("epoch");
    ok(&mut s, Actor::System, Command::RegisterAgent { agent_id: "a".into(), name: "Node A".into(), at: 0 });
    ok(&mut s, Actor::User { user_id: "u".into() }, Command::CreateRequest { request_id: "r".into(), body: "Build it".into(), auto_merge: false, maintenance: false, message: false });
    ok(&mut s, orchestrator(), Command::CreateTask { plan_items: None, continues: None,
        task_id: "t".into(), request_id: "r".into(), title: "Build it".into(), agent_id: Some("a".into()),
        review_prs: false, dependencies: BTreeSet::new(), stack: None,
        launch: Some(LaunchPlan::new("node", "/tmp")),
    });
    s
}
fn option(id: &str) -> QuestionOption {
    QuestionOption { id: id.into(), label: id.into(), description: None }
}
fn breach(error: &DomainError) -> &ContractBreach {
    error.contract.as_ref().unwrap_or_else(|| panic!("{error:?} names no rule of the card contracts"))
}

fn sample(kind: SystemCardKind) -> SystemCard {
    use SystemCardKind as K;
    match kind {
        K::NodePause => SystemCard::NodePause { pause: NodePauseKind::User, since: 0 },
        K::OverPace => SystemCard::OverPace { day: "2026-11-07".into() },
        K::PaceResume => SystemCard::PaceResume { day: "2026-11-07".into() },
        K::Machine => SystemCard::Machine,
        K::LocalNetwork => SystemCard::LocalNetwork,
        K::McpWait => SystemCard::McpWait,
        K::ClaudeSignedOut => SystemCard::ClaudeSignedOut,
        K::ClaudeOutage => SystemCard::ClaudeOutage,
        K::ClaudeUsage => SystemCard::ClaudeUsage { resets_at: None },
        K::HostLost => SystemCard::HostLost,
        K::TurnEnded => SystemCard::TurnEnded,
        K::Stalled => SystemCard::Stalled,
        K::SessionEnded => SystemCard::SessionEnded,
        K::HandoffDoubt => SystemCard::HandoffDoubt,
        K::DecisionCancelled => SystemCard::DecisionCancelled,
        K::Dropped => SystemCard::Dropped,
        K::Unheard => SystemCard::Unheard,
        K::EndedUnanswered => SystemCard::EndedUnanswered { question: "q".into() },
        K::StartProblem => SystemCard::StartProblem,
        K::PostMergeProblem => SystemCard::PostMergeProblem,
        K::CloseProblem => SystemCard::CloseProblem,
        K::DiscardWorktrees => SystemCard::DiscardWorktrees,
        K::OpenPrDelivery => SystemCard::OpenPrDelivery,
        K::LaunchProblem => SystemCard::LaunchProblem { run: "run".into() },
        K::BatchProblem => SystemCard::BatchProblem { run: "run".into() },
        K::StuckProblem => SystemCard::StuckProblem,
        K::RequestDone => SystemCard::RequestDone,
        K::RequestTasks => SystemCard::RequestTasks,
        K::Inbox => SystemCard::Inbox { about: InboxCard::StallNotice, subject: "t".into(), number: 1 },
        K::RefusedCards => SystemCard::RefusedCards { owner: "a".into() },
        K::Unknown => SystemCard::Unknown,
    }
}

/// A card that keeps every rule of `card_type`'s contract, as its first creator would send it to
/// its first reader.
struct Fitting {
    card: CardSpec,
    system: Option<SystemCard>,
    payload: InteractionPayload,
    author: Author,
    to: Recipient,
    held: bool,
}
impl Fitting {
    fn of(card_type: CardType) -> Self {
        let contract = card_type.contract();
        let creator = contract.creators[0];
        let author = match creator {
            Creator::Node | Creator::Hook => Author::Agent { agent_id: "a".into() },
            Creator::Orchestrator => Author::Orchestrator { generation: 0 },
            Creator::Alondra => Author::System,
        };
        let to = recipient(contract.readers[0]);
        let mut card = CardSpec::new(contract.kinds[0], "Title").why("Why").content(ContentKind::Report, "Body");
        if contract.requires.contains(&CardField::Verify) {
            card.verify = Some("I read the setting".into());
        }
        if card_type == CardType::ProposedPlan {
            card.plan = Some("- [ ] Build it".into());
        }
        let delivery = |is_final| InteractionPayload::Delivery { targets: vec![], is_final, acceptance: None, feedback: vec![] };
        let payload = match card_type {
            CardType::Result | CardType::ProposedPlan | CardType::System(SystemCardKind::OpenPrDelivery) => delivery(true),
            CardType::Progress => delivery(false),
            _ if contract.response == Response::Decides => InteractionPayload::Question { options: vec![option("go")], answer: None },
            _ => InteractionPayload::Information { read_by: BTreeSet::new() },
        };
        let system = match card_type {
            CardType::System(kind) => Some(sample(kind)),
            _ => None,
        };
        Fitting { card, system, payload, author, to, held: creator == Creator::Hook }
    }
    fn check(&self, blocking: bool) -> Result<CardType, ContractBreach> {
        NewCard { card: &self.card, system: self.system.as_ref(), payload: &self.payload, author: &self.author, to: &self.to, blocking, held: self.held }.check()
    }
}
fn recipient(reader: Reader) -> Recipient {
    match reader {
        Reader::User => Recipient::User,
        Reader::Orchestrator => Recipient::Orchestrator,
        Reader::Node => Recipient::Agent { agent_id: "a".into() },
    }
}

#[test]
fn every_card_type_accepts_a_card_that_keeps_its_contract_and_names_the_rule_a_card_breaks() {
    for card_type in CardType::all() {
        let contract = card_type.contract();
        if contract.creators.is_empty() {
            continue;
        }
        let fitting = Fitting::of(card_type);
        assert_eq!(fitting.check(false), Ok(card_type), "{} refused a card that keeps its contract", card_type.name());
        let held = fitting.held;
        assert_eq!(CardType::of(&fitting.card, fitting.system.as_ref(), &fitting.payload, held), Some(card_type));

        let rule_of = |broken: &Fitting, blocking| broken.check(blocking).expect_err(&card_type.name());
        if let Some(stranger) = [Reader::User, Reader::Orchestrator, Reader::Node].into_iter().find(|r| !contract.readers.contains(r)) {
            let mut broken = Fitting::of(card_type);
            broken.to = recipient(stranger);
            let breach = rule_of(&broken, false);
            assert_eq!((breach.card_type, breach.rule, breach.field.as_str()), (Some(card_type), ContractRule::Reader, "recipient"));
        }
        if !held && let Some(stranger) = [Author::Orchestrator { generation: 0 }, Author::System].into_iter().find(|a| *a != fitting.author && {
            let creator = if *a == Author::System { Creator::Alondra } else { Creator::Orchestrator };
            !contract.creators.contains(&creator)
        }) {
            let mut broken = Fitting::of(card_type);
            broken.author = stranger;
            broken.held = false;
            assert_eq!(rule_of(&broken, false).rule, ContractRule::Creator, "{}", card_type.name());
        }
        for field in contract.requires {
            let mut broken = Fitting::of(card_type);
            match field {
                CardField::Title => broken.card.title = None,
                CardField::Why => broken.card.why.clear(),
                CardField::Content => broken.card.content = None,
                CardField::Verify => broken.card.verify = None,
            }
            assert_eq!(rule_of(&broken, false).rule, ContractRule::Required, "{} without {field:?}", card_type.name());
        }
        match (&fitting.payload, contract.response) {
            (InteractionPayload::Question { .. }, Response::Decides)
                if !contract.answers_in_text && !(fitting.author == Author::System && fitting.card.kind == CardKind::Problem) => {
                let mut broken = Fitting::of(card_type);
                broken.payload = InteractionPayload::Question { options: vec![], answer: None };
                let breach = rule_of(&broken, false);
                assert_eq!((breach.rule, breach.field.as_str()), (ContractRule::NoDecision, "options"), "{}", card_type.name());
            }
            (InteractionPayload::Information { .. }, _) => {
                assert_eq!(rule_of(&fitting, true).rule, ContractRule::ReadOnly, "{} blocking", card_type.name());
            }
            _ => {}
        }
    }
}

#[test]
fn a_kind_no_daemon_writes_anymore_is_never_created() {
    let fitting = Fitting { system: Some(SystemCard::Unknown), ..Fitting::of(CardType::Notice) };
    assert_eq!(fitting.check(false).unwrap_err().rule, ContractRule::Creator);
}

/// Case 1, 2 and 17 of the audit: a report whose need asked for a decision, a question sent as a
/// review, and a review asking for an action the node cannot do. A result's need is its type's.
#[test]
fn a_result_never_asks_in_its_need_for_what_its_actions_do_not_do() {
    let mut s = fixture();
    let card = CardSpec::new(CardKind::Review, "Weekly cycle of the app").content(ContentKind::Report, "This week's numbers.");
    let card = CardSpec { need: "Decide whether you post the app in developer forums".into(), ..card };
    ok(&mut s, agent(), Command::PresentResult {
        checklist: Vec::new(), attachments: vec![], task_id: "t".into(), interaction_id: "result".into(),
        body: "This week's numbers.".into(), targets: vec![DeliveryTarget::Text { text: "Report".into() }],
        is_final: true, nothing_to_review: false, card,
    });
    assert_eq!(s.interactions[&"result".into()].card.need, RESULT_NEED);
}

#[test]
fn a_progress_notice_says_it_is_to_read() {
    let mut s = fixture();
    let card = CardSpec { need: "Wait for the audits to finish".into(), ..CardSpec::new(CardKind::Know, "Halfway").content(ContentKind::Report, "Half done.") };
    ok(&mut s, agent(), Command::PresentResult {
        checklist: Vec::new(), attachments: vec![], task_id: "t".into(), interaction_id: "progress".into(),
        body: "Half done.".into(), targets: vec![DeliveryTarget::Text { text: "Half done".into() }],
        is_final: false, nothing_to_review: false, card,
    });
    assert_eq!(s.interactions[&"progress".into()].card.need, PROGRESS_NEED);
}

/// What the removed task «Sin tarjetas que solo abren GitHub» asked: a card that asks the user for
/// something offers at least one option.
#[test]
fn a_problem_with_nothing_to_press_is_refused() {
    let mut s = fixture();
    let error = refused(&mut s, orchestrator(), Command::RaiseProblem {
        attachments: vec![], interaction_id: "problem".into(), scope: Scope::Task { task_id: "t".into() },
        body: "GitHub refused the merge".into(), options: vec![],
        card: CardSpec::new(CardKind::Problem, "The merge failed").why("PR #65 stays open"),
    });
    assert_eq!(error.code, "card_contract");
    assert_eq!((breach(&error).card_type, breach(&error).rule), (Some(CardType::Problem), ContractRule::NoDecision));
    ok(&mut s, orchestrator(), Command::RaiseProblem {
        attachments: vec![], interaction_id: "problem".into(), scope: Scope::Task { task_id: "t".into() },
        body: "GitHub refused the merge".into(), options: vec![option("retry")],
        card: CardSpec::new(CardKind::Problem, "The merge failed").why("PR #65 stays open"),
    });
}

#[test]
fn a_node_does_not_suggest_to_the_user() {
    let mut s = fixture();
    let error = refused(&mut s, agent(), Command::AskQuestion {
        attachments: vec![], interaction_id: "idea".into(), scope: Scope::Task { task_id: "t".into() },
        recipient: Recipient::User, body: "The README names an old flag".into(), blocking: false,
        card: CardSpec::new(CardKind::Propose, "The README names an old flag").why("Not this task"),
        options: suggestion_options(),
    });
    assert_eq!((breach(&error).card_type, breach(&error).rule), (Some(CardType::Suggestion), ContractRule::Reader));
}

/// Case 4 of the audit: «… is stuck: Alondra refused its card» reached the user.
#[test]
fn a_node_stuck_on_refused_cards_is_told_to_the_orchestrator_never_to_the_user() {
    let fitting = Fitting::of(CardType::System(SystemCardKind::RefusedCards));
    assert_eq!(fitting.to, Recipient::Orchestrator);
    let to_user = Fitting { to: Recipient::User, ..Fitting::of(CardType::System(SystemCardKind::RefusedCards)) };
    assert_eq!(to_user.check(false).unwrap_err().rule, ContractRule::Reader);
    let mut s = fixture();
    let error = refused(&mut s, Actor::System, Command::RaiseProblem {
        attachments: vec![], interaction_id: "problem-a-card".into(), scope: Scope::Task { task_id: "t".into() },
        body: "Node A sent card x and Alondra refused it 3 times in a row".into(), options: vec![],
        card: CardSpec { system: Some(SystemCard::RefusedCards { owner: "a".into() }), cause: Some("refused-cards:a".into()),
            ..CardSpec::new(CardKind::Problem, "Node A is stuck: Alondra refused its card").why("checklist_incomplete") },
    });
    assert_eq!(breach(&error).card_type, Some(CardType::System(SystemCardKind::RefusedCards)));
}

#[test]
fn a_refusal_of_a_card_names_its_type_even_when_only_a_field_is_too_long() {
    let mut s = fixture();
    let error = refused(&mut s, agent(), Command::AskQuestion {
        attachments: vec![], interaction_id: "q".into(), scope: Scope::Task { task_id: "t".into() },
        recipient: Recipient::User, body: "Which?".into(), blocking: false,
        card: CardSpec::new(CardKind::Decide, "Pick one").why(&"w".repeat(CardSpec::WHY_MAX + 1)),
        options: vec![option("a"), option("b")],
    });
    assert_eq!(error.code, "invalid_input");
    assert_eq!(*breach(&error), ContractBreach { card_type: Some(CardType::Question), rule: ContractRule::Shape, field: "card.why".into() });
}

#[test]
fn every_card_alondra_writes_keeps_its_contract() {
    let task: TaskId = "t".into();
    let cards = [lifecycle::decision_cancelled_card("d".into(), &task, "Build it", 1)];
    for card in cards {
        let checked = NewCard { card: &card.card, system: card.system_card.as_ref(), payload: &card.payload, author: &card.author,
            to: &card.recipient, blocking: card.blocking, held: false }.check();
        assert!(checked.is_ok(), "{:?}: {checked:?}", card.system_card);
    }
}

/// The catalog the repository documents is the one the domain checks.
#[test]
fn the_documented_catalog_lists_every_card_type_with_its_contract() {
    let documented = include_str!("../../../../docs/architecture/cards.md");
    let table = catalog_table();
    assert!(documented.contains(&table), "docs/architecture/cards.md is out of date; its table is:\n\n{table}");
}
