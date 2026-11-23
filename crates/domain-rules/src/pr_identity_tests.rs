//! A pull request is its provider key, not its address: a repository that moves keeps one record,
//! two records of one pull request join into one card with everything both had, and nothing of it
//! reaches the provider. `ana/lumo` moves to `lumo-org/lumo`; `ana/site` is another repository
//! whose pull request has the same number.
use super::*;
use proptest::prelude::*;
use std::collections::BTreeSet;

const AGENT: &str = "a";
const TASK: &str = "t";
const OLD: &str = "ana/lumo";
const NEW: &str = "lumo-org/lumo";
const SITE: &str = "ana/site";
const HEAD: &str = "1111111111111111111111111111111111111111";
const SITE_HEAD: &str = "2222222222222222222222222222222222222222";
const HOUR: Millis = 60 * 60 * 1000;

fn exec(s: &mut DomainState, actor: Actor, command: Command) -> Result<CommandOutcome, DomainError> {
    s.execute(&actor, &CommandEnvelope {
        protocol_version: PROTOCOL_VERSION, data_epoch: s.data_epoch.clone(), command_id: format!("c-{}", s.revision).into(),
        expected_revision: s.revision, command,
    })
}

fn must(s: &mut DomainState, actor: Actor, command: Command) -> CommandOutcome {
    let shown = format!("{command:?}");
    exec(s, actor, command).unwrap_or_else(|error| panic!("{shown} was refused: {error}"))
}

fn node() -> Actor {
    Actor::Agent { agent_id: AGENT.into() }
}

fn user() -> Actor {
    Actor::User { user_id: "u".into() }
}

fn at_work() -> DomainState {
    let mut s = DomainState::new("epoch");
    must(&mut s, Actor::System, Command::RegisterAgent { agent_id: AGENT.into(), name: "Node".into(), at: 0 });
    must(&mut s, user(), Command::CreateRequest { request_id: "r".into(), body: "Work".into(), auto_merge: false, maintenance: false, message: false });
    must(&mut s, Actor::Orchestrator { generation: 0 }, Command::CreateTask { priority: Default::default(), plan_items: None, continues: None,
        task_id: TASK.into(), request_id: "r".into(), title: "Fix the menu".into(), agent_id: None, review_prs: false,
        dependencies: BTreeSet::new(), stack: None, launch: Some(LaunchPlan::new("node", "/synthetic/repository")),
    });
    must(&mut s, Actor::System, Command::AdmitExecution { provider: alondra_core::accounts::Provider::Claude, task_id: TASK.into(), agent_id: AGENT.into(),
        conversation: "conversation".into(), instance: "instance".into(), host: "sdk-host".into(), model: None, account: None, leaves_limit: None });
    let LifecycleRecord::Planned { input } = s.lifecycle_journal[&TaskId::from(TASK)][0].clone() else { panic!("planned first") };
    must(&mut s, Actor::System, Command::RecordRuntimeFacts { task_id: TASK.into(), generation: 1, facts: vec![
        RuntimeObservation::ResourceAvailable,
        RuntimeObservation::TurnStarted { turn: "turn-1".into(), input },
    ] });
    s
}

fn id(repository: &str, number: u64) -> PrId {
    hosted_pr_id("github.com", repository, number)
}

fn register(s: &mut DomainState, repository: &str, number: u64, head: &str) -> Result<CommandOutcome, DomainError> {
    exec(s, node(), Command::RegisterPr {
        task_id: TASK.into(), pr_id: id(repository, number), host: "github.com".into(), repository: repository.into(),
        number, head: head.into(), base: "main".into(), allowed_methods: vec![MergeMethod::Squash],
    })
}

