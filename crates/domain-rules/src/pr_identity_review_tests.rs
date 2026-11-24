//! Review reproductions for the stable PR identity. Each test states the behaviour the approved
//! design asks for; a failure is a finding.
use super::*;
use std::collections::BTreeSet;

const AGENT: &str = "a";
const TASK: &str = "t";
const OLD: &str = "ana/lumo";
const NEW: &str = "lumo-org/lumo";
const SITE: &str = "ana/site";
const HEAD: &str = "1111111111111111111111111111111111111111";
const SITE_HEAD: &str = "2222222222222222222222222222222222222222";

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
    exec(s, Actor::Agent { agent_id: AGENT.into() }, Command::RegisterPr {
        task_id: TASK.into(), pr_id: id(repository, number), host: "github.com".into(), repository: repository.into(),
        number, head: head.into(), base: "main".into(), allowed_methods: vec![MergeMethod::Squash],
    })
}

fn confirmed(key: &str, current: &str) -> PrAddressVerdict {
    PrAddressVerdict::Confirmed { key: ProviderPrKey(key.into()), locator: ProviderLocator(format!("locator-{key}")), current_repository: current.into() }
}

fn observe(pr: &PrId, via: &str, at: Millis, verdict: PrAddressVerdict) -> Command {
    Command::ObservePrAddress { pr_id: pr.clone(), read: PrRead::Address { repository: via.into() }, read_started_at: at, verdict }
}

/// Design §5.2 and §5.10: an unchecked record at an address is that address's pull request only
/// when its branch or head match. `ana/lumo` moved away before any read; `ana/site` was then
/// renamed into `ana/lumo`. A read of the `ana/site` record must not hand its key to the
/// unchecked `ana/lumo` record and join two different pull requests.
#[test]
fn an_unchecked_record_is_not_joined_into_another_pr_renamed_into_its_address() {
    let mut s = at_work();
    let (lumo, site) = (id(OLD, 42), id(SITE, 42));
    register(&mut s, OLD, 42, HEAD).unwrap();
    register(&mut s, SITE, 42, SITE_HEAD).unwrap();

    must(&mut s, Actor::System, observe(&site, SITE, 2_000, confirmed("key-b", OLD)));

    let kept = &s.pull_requests[&lumo];
    assert!(kept.is_tracked(), "the ana/lumo record joined another pull request: {:?}", kept.tracking);
    assert_ne!(kept.identity.key(), Some(&ProviderPrKey("key-b".into())), "the ana/lumo record took another pull request's key");
}

/// Design §5.2 (row «Resultado de ResolveByNode: Found»): a verified pull request whose old name
/// another repository took is found again by its locator. The engine looks for a record by its
/// locator only while `relocation_asked_at` is set and `identity.locator()` is some
/// (`crates/engine-github/src/pr_identity.rs`, `relocate`).
#[test]
fn a_verified_record_displaced_from_its_address_can_still_be_found_by_its_locator() {
    let mut s = at_work();
    let (lumo, site) = (id(OLD, 42), id(SITE, 42));
    register(&mut s, OLD, 42, HEAD).unwrap();
    must(&mut s, Actor::System, observe(&lumo, OLD, 1_000, confirmed("key-a", OLD)));
    register(&mut s, SITE, 42, SITE_HEAD).unwrap();

    must(&mut s, Actor::System, observe(&site, SITE, 2_000, confirmed("key-b", OLD)));

    let displaced = &s.pull_requests[&lumo];
    let engine_relocates_it = displaced.is_tracked() && displaced.relocation_asked_at.is_some() && displaced.identity.locator().is_some();
    assert!(engine_relocates_it, "nothing will ever look for the displaced record: {:?}", displaced.identity);
}