fn deliver(s: &mut DomainState, prs: &[PrId]) {
    let targets = prs.iter().map(|pr| DeliveryTarget::PullRequest { pr_id: pr.clone(), head: String::new() }).collect();
    must(s, node(), Command::PresentResult {
        checklist: Vec::new(), attachments: vec![], task_id: TASK.into(), interaction_id: "final".into(), body: "Done".into(),
        targets, is_final: true, nothing_to_review: false, card: CardSpec::delivery("Review the result", &[], true),
    });
    must(s, Actor::System, Command::RecordTurnBoundary { task_id: TASK.into(), generation: 1, turn: "turn-1".into(), failure: None, failed: None });
}

fn confirmed(key: &str, current: &str) -> PrAddressVerdict {
    PrAddressVerdict::Confirmed { key: ProviderPrKey(key.into()), locator: ProviderLocator(format!("locator-{key}")), current_repository: current.into() }
}

fn observe(pr: &PrId, via: &str, at: Millis, verdict: PrAddressVerdict) -> Command {
    Command::ObservePrAddress { pr_id: pr.clone(), read: PrRead::Address { repository: via.into() }, read_started_at: at, verdict }
}

fn tracked(s: &DomainState) -> Vec<&PullRequest> {
    s.pull_requests.values().filter(|pr| pr.is_tracked()).collect()
}

/// The PR of each card waiting for the user.
fn cards(s: &DomainState) -> Vec<PrId> {
    crate::ReadIndex::build(s).cards().iter().filter_map(|item| {
        let InboxSubject::Interaction { interaction_id, pr_id } = &item.subject;
        pr_id.clone().or_else(|| match &s.interactions[interaction_id].payload {
            InteractionPayload::Delivery { targets, .. } => targets.iter().find_map(|t| match t {
                DeliveryTarget::PullRequest { pr_id, .. } => Some(pr_id.clone()),
                _ => None,
            }),
            _ => None,
        })
    }).collect()
}

fn no_remote_effects(outcome: &CommandOutcome, s: &DomainState) {
    assert!(!outcome.effects.iter().any(|e| matches!(e, DomainEffect::Relocate { .. })), "{:?}", outcome.effects);
    assert!(s.tasks[&TaskId::from(TASK)].open_pr.is_none(), "nothing waits to be closed on the provider");
}

/// The two records the move left: the task registered the PR at its old address and again,
/// after the move, at its new one, and delivered both.
fn two_copies() -> DomainState {
    let mut s = at_work();
    register(&mut s, OLD, 42, HEAD).unwrap();
    register(&mut s, NEW, 42, HEAD).unwrap();
    deliver(&mut s, &[id(OLD, 42), id(NEW, 42)]);
    s
}

#[test]
fn a_transferred_repository_keeps_one_pr_record() {
    let mut s = at_work();
    register(&mut s, OLD, 42, HEAD).unwrap();
    must(&mut s, Actor::System, observe(&id(OLD, 42), OLD, 1_000, confirmed("key-a", NEW)));
    assert_eq!(register(&mut s, NEW, 42, HEAD).unwrap_err().code, "no_effect", "the new address is the PR already registered");
    assert_eq!(register(&mut s, OLD, 42, HEAD).unwrap_err().code, "no_effect", "a node whose git remote still names the old repository names the same PR");
    register(&mut s, OLD, 42, SITE_HEAD).expect("a new head of that PR through its old address corrects the one record");
    assert_eq!(s.pull_requests[&id(OLD, 42)].head, SITE_HEAD);

    let records = tracked(&s);
    assert_eq!(records.len(), 1, "{records:?}");
    assert_eq!(records[0].id, id(OLD, 42));
    assert_eq!(records[0].repository, NEW);
    assert_eq!(records[0].aliases, BTreeSet::from([OLD.to_owned()]));
    assert_eq!(s.tasks[&TaskId::from(TASK)].pr_ids().count(), 1);
}

#[test]
fn two_records_of_one_pr_join_into_one_card_without_remote_effects() {
    let mut s = two_copies();
    assert_eq!(cards(&s).len(), 2, "the move left two cards of one PR");

    let first = must(&mut s, Actor::System, observe(&id(OLD, 42), OLD, 1_000, confirmed("key-a", NEW)));
    let second = must(&mut s, Actor::System, observe(&id(NEW, 42), NEW, 2_000, confirmed("key-a", NEW)));
    no_remote_effects(&first, &s);
    no_remote_effects(&second, &s);

    assert_eq!(cards(&s), [id(OLD, 42)]);
    let kept = &s.pull_requests[&id(OLD, 42)];
    assert_eq!((kept.repository.as_str(), kept.address_read_at), (NEW, 2_000));
    assert_eq!(s.pull_requests[&id(NEW, 42)].tracking, PrTracking::Superseded { by: id(OLD, 42) });
    let task = &s.tasks[&TaskId::from(TASK)];
    assert_eq!(s.open_prs(task).map(|pr| pr.id.clone()).collect::<Vec<_>>(), [id(OLD, 42)]);
    assert_eq!(s.task_prs(task).count(), 1);
    assert_eq!(s.main_pr(task).map(|pr| pr.id.clone()), Some(id(OLD, 42)));
    let InteractionPayload::Delivery { targets, .. } = &s.interactions[&"final".into()].payload else { panic!("a delivery") };
    assert_eq!(*targets, [DeliveryTarget::PullRequest { pr_id: id(OLD, 42), head: HEAD.into() }]);
}

#[test]
fn a_record_never_shares_its_address_with_another_pr() {
    let mut s = at_work();
    let (lumo, site) = (id(OLD, 42), id(SITE, 42));
    register(&mut s, OLD, 42, HEAD).unwrap();
    must(&mut s, Actor::System, observe(&lumo, OLD, 1_000, confirmed("key-a", OLD)));
    register(&mut s, SITE, 42, SITE_HEAD).unwrap();

    let renamed = must(&mut s, Actor::System, observe(&site, SITE, 2_000, confirmed("key-b", OLD)));
    assert_eq!(s.pull_requests[&site].repository, OLD, "the repository renamed into the old name took the address");
    assert_eq!(s.pull_requests[&lumo].identity,
        PrIdentity::Unresolvable { key: Some(ProviderPrKey("key-a".into())), reason: UnresolvableReason::AddressReused, since: 2_000 });
    assert_eq!(renamed.effects.iter().filter(|e| matches!(e, DomainEffect::Relocate { .. })).cloned().collect::<Vec<_>>(),
        [DomainEffect::Relocate { pr_id: lumo.clone(), locator: ProviderLocator("locator-key-a".into()) }]);

    must(&mut s, Actor::System, Command::ObservePrAddress { pr_id: lumo.clone(), read: PrRead::Locator, read_started_at: 3_000, verdict: confirmed("key-a", NEW) });
    assert!(s.pull_requests[&lumo].identity.is_verified(), "its locator found it where it moved");
    assert_eq!(s.pull_requests[&lumo].repository, NEW);
    assert_eq!(tracked(&s).len(), 2, "two pull requests, two records");
}

fn authorization(id: &str, pr: &PrId, head: &str, at: Revision, attempt: MergeAttemptState) -> MergeAuthorization {
    MergeAuthorization { id: id.into(), task_id: TASK.into(), pr_id: pr.clone(), presentation_id: "final".into(), head: head.into(),
        method: MergeMethod::Squash, user_id: "u".into(), valid: true, attempt, claimed_by: None, at_revision: at, despite_pending_message: false }
}

fn decision(presentation: &str, at: Revision) -> PrDecision {
    PrDecision { presentation_id: presentation.into(), head: HEAD.into(), accepted: true, user_id: "u".into(), at_revision: at }
}