/// Design §5.9 and §5.10: an alias is an address GitHub confirmed was this pull request. After
/// `ana/lumo` moved to `lumo-org/lumo` and someone created a new `ana/lumo`, a new pull request #5
/// there is not the merged #5 of a finished task.
#[test]
fn a_new_pr_at_a_reused_old_name_is_not_the_finished_task_pr_with_that_alias() {
    let mut s = at_work();
    let old = id(OLD, 5);
    register(&mut s, OLD, 5, HEAD).unwrap();
    must(&mut s, Actor::System, observe(&old, OLD, 1_000, confirmed("key-old-5", NEW)));
    must(&mut s, user(), Command::CancelTask { task_id: TASK.into(), reason: None });
    must(&mut s, Actor::System, Command::RecordNodeEnd { task_id: TASK.into(), end: NodeEnd::Closed, agent_id: None });
    assert!(s.tasks[&TaskId::from(TASK)].state.terminal(), "{:?}", s.tasks[&TaskId::from(TASK)].state);
    must(&mut s, Actor::System, Command::RegisterAgent { agent_id: "b".into(), name: "Node B".into(), at: 0 });
    must(&mut s, user(), Command::CreateRequest { request_id: "r2".into(), body: "More work".into(), auto_merge: false, maintenance: false, message: false });
    must(&mut s, Actor::Orchestrator { generation: 0 }, Command::CreateTask { priority: Default::default(), plan_items: None, continues: None,
        task_id: "t2".into(), request_id: "r2".into(), title: "New work".into(), agent_id: Some("b".into()), review_prs: false,
        dependencies: BTreeSet::new(), stack: None, launch: Some(LaunchPlan::new("node", "/synthetic/repository")),
    });

    must(&mut s, Actor::Agent { agent_id: "b".into() }, Command::RegisterPr {
        task_id: "t2".into(), pr_id: id(OLD, 5), host: "github.com".into(), repository: OLD.into(),
        number: 5, head: SITE_HEAD.into(), base: "main".into(), allowed_methods: vec![MergeMethod::Squash],
    });

    assert_eq!(s.pull_requests[&old].task_id, TaskId::from(TASK), "the finished task's PR, now at {NEW}, was taken by the new task");
    let t2 = &s.tasks[&TaskId::from("t2")];
    let mine: Vec<&PullRequest> = s.task_prs(t2).collect();
    assert_eq!(mine.len(), 1);
    assert_eq!(mine[0].repository, OLD, "the new task's PR is the one at the address its node named: {:?}", mine[0]);
}

/// Design §5.3 and T7: any order of the same reads ends in the same state. A slow read that
/// started first and confirmed the record, and a later one that found another pull request at
/// that address.
#[test]
fn a_slow_confirmation_and_a_later_names_another_pr_give_the_same_state_in_any_order() {
    let pr = id(OLD, 42);
    let run = |reads: &[(Millis, PrAddressVerdict)]| {
        let mut s = at_work();
        register(&mut s, OLD, 42, HEAD).unwrap();
        for (at, verdict) in reads {
            must(&mut s, Actor::System, observe(&pr, OLD, *at, verdict.clone()));
        }
        let pr = &s.pull_requests[&pr];
        (pr.identity.clone(), pr.repository.clone(), pr.relocation_asked_at)
    };
    let slow = (1_000, confirmed("key-a", OLD));
    let later = (2_000, PrAddressVerdict::NamesAnotherPr);
    assert_eq!(run(&[later.clone(), slow.clone()]), run(&[slow, later]));
}

/// Design §5.6 and §5.7: Alondra never acts on GitHub for a pull request it cannot check, and its
/// card offers only removal. A go-ahead to merge it can never run, so it is refused when given.
#[test]
fn a_merge_go_ahead_for_a_pr_alondra_cannot_check_is_refused() {
    let mut s = at_work();
    let pr = id(OLD, 42);
    register(&mut s, OLD, 42, HEAD).unwrap();
    must(&mut s, Actor::Agent { agent_id: AGENT.into() }, Command::PresentResult {
        checklist: Vec::new(), attachments: vec![], task_id: TASK.into(), interaction_id: "final".into(), body: "Done".into(),
        targets: vec![DeliveryTarget::PullRequest { pr_id: pr.clone(), head: String::new() }], is_final: true, nothing_to_review: false,
        card: CardSpec::delivery("Review the result", &[], true),
    });
    must(&mut s, Actor::System, Command::RecordTurnBoundary { task_id: TASK.into(), generation: 1, turn: "turn-1".into(), failure: None, failed: None });
    must(&mut s, Actor::System, observe(&pr, OLD, 1_000, PrAddressVerdict::NamesAnotherPr));
    assert!(s.pull_requests[&pr].identity.is_unresolvable());

    let outcome = exec(&mut s, user(), Command::AuthorizeMerge { authorization_id: "go".into(), presentation_id: "final".into(), pr_id: pr,
        expected_head: HEAD.into(), method: MergeMethod::Squash, despite_pending_message: false });
    assert!(outcome.is_err(), "a merge that can never run was accepted: {:?}", s.merge_authorizations.values().collect::<Vec<_>>());
}

const NEXT: &str = "lumo-org/lumo-next";
const PUSHED: &str = "3333333333333333333333333333333333333333";