#[test]
fn joining_keeps_decisions_and_references() {
    let mut s = two_copies();
    let (kept, copy) = (id(OLD, 42), id(NEW, 42));
    s.pull_requests.get_mut(&kept).unwrap().review = Some(PullRequestReview { decisions: vec![decision("final", 3)] });
    s.pull_requests.get_mut(&copy).unwrap().review = Some(PullRequestReview { decisions: vec![decision("final", 1), decision("final", 5)] });
    for a in [
        authorization("on-kept", &kept, HEAD, 5, MergeAttemptState::Unclaimed),
        authorization("same-head", &copy, HEAD, 7, MergeAttemptState::Unclaimed),
        authorization("in-flight", &copy, HEAD, 8, MergeAttemptState::Claimed),
    ] {
        s.merge_authorizations.insert(a.id.clone(), a);
    }
    let final_id = InteractionId::from("final");
    let delivery = s.interactions.get_mut(&final_id).unwrap();
    delivery.subject = Some(InteractionSubject::PullRequest { pr_id: copy.clone() });
    delivery.card.blocks.push(CardTarget::PullRequest { pr_id: copy.clone() });
    let InteractionPayload::Delivery { feedback, .. } = &mut delivery.payload else { panic!("a delivery") };
    feedback.push(Feedback { target: FeedbackTarget::PullRequest { pr_id: copy.clone(), head: HEAD.into() }, reply_window_start: None,
        attachments: vec![], user_id: "u".into(), body: "Explain".into(), at_revision: 1, instruction: Instruction::Reply, merge_failure: None });
    s.checklist_marks.insert(final_id.clone(), vec![ChecklistMark { item: "1".into(),
        evidence: vec![Evidence { kind: EvidenceKind::Pr, value: copy.0.clone(), detail: None }] }]);
    s.stack_readiness.insert(TASK.into(), StackReadiness { task_id: TASK.into(), presentation_id: final_id.clone(), pr_id: copy.clone(), head: HEAD.into(), ready: true });
    s.tasks.get_mut(&TaskId::from(TASK)).unwrap().opened_prs.insert(OpenedPr { host: "github.com".into(), repository: NEW.into(), number: 42 });
    s.pr_task_history.insert(copy.clone(), BTreeSet::from([TaskId::from(TASK)]));

    must(&mut s, Actor::System, observe(&kept, OLD, 1_000, confirmed("key-a", NEW)));

    let review = s.pull_requests[&kept].review.as_ref().unwrap();
    assert_eq!(review.decisions, [decision("final", 1), decision("final", 3), decision("final", 5)], "no decision is lost");
    let a = |id: &str| &s.merge_authorizations[&AuthorizationId::from(id)];
    assert!(s.merge_authorizations.values().all(|a| a.pr_id == kept));
    assert!(a("on-kept").valid, "the older go-ahead for the same commit stays");
    assert!(!a("same-head").valid, "the same decision twice counts once");
    assert!(a("in-flight").valid && a("in-flight").attempt == MergeAttemptState::Claimed, "a merge in flight is never undone");
    let delivery = &s.interactions[&final_id];
    assert_eq!(delivery.subject, Some(InteractionSubject::PullRequest { pr_id: kept.clone() }));
    assert_eq!(delivery.card.blocks, [CardTarget::PullRequest { pr_id: kept.clone() }]);
    let InteractionPayload::Delivery { feedback, targets, .. } = &delivery.payload else { panic!("a delivery") };
    assert_eq!(feedback[0].target, FeedbackTarget::PullRequest { pr_id: kept.clone(), head: HEAD.into() });
    assert_eq!(targets.len(), 1);
    assert_eq!(s.checklist_marks[&final_id][0].evidence[0].value, kept.0);
    assert_eq!(s.stack_readiness[&TaskId::from(TASK)].pr_id, kept);
    assert!(s.tasks[&TaskId::from(TASK)].opened_prs.is_empty(), "the copy's address is not read again as a new PR");
    assert!(s.pr_task_history[&kept].contains(&TaskId::from(TASK)));
}

#[test]
fn an_action_on_a_joined_card_is_refused_and_touches_nothing() {
    let mut s = two_copies();
    must(&mut s, Actor::System, observe(&id(OLD, 42), OLD, 1_000, confirmed("key-a", NEW)));
    let copy = id(NEW, 42);
    let copy_target = vec![DeliveryTarget::PullRequest { pr_id: copy.clone(), head: HEAD.into() }];
    for command in [
        Command::AuthorizeMerge { authorization_id: "merge-copy".into(), presentation_id: "final".into(), pr_id: copy.clone(),
            expected_head: HEAD.into(), method: MergeMethod::Squash, despite_pending_message: false },
        Command::SubmitFeedback { attachments: vec![], presentation_id: "final".into(), targets: copy_target.clone(), body: "Change it".into(),
            instruction: Instruction::default(), merge_failure: None },
        Command::AcceptDelivery { presentation_id: "final".into(), targets: copy_target.clone() },
    ] {
        let before = s.clone();
        let error = exec(&mut s, user(), command.clone()).unwrap_err();
        assert_eq!((error.code.as_str(), error.message.as_str()), ("pr_superseded", PR_SUPERSEDED), "{command:?}");
        assert_eq!(s, before, "{command:?} changed nothing");
    }
    for command in [
        Command::ObservePrState { pr_id: copy.clone(), head: HEAD.into(), base: "main".into(), state: PrState::Closed, merged_head: None },
        Command::ObservePrMergeMethods { pr_id: copy.clone(), allowed_methods: vec![MergeMethod::Merge] },
    ] {
        assert_eq!(exec(&mut s, Actor::System, command).unwrap_err().code, "pr_superseded");
    }
    assert!(s.merge_authorizations.is_empty());
}

#[test]
fn same_number_in_two_repositories_stays_two_prs() {
    let mut s = at_work();
    register(&mut s, OLD, 5, HEAD).unwrap();
    register(&mut s, SITE, 5, SITE_HEAD).unwrap();
    deliver(&mut s, &[id(OLD, 5), id(SITE, 5)]);
    must(&mut s, Actor::System, observe(&id(OLD, 5), OLD, 1_000, confirmed("key-a", OLD)));
    must(&mut s, Actor::System, observe(&id(SITE, 5), SITE, 1_000, confirmed("key-b", SITE)));

    assert_eq!(tracked(&s).len(), 2);
    assert_eq!(cards(&s), [id(OLD, 5), id(SITE, 5)]);
    assert!(s.pull_requests.values().all(|pr| pr.aliases.is_empty()));
}

#[derive(Clone, Debug)]
enum Read {
    BeforeMove(Millis),
    Old(Millis),
    New(Millis),
    Missing(Millis),
    Unreachable(Millis),
}

fn reading(read: &Read) -> Command {
    match read {
        Read::BeforeMove(at) => observe(&id(OLD, 42), OLD, *at, confirmed("key-a", OLD)),
        Read::Old(at) => observe(&id(OLD, 42), OLD, *at, confirmed("key-a", NEW)),
        Read::New(at) => observe(&id(NEW, 42), NEW, *at, confirmed("key-a", NEW)),
        Read::Missing(at) => observe(&id(NEW, 42), NEW, *at, PrAddressVerdict::NotFound),
        Read::Unreachable(at) => observe(&id(OLD, 42), OLD, *at, PrAddressVerdict::Unreachable),
    }
}

/// The records that stand for a PR, whole, and what became of each copy.
fn identity_state(s: &DomainState) -> (Vec<PullRequest>, Vec<(PrId, PrTracking)>) {
    (tracked(s).into_iter().cloned().collect(), s.pull_requests.values().map(|pr| (pr.id.clone(), pr.tracking.clone())).collect())
}