/// The task registered `ana/lumo#42` and `lumo-org/lumo#42`, and the read of the first one joins
/// the second into it.
fn joined_copy(s: &mut DomainState) -> (PrId, PrId) {
    let (kept, copy) = (id(OLD, 42), id(NEW, 42));
    register(s, OLD, 42, HEAD).unwrap();
    register(s, NEW, 42, HEAD).unwrap();
    must(s, Actor::System, observe(&kept, OLD, 1_000, confirmed("key-a", NEW)));
    assert_eq!(s.pull_requests[&copy].tracking, PrTracking::Superseded { by: kept.clone() });
    (kept, copy)
}

/// Design §5.4: a record that took a copy in joins a later record of the same pull request, and
/// the copy follows the record that stays.
#[test]
fn a_record_that_took_a_copy_in_can_join_a_later_one() {
    let mut s = at_work();
    let (kept, copy) = joined_copy(&mut s);
    must(&mut s, Actor::System, Command::RegisterAgent { agent_id: "b".into(), name: "Node B".into(), at: 0 });
    must(&mut s, user(), Command::CreateRequest { request_id: "r2".into(), body: "More work".into(), auto_merge: false, maintenance: false, message: false });
    must(&mut s, Actor::Orchestrator { generation: 0 }, Command::CreateTask { priority: Default::default(), plan_items: None, continues: None,
        task_id: "t2".into(), request_id: "r2".into(), title: "New work".into(), agent_id: Some("b".into()), review_prs: false,
        dependencies: BTreeSet::new(), stack: None, launch: Some(LaunchPlan::new("node", "/synthetic/repository")),
    });
    must(&mut s, user(), Command::CancelTask { task_id: TASK.into(), reason: None });
    must(&mut s, Actor::System, Command::RecordNodeEnd { task_id: TASK.into(), end: NodeEnd::Closed, agent_id: None });
    let later = id(NEXT, 42);
    must(&mut s, Actor::Agent { agent_id: "b".into() }, Command::RegisterPr {
        task_id: "t2".into(), pr_id: later.clone(), host: "github.com".into(), repository: NEXT.into(),
        number: 42, head: SITE_HEAD.into(), base: "main".into(), allowed_methods: vec![MergeMethod::Squash],
    });

    must(&mut s, Actor::System, observe(&later, NEXT, 2_000, confirmed("key-a", NEXT)));

    assert_eq!(s.pull_requests[&kept].tracking, PrTracking::Superseded { by: later.clone() });
    assert_eq!(s.pull_requests[&copy].tracking, PrTracking::Superseded { by: later.clone() }, "the copy follows the record that stays");
    assert_eq!(s.surviving_pr(&copy), &later);
}

/// Design §5.7: the only action of a card for a record Alondra cannot check works on a record
/// that took copies in.
#[test]
fn the_survivor_of_a_join_can_be_removed_from_alondra() {
    let mut s = at_work();
    let (kept, copy) = joined_copy(&mut s);
    for at in [10_000, 10_000 + STRIKE_GAP, 10_000 + 2 * STRIKE_GAP] {
        must(&mut s, Actor::System, Command::ObservePrAddress { pr_id: kept.clone(), read: PrRead::Locator, read_started_at: at, verdict: PrAddressVerdict::NotFound });
    }
    assert!(s.pull_requests[&kept].identity.is_unresolvable());

    must(&mut s, user(), Command::DetachPr { pr_id: kept.clone(), at: 20_000_000 });

    assert!(matches!(s.pull_requests[&kept].tracking, PrTracking::Detached { .. }));
    assert_eq!(s.surviving_pr(&copy), &kept);
}

/// Design §5.9: registering an address names the record that lives there; a record that only has
/// that address as an old alias is not changed by it.
#[test]
fn registering_by_address_prefers_the_record_living_there_over_an_alias() {
    let mut s = at_work();
    let (lumo, site) = (id(OLD, 42), id(SITE, 42));
    register(&mut s, OLD, 42, HEAD).unwrap();
    register(&mut s, SITE, 42, SITE_HEAD).unwrap();
    must(&mut s, Actor::System, observe(&lumo, OLD, 1_000, confirmed("key-a", NEW)));
    must(&mut s, Actor::System, observe(&site, SITE, 2_000, confirmed("key-b", OLD)));
    assert_eq!((s.pull_requests[&lumo].repository.as_str(), s.pull_requests[&site].repository.as_str()), (NEW, OLD));

    register(&mut s, OLD, 42, PUSHED).unwrap();

    assert_eq!((s.pull_requests[&lumo].head.as_str(), s.pull_requests[&site].head.as_str()), (HEAD, PUSHED));
}