proptest! {
    #[test]
    fn observation_order_and_repetition_give_the_same_state(
        order in Just(vec![Read::BeforeMove(400), Read::Old(1_000), Read::New(3_000), Read::Old(2_000), Read::Missing(500), Read::Unreachable(4_000)]).prop_shuffle(),
        repeats in proptest::collection::vec(0usize..6, 0..4),
    ) {
        let mut reads = order.clone();
        for i in repeats {
            reads.push(order[i].clone());
        }
        let mut expected = two_copies();
        for read in [Read::BeforeMove(400), Read::Missing(500), Read::Old(1_000), Read::Old(2_000), Read::New(3_000), Read::Unreachable(4_000)] {
            must(&mut expected, Actor::System, reading(&read));
        }
        let mut s = two_copies();
        for read in &reads {
            must(&mut s, Actor::System, reading(read));
        }
        prop_assert_eq!(identity_state(&s), identity_state(&expected));
        let kept = &s.pull_requests[&id(OLD, 42)];
        prop_assert_eq!(kept.repository.as_str(), NEW);
        prop_assert_eq!(kept.address_read_at, 3_000);
        prop_assert_eq!(&kept.aliases, &BTreeSet::from([OLD.to_owned()]));
        prop_assert_eq!(kept.missing, None);
        prop_assert_eq!(cards(&s), vec![id(OLD, 42)]);
    }
}

#[test]
fn an_unreachable_github_never_changes_identity() {
    let mut s = at_work();
    register(&mut s, OLD, 42, HEAD).unwrap();
    let pr = id(OLD, 42);
    let before = s.pull_requests[&pr].clone();
    for n in 0..10 {
        must(&mut s, Actor::System, observe(&pr, OLD, 1_000 + n * HOUR, PrAddressVerdict::Unreachable));
    }
    assert_eq!(s.pull_requests[&pr], before);

    must(&mut s, Actor::System, observe(&pr, OLD, 20 * HOUR, confirmed("key-a", OLD)));
    let verified = s.pull_requests[&pr].clone();
    for n in 0..10 {
        let outcome = must(&mut s, Actor::System, observe(&pr, OLD, 21 * HOUR + n * HOUR, PrAddressVerdict::Unreachable));
        assert!(outcome.effects.is_empty(), "{:?}", outcome.effects);
    }
    assert_eq!(s.pull_requests[&pr], verified);
}

#[test]
fn a_verified_pr_whose_address_leads_elsewhere_is_looked_for_by_its_locator() {
    let mut s = at_work();
    register(&mut s, OLD, 42, HEAD).unwrap();
    let pr = id(OLD, 42);
    must(&mut s, Actor::System, observe(&pr, OLD, 1_000, confirmed("key-a", OLD)));
    for (at, verdict) in [(2_000, PrAddressVerdict::NamesAnotherPr), (2_100, PrAddressVerdict::NotFound)] {
        let outcome = must(&mut s, Actor::System, observe(&pr, OLD, at, verdict.clone()));
        assert_eq!(outcome.effects, [DomainEffect::Relocate { pr_id: pr.clone(), locator: ProviderLocator("locator-key-a".into()) }]);
        assert_eq!(s.pull_requests[&pr].repository, OLD, "the record does not change until the PR is found");
        assert!(must(&mut s, Actor::System, observe(&pr, OLD, at, verdict)).effects.is_empty(), "the same read asks once");
    }
    let wrong_key = must(&mut s, Actor::System, observe(&pr, OLD, 2_500, confirmed("key-b", OLD)));
    assert!(matches!(wrong_key.effects[..], [DomainEffect::Relocate { .. }]), "a read that reached another PR never replaces the key");
    assert_eq!(s.pull_requests[&pr].identity.key(), Some(&ProviderPrKey("key-a".into())));
    must(&mut s, Actor::System, Command::ObservePrAddress { pr_id: pr.clone(), read: PrRead::Locator, read_started_at: 3_000, verdict: confirmed("key-a", NEW) });
    assert_eq!(s.pull_requests[&pr].repository, NEW);
    assert_eq!(s.pull_requests[&pr].aliases, BTreeSet::from([OLD.to_owned()]));
}

/// An old record whose address now names another pull request: Alondra cannot tell which one it
/// is.
fn unresolvable() -> DomainState {
    let mut s = at_work();
    register(&mut s, OLD, 42, HEAD).unwrap();
    must(&mut s, Actor::System, observe(&id(OLD, 42), OLD, 1_000, PrAddressVerdict::NamesAnotherPr));
    s
}

#[test]
fn an_unresolvable_pr_offers_only_removal_and_removal_touches_nothing() {
    let pr = id(OLD, 42);
    let mut s = unresolvable();
    assert_eq!(s.pull_requests[&pr].identity,
        PrIdentity::Unresolvable { key: None, reason: UnresolvableReason::AddressReused, since: 1_000 });
    assert!(s.pull_requests[&pr].aliases.is_empty(), "an address that names another PR is never an alias");
    assert_eq!(exec(&mut s, node(), Command::DetachPr { pr_id: pr.clone(), at: 2_000 }).unwrap_err().code, "forbidden");
    let outcome = must(&mut s, user(), Command::DetachPr { pr_id: pr.clone(), at: 2_000 });
    assert_eq!(s.pull_requests[&pr].tracking, PrTracking::Detached { at: 2_000 });
    no_remote_effects(&outcome, &s);
    assert!(outcome.effects.iter().any(|e| matches!(e, DomainEffect::NotifyAgent { message, .. } if *message == detached_notice(42))), "{:?}", outcome.effects);
    let task = &s.tasks[&TaskId::from(TASK)];
    assert!(!task.state.terminal(), "the task goes on without the PR");
    assert!(s.open_prs(task).next().is_none());
    assert_eq!(exec(&mut s, user(), Command::DetachPr { pr_id: pr.clone(), at: 3_000 }).unwrap_err().code, "pr_detached");
    register(&mut s, OLD, 42, HEAD).expect("the node may register a new PR at that address");
    assert!(s.pull_requests[&PrId::from(format!("{}~2", pr.0))].is_tracked());

    let mut delivered = unresolvable();
    deliver(&mut delivered, &[pr.clone()]);
    assert!(!cards(&delivered).is_empty());
    let outcome = must(&mut delivered, user(), Command::DetachPr { pr_id: pr.clone(), at: 2_000 });
    no_remote_effects(&outcome, &delivered);
    assert!(delivered.tasks[&TaskId::from(TASK)].cancelled(), "a delivery of only that PR ends as when the PR closes unmerged");
    assert!(cards(&delivered).is_empty());

    let mut verified = at_work();
    register(&mut verified, OLD, 42, HEAD).unwrap();
    must(&mut verified, Actor::System, observe(&pr, OLD, 1_000, confirmed("key-a", OLD)));
    assert_eq!(exec(&mut verified, user(), Command::DetachPr { pr_id: pr, at: 2_000 }).unwrap_err().code, "pr_not_unresolvable");
}

#[test]
fn gone_needs_three_definitive_answers_an_hour_apart() {
    let pr = id(OLD, 42);
    let gone = |times: &[Millis]| {
        let mut s = at_work();
        register(&mut s, OLD, 42, HEAD).unwrap();
        for at in times {
            must(&mut s, Actor::System, observe(&pr, OLD, *at, PrAddressVerdict::NotFound));
        }
        s.pull_requests[&pr].identity.clone()
    };
    let start = 10 * HOUR;
    assert!(gone(&[start, start + HOUR]).is_unverified(), "two answers are not enough");
    assert!(gone(&[start, start + 10 * 60 * 1000, start + 20 * 60 * 1000]).is_unverified(), "three answers within an hour count once");
    assert!(gone(&[start, start + 30 * 60 * 1000, start + HOUR, start + HOUR + 59 * 60 * 1000]).is_unverified());
    assert_eq!(gone(&[start, start + HOUR, start + 2 * HOUR]),
        PrIdentity::Unresolvable { key: None, reason: UnresolvableReason::Gone, since: start + 2 * HOUR });

    let mut s = at_work();
    register(&mut s, OLD, 42, HEAD).unwrap();
    must(&mut s, Actor::System, observe(&pr, OLD, start, PrAddressVerdict::NotFound));
    must(&mut s, Actor::System, observe(&pr, OLD, start + HOUR, confirmed("key-a", OLD)));
    must(&mut s, Actor::System, observe(&pr, OLD, start + 2 * HOUR, PrAddressVerdict::NamesAnotherPr));
    for n in 0..3 {
        must(&mut s, Actor::System, Command::ObservePrAddress { pr_id: pr.clone(), read: PrRead::Locator, read_started_at: start + (3 + n) * HOUR,
            verdict: PrAddressVerdict::NotFound });
    }
    assert_eq!(s.pull_requests[&pr].identity, PrIdentity::Unresolvable {
        key: Some(ProviderPrKey("key-a".into())), reason: UnresolvableReason::Gone, since: start + 5 * HOUR });
}

/// The domain names no provider in the identity of a pull request: the key and the locator are
/// opaque, and what they are for one provider lives in that provider's adapter.
#[test]
fn the_pr_identity_names_no_provider() {
    const FORBIDDEN: [&str; 6] = ["github", "database", "node_id", "nodeid", "fulldatabaseid", "graphql"];
    let sources = [include_str!("pr_identity.rs"), include_str!("rules/pr_identity.rs")];
    for source in sources {
        let lower = source.to_ascii_lowercase();
        for word in FORBIDDEN {
            assert!(!lower.contains(word), "the identity of a pull request names `{word}`");
        }
    }
    let mut s = two_copies();
    must(&mut s, Actor::System, observe(&id(OLD, 42), OLD, 1_000, confirmed("key-a", NEW)));
    s.pull_requests.get_mut(&id(NEW, 42)).unwrap().missing = Some(PrMissing { strikes: 1, last: 1 });
    let values = [
        serde_json::to_value(s.pull_requests.values().collect::<Vec<_>>()).unwrap(),
        serde_json::to_value(PrIdentity::Unresolvable { key: Some(ProviderPrKey("k".into())), reason: UnresolvableReason::Gone, since: 1 }).unwrap(),
        serde_json::to_value(PrTracking::Detached { at: 1 }).unwrap(),
        serde_json::to_value(reading(&Read::Old(1))).unwrap(),
        serde_json::to_value(Command::ObservePrAddress { pr_id: id(OLD, 42), read: PrRead::Locator, read_started_at: 1, verdict: PrAddressVerdict::NamesAnotherPr }).unwrap(),
        serde_json::to_value(DomainEffect::Relocate { pr_id: id(OLD, 42), locator: ProviderLocator("l".into()) }).unwrap(),
    ];
    fn names(value: &serde_json::Value, out: &mut Vec<String>) {
        match value {
            serde_json::Value::Object(map) => {
                for (key, value) in map {
                    out.push(key.to_ascii_lowercase());
                    if key == "kind" { out.push(value.as_str().unwrap_or_default().to_ascii_lowercase()); }
                    names(value, out);
                }
            }
            serde_json::Value::Array(list) => list.iter().for_each(|value| names(value, out)),
            _ => {}
        }
    }
    let mut seen = Vec::new();
    values.iter().for_each(|value| names(value, &mut seen));
    for name in &seen {
        assert!(!FORBIDDEN.iter().any(|word| name.contains(word)), "the stored identity has a field `{name}`");
    }
    for field in ["identity", "tracking", "aliases", "address_read_at", "key", "locator", "missing"] {
        assert!(seen.iter().any(|name| name == field), "{field} is serialized");
    }
}
