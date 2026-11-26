//! A pull request's identity, its aliases and the records it joined outlive a restart, a database
//! from before identities were kept upgrades and joins its copies of one pull request, the tables
//! refuse what the domain never leaves, and metrics count a renamed repository by its new name.
//! `ana/lumo` moves to `lumo-org/lumo`; `ana/site` is another repository.
use super::*;
use crate::testing::{Fixture, agent, envelope, execute, seeded};
use std::collections::BTreeSet;

const OLD: &str = "ana/lumo";
const NEW: &str = "lumo-org/lumo";
const SITE: &str = "ana/site";
const HEAD: &str = "1111111111111111111111111111111111111111";

/// The table as it was before identities were kept, and its triggers.
const TABLE_BEFORE: &str = "CREATE TABLE pull_requests(id TEXT PRIMARY KEY CHECK(length(id)>0),task_id TEXT NOT NULL REFERENCES tasks(id),host TEXT NOT NULL,repository TEXT NOT NULL,number INTEGER NOT NULL CHECK(number>0),head TEXT NOT NULL,base TEXT NOT NULL,state TEXT NOT NULL CHECK(state IN('open','closed','merged')),merged_head TEXT,allowed_methods TEXT NOT NULL CHECK(json_valid(allowed_methods)),UNIQUE(host,repository,number),UNIQUE(task_id,id));
CREATE TRIGGER immutable_pr_ownership BEFORE UPDATE ON pull_requests
WHEN NEW.id<>OLD.id OR NEW.host<>OLD.host OR NEW.repository<>OLD.repository OR NEW.number<>OLD.number
OR (NEW.task_id<>OLD.task_id AND (SELECT state FROM tasks WHERE id=OLD.task_id) NOT IN('done','cancelled'))
BEGIN SELECT RAISE(ABORT,'PR identity and live ownership are immutable'); END;
CREATE TRIGGER retained_pr BEFORE DELETE ON pull_requests BEGIN SELECT RAISE(ABORT,'PR ownership is retained for Task lifetime'); END;";

fn user() -> Actor {
    Actor::User { user_id: "user-1".into() }
}

fn id(repository: &str, number: u64) -> PrId {
    hosted_pr_id("github.com", repository, number)
}

fn register(store: &mut DomainStore, task: &str, by: &Actor, repository: &str, number: u64) {
    execute(store, by, &format!("register-{task}-{repository}#{number}"), Command::RegisterPr {
        task_id: task.into(), pr_id: id(repository, number), host: "github.com".into(), repository: repository.into(), number,
        head: HEAD.into(), base: "main".into(), allowed_methods: vec![MergeMethod::Squash],
    });
}

fn confirmed(key: &str, current: &str) -> PrAddressVerdict {
    PrAddressVerdict::Confirmed { key: ProviderPrKey(key.into()), locator: ProviderLocator(format!("locator-{key}")), current_repository: current.into() }
}

fn observe(pr: &PrId, via: &str, at: Millis, verdict: PrAddressVerdict) -> Command {
    Command::ObservePrAddress { pr_id: pr.clone(), read: PrRead::Address { repository: via.into() }, read_started_at: at, verdict }
}

fn count(store: &DomainStore, sql: &str) -> i64 {
    store.conn.query_row(sql, [], |row| row.get(0)).unwrap()
}

fn write(store: &DomainStore, before: &DomainState, after: &DomainState) {
    let tx = rusqlite::Transaction::new_unchecked(&store.conn, TransactionBehavior::Immediate).unwrap();
    let scope = alondra_store::AuthorityScope::new(epoch_of(&store.conn).unwrap()).unwrap();
    let mut journal = alondra_store::LifecycleTransaction::attach(tx, scope).unwrap();
    repository::save_changes(&store.conn, &mut journal, before, after).unwrap();
    journal.commit().unwrap();
}

fn decision(at: Revision) -> PrDecision {
    PrDecision { presentation_id: "delivery-1".into(), head: HEAD.into(), accepted: true, user_id: "user-1".into(), at_revision: at }
}

/// task-1 registered its PR at the old address and again, after the move, at the new one, and
/// delivered both with another PR; the copy at the new address carries decisions, a go-ahead, a merge in flight,
/// the card's subject, a reply, checklist evidence and the stack's readiness.
fn two_copies(store: &mut DomainStore) -> (PrId, PrId) {
    seeded(store);
    let (kept, copy) = (id(OLD, 42), id(NEW, 42));
    register(store, "task-1", &agent(), OLD, 42);
    register(store, "task-1", &agent(), NEW, 42);
    register(store, "task-1", &agent(), SITE, 5);
    execute(store, &agent(), "present", Command::PresentResult {
        checklist: vec![], task_id: "task-1".into(), interaction_id: "delivery-1".into(), body: "Review the change".into(),
        targets: [&kept, &copy, &id(SITE, 5)].map(|pr| DeliveryTarget::PullRequest { pr_id: pr.clone(), head: HEAD.into() }).to_vec(),
        is_final: true, nothing_to_review: false, attachments: vec![], card: CardSpec::delivery("Review the change", &[], true),
    });
    let before = store.load().unwrap();
    let mut after = DomainState::clone(&before);
    after.pull_requests.get_mut(&kept).unwrap().review = Some(PullRequestReview { decisions: vec![decision(3)] });
    after.pull_requests.get_mut(&copy).unwrap().review = Some(PullRequestReview { decisions: vec![decision(1), decision(5)] });
    for (authorization, attempt) in [("same-head", MergeAttemptState::Unclaimed), ("in-flight", MergeAttemptState::Claimed)] {
        after.merge_authorizations.insert(authorization.into(), MergeAuthorization {
            id: authorization.into(), task_id: "task-1".into(), pr_id: copy.clone(), presentation_id: "delivery-1".into(), head: HEAD.into(),
            method: MergeMethod::Squash, user_id: "user-1".into(), valid: true, attempt, claimed_by: None, at_revision: 7, despite_pending_message: false,
        });
    }
    let delivery = after.interactions.get_mut(&"delivery-1".into()).unwrap();
    delivery.subject = Some(InteractionSubject::PullRequest { pr_id: copy.clone() });
    delivery.card.blocks.push(CardTarget::PullRequest { pr_id: copy.clone() });
    let InteractionPayload::Delivery { feedback, .. } = &mut delivery.payload else { panic!("a delivery") };
    feedback.push(Feedback { target: FeedbackTarget::PullRequest { pr_id: copy.clone(), head: HEAD.into() }, reply_window_start: None,
        attachments: vec![], user_id: "user-1".into(), body: "Explain".into(), at_revision: 1, instruction: Instruction::Question, merge_failure: None });
    after.checklist_marks.insert("delivery-1".into(), vec![ChecklistMark { item: "1".into(),
        evidence: vec![Evidence { kind: EvidenceKind::Pr, value: copy.0.clone(), detail: None }] }]);
    after.stack_readiness.insert("task-1".into(), StackReadiness { task_id: "task-1".into(), presentation_id: "delivery-1".into(), pr_id: copy.clone(), head: HEAD.into(), ready: true });
    write(store, &before, &after);
    let stored = store.load().unwrap();
    assert_eq!((&stored.pull_requests, &stored.interactions, &stored.merge_authorizations, &stored.checklist_marks, &stored.stack_readiness),
        (&after.pull_requests, &after.interactions, &after.merge_authorizations, &after.checklist_marks, &after.stack_readiness),
        "the references to the copy are stored");
    (kept, copy)
}

/// What task-1 keeps once the copy joined the record at the old address.
fn assert_joined(store: &mut DomainStore, kept: &PrId, copy: &PrId) {
    let state = store.load().unwrap();
    let (survivor, joined) = (&state.pull_requests[kept], &state.pull_requests[copy]);
    assert!(survivor.is_tracked());
    assert_eq!(survivor.repository, NEW);
    assert_eq!(survivor.aliases, BTreeSet::from([OLD.to_owned()]));
    assert_eq!(survivor.identity, PrIdentity::Verified { key: ProviderPrKey("key-a".into()), locator: ProviderLocator("locator-key-a".into()) });
    assert_eq!(joined.tracking, PrTracking::Superseded { by: kept.clone() });
    assert_eq!(survivor.review.as_ref().unwrap().decisions, [decision(1), decision(3), decision(5)], "no decision is lost");
    let rows = |sql: &str| -> Vec<String> {
        store.conn.prepare(sql).unwrap().query_map([], |row| row.get(0)).unwrap().collect::<rusqlite::Result<_>>().unwrap()
    };
    assert_eq!(rows("SELECT pr_id FROM delivery_targets WHERE interaction_id='delivery-1' ORDER BY ordinal"), [kept.0.clone(), id(SITE, 5).0]);
    assert_eq!(rows("SELECT pr_id FROM pr_decisions ORDER BY pr_id,ordinal"), [kept.0.clone(), kept.0.clone(), kept.0.clone()]);
    assert_eq!(rows("SELECT DISTINCT pr_id FROM merge_authorizations"), [kept.0.clone()]);
    assert_eq!(rows("SELECT pr_id FROM interaction_subjects WHERE interaction_id='delivery-1'"), [kept.0.clone()]);
    assert_eq!(rows("SELECT pr_id FROM stack_readiness"), [kept.0.clone()]);
    assert_eq!(rows("SELECT json_extract(target,'$.pr_id') FROM delivery_feedback"), [kept.0.clone()]);
    assert_eq!(rows("SELECT json_extract(marks,'$[0].evidence[0].value') FROM delivery_checklists"), [kept.0.clone()]);
    assert_eq!(rows("SELECT repository FROM pull_request_aliases"), [OLD.to_owned()]);
    assert_eq!(count(store, "SELECT count(*) FROM pragma_foreign_key_check"), 0);
}

#[test]
fn pr_identity_survives_a_restart() {
    let fixture = Fixture::new();
    let mut store = DomainStore::open(&fixture.path(), &alondra_domain_rules::Rules).unwrap();
    let (kept, copy) = two_copies(&mut store);
    let site = id(SITE, 5);

    execute(&mut store, &Actor::System, "observe-old", observe(&kept, OLD, 1_000, confirmed("key-a", NEW)));
    execute(&mut store, &Actor::System, "observe-site", observe(&site, SITE, 1_000, PrAddressVerdict::NamesAnotherPr));
    execute(&mut store, &user(), "detach-site", Command::DetachPr { pr_id: site.clone(), at: 2_000 });
    let committed = store.load().unwrap();
    drop(store);

    let mut store = DomainStore::open(&fixture.path(), &alondra_domain_rules::Rules).unwrap();
    assert_eq!(store.load().unwrap(), committed, "a restart reads the same identities");
    assert_joined(&mut store, &kept, &copy);
    assert_eq!(committed.pull_requests[&site].tracking, PrTracking::Detached { at: 2_000 });
    assert!(matches!(committed.pull_requests[&site].identity, PrIdentity::Unresolvable { reason: UnresolvableReason::AddressReused, .. }));

    for (name, pr, via) in [("again-old", &kept, OLD), ("again-copy", &copy, NEW)] {
        let again = envelope(&mut store, name, observe(pr, via, 1_000, confirmed("key-a", NEW)));
        match store.execute_authenticated(&Actor::System, &again, None) {
            Ok(commit) => assert!(!commit.outcome.changed, "{name} changed {:?}", commit.outcome),
            Err(StoreError::Domain(e)) => assert_eq!(e.code, "no_effect", "{name}: {e}"),
            Err(other) => panic!("{name}: {other}"),
        }
    }
    assert_eq!(store.load().unwrap().pull_requests, committed.pull_requests, "applying the same reads again changes nothing");
}

/// The database as it was before identities were kept: the table of that shape, no aliases.
fn to_the_shape_before_identities(path: &std::path::Path) {
    let conn = Connection::open(path).unwrap();
    conn.execute_batch(&format!("PRAGMA foreign_keys=OFF;
        CREATE TABLE copied AS SELECT id,task_id,host,repository,number,head,base,state,merged_head,allowed_methods FROM pull_requests;
        DROP TABLE pull_request_aliases; DROP TABLE pull_requests; {TABLE_BEFORE}
        INSERT INTO pull_requests SELECT * FROM copied; DROP TABLE copied;")).unwrap();
}

#[test]
fn an_old_database_with_duplicate_records_upgrades_and_joins() {
    let fixture = Fixture::new();
    let mut store = DomainStore::open(&fixture.path(), &alondra_domain_rules::Rules).unwrap();
    let (kept, copy) = two_copies(&mut store);
    let before = store.load().unwrap();
    drop(store);
    to_the_shape_before_identities(&fixture.path());

    let mut store = DomainStore::open(&fixture.path(), &alondra_domain_rules::Rules).unwrap();
    let upgraded = store.load().unwrap();
    assert_eq!(upgraded, before, "both records load as they were");
    for pr in [&kept, &copy] {
        let record = &upgraded.pull_requests[pr];
        assert!(record.identity.is_unverified() && record.is_tracked() && record.aliases.is_empty(), "{record:?}");
    }
    assert_eq!(count(&store, "SELECT count(*) FROM pragma_table_info('pull_requests') WHERE name IN('provider_key','provider_locator')"), 2);
    assert_eq!(count(&store, "SELECT count(*) FROM pragma_foreign_key_check"), 0);

    execute(&mut store, &Actor::System, "observe-old", observe(&kept, OLD, 1_000, confirmed("key-a", NEW)));
    assert_joined(&mut store, &kept, &copy);
    let state = store.load().unwrap();
    let tracked: Vec<PrId> = state.pull_requests.values().filter(|pr| pr.is_tracked() && pr.number == 42).map(|pr| pr.id.clone()).collect();
    assert_eq!(tracked, [kept], "one record of the PR stays");
}

fn refused(store: &DomainStore, sql: &str) -> bool {
    store.conn.execute_batch("SAVEPOINT probe").unwrap();
    let refused = store.conn.execute_batch(sql).is_err();
    store.conn.execute_batch("ROLLBACK TO probe; RELEASE probe").unwrap();
    refused
}

#[test]
fn the_schema_refuses_invalid_identity_combinations() {
    let fixture = Fixture::new();
    let mut store = DomainStore::open(&fixture.path(), &alondra_domain_rules::Rules).unwrap();
    seeded(&mut store);
    register(&mut store, "task-1", &agent(), OLD, 42);
    register(&mut store, "task-1", &agent(), SITE, 5);
    let (lumo, site) = (id(OLD, 42), id(SITE, 5));
    let set = |pr: &PrId, columns: &str| format!("UPDATE pull_requests SET {columns} WHERE id='{}';", pr.0);
    let verified = |key: &str| format!("identity='verified',provider_key='{key}',provider_locator='locator-{key}'");

    assert!(refused(&store, &set(&lumo, &format!("tracking='superseded',superseded_by='{}'", site.0))), "a joined record is a verified one");
    assert!(refused(&store, &set(&lumo, "tracking='detached',detached_at=1")), "only a record Alondra cannot check is removed");
    assert!(refused(&store, &set(&lumo, "identity='verified'")), "a verified record has its key and locator");
    assert!(refused(&store, &set(&lumo, "missing_strikes=1")), "a strike says when it was read");

    store.conn.execute_batch(&set(&lumo, &verified("key-a"))).unwrap();
    assert!(refused(&store, &set(&lumo, "provider_key='key-b'")), "a key never changes once known");
    assert!(refused(&store, &set(&site, &verified("key-a"))), "two tracked records never share a key");
    assert!(refused(&store, &set(&lumo, &format!("repository='{NEW}'"))), "an address moves only from one kept as an alias");
    assert!(refused(&store, &format!("INSERT INTO pull_requests(id,task_id,host,repository,number,head,base,state,allowed_methods)
        VALUES('again','task-1','github.com','{OLD}',42,'{HEAD}','main','open','[]');")), "two tracked records never share an address");

    store.conn.execute_batch(&format!("INSERT INTO pull_request_aliases(pr_id,repository) VALUES('{}','{OLD}');", lumo.0)).unwrap();
    store.conn.execute_batch(&set(&lumo, &format!("repository='{NEW}'"))).unwrap();
    assert!(refused(&store, &format!("DELETE FROM pull_request_aliases WHERE pr_id='{}';", lumo.0)), "an alias is kept");
    assert!(refused(&store, &format!("UPDATE pull_request_aliases SET repository='ana/other' WHERE pr_id='{}';", lumo.0)), "an alias never changes");

    store.conn.execute_batch(&set(&site, "identity='unresolvable',unresolvable_reason='gone',unresolvable_since=1")).unwrap();
    store.conn.execute_batch(&set(&site, "tracking='detached',detached_at=2")).unwrap();
    assert!(refused(&store, &set(&site, "tracking='tracked',detached_at=NULL")), "a removed record stays removed");
    assert!(!refused(&store, "SELECT 1;"), "the probe itself passes");
}

/// task-1 registered `ana/lumo#42`, was cancelled and its closed pull request ended it; its
/// metrics are folded under the project `lumo`. task-2 is a live task of another agent.
fn ended_task_in_lumo(store: &mut DomainStore) {
    seeded(store);
    execute(store, &Actor::Orchestrator { generation: 0 }, "title", Command::TitleRequest { request_id: "request-1".into(), title: "Lumo".into() });
    execute(store, &Actor::System, "agent-2", Command::RegisterAgent { agent_id: "agent-2".into(), name: "Second".into(), at: 0 });
    execute(store, &Actor::Orchestrator { generation: 0 }, "task-2", Command::CreateTask { priority: Default::default(), plan_items: None, continues: None,
        task_id: "task-2".into(), request_id: "request-1".into(), title: "Later".into(), agent_id: Some("agent-2".into()), review_prs: true,
        dependencies: BTreeSet::new(), stack: None, launch: Some(LaunchPlan::new("node", "/synthetic/repository")),
    });
    register(store, "task-1", &agent(), OLD, 42);
    execute(store, &user(), "cancel", Command::CancelTask { task_id: "task-1".into(), reason: None });
    execute(store, &Actor::System, "node-end", Command::RecordNodeEnd { task_id: "task-1".into(), end: NodeEnd::Closed, agent_id: None });
    execute(store, &Actor::System, "closed", Command::ObservePrState { pr_id: id(OLD, 42), head: HEAD.into(), base: "main".into(), state: PrState::Closed, merged_head: None });
    assert!(store.load().unwrap().tasks[&TaskId::from("task-1")].state.terminal());
    assert_eq!(store.fold_ended_tasks(10).unwrap(), 1);
}

fn metrics_project(store: &DomainStore) -> Option<String> {
    store.conn.query_row("SELECT project FROM task_metrics WHERE task_id='task-1'", [], |row| row.get(0)).unwrap()
}

#[test]
fn metrics_follow_the_current_repository_name() {
    let fixture = Fixture::new();
    let mut store = DomainStore::open(&fixture.path(), &alondra_domain_rules::Rules).unwrap();
    ended_task_in_lumo(&mut store);
    assert_eq!(metrics_project(&store).as_deref(), Some("lumo"));

    let renamed = "ana/lumen";
    register(&mut store, "task-2", &Actor::Agent { agent_id: "agent-2".into() }, renamed, 42);
    execute(&mut store, &Actor::System, "observe", observe(&id(OLD, 42), OLD, 1_000, confirmed("key-a", renamed)));
    let state = store.load().unwrap();
    assert_eq!(state.pull_requests[&id(OLD, 42)].tracking, PrTracking::Superseded { by: id(renamed, 42) }, "the live task's record stays");
    assert_eq!(metrics_project(&store).as_deref(), Some("lumen"), "the ended task counts under the repository's new name");
}

/// task-1's record, found at `ana/lumo#42` and gone: three answers an hour apart, by its locator.
fn gone(store: &mut DomainStore, pr: &PrId) {
    for step in 0..3 {
        let at = 10_000 + step * STRIKE_GAP;
        execute(store, &Actor::System, &format!("gone-{step}"), Command::ObservePrAddress { pr_id: pr.clone(), read: PrRead::Locator, read_started_at: at, verdict: PrAddressVerdict::NotFound });
    }
}

/// Design §5.4: a joined copy follows its survivor when that one joins a later record.
#[test]
fn review_the_schema_lets_a_joined_record_follow_its_survivor() {
    let fixture = Fixture::new();
    let mut store = DomainStore::open(&fixture.path(), &alondra_domain_rules::Rules).unwrap();
    seeded(&mut store);
    for (number, repository) in [(1, OLD), (2, NEW), (3, SITE)] {
        register(&mut store, "task-1", &agent(), repository, number);
    }
    let (copy, survivor, later) = (id(OLD, 1), id(NEW, 2), id(SITE, 3));
    let set = |pr: &PrId, columns: &str| format!("UPDATE pull_requests SET {columns} WHERE id='{}';", pr.0);
    let verified = "identity='verified',provider_key='key-a',provider_locator='locator-key-a'";
    store.conn.execute_batch(&set(&survivor, verified)).unwrap();
    store.conn.execute_batch(&set(&copy, &format!("{verified},tracking='superseded',superseded_by='{}'", survivor.0))).unwrap();
    store.conn.execute_batch(&set(&survivor, &format!("tracking='superseded',superseded_by='{}'", later.0))).unwrap();
    store.conn.execute_batch(&set(&later, verified)).unwrap();

    store.conn.execute_batch(&set(&copy, &format!("superseded_by='{}'", later.0))).unwrap();

    assert_eq!(count(&store, &format!("SELECT count(*) FROM pull_requests WHERE superseded_by='{}'", later.0)), 2);
    assert!(refused(&store, &set(&copy, "tracking='tracked',superseded_by=NULL")), "a joined record never tracks again");
    assert!(refused(&store, &set(&copy, "tracking='detached',superseded_by=NULL,detached_at=1")), "a joined record is never removed");
}

#[test]
fn review_the_survivor_of_a_join_can_be_removed() {
    let fixture = Fixture::new();
    let mut store = DomainStore::open(&fixture.path(), &alondra_domain_rules::Rules).unwrap();
    let (kept, copy) = two_copies(&mut store);
    execute(&mut store, &Actor::System, "observe-old", observe(&kept, OLD, 1_000, confirmed("key-a", NEW)));
    assert_joined(&mut store, &kept, &copy);
    gone(&mut store, &kept);

    execute(&mut store, &user(), "detach", Command::DetachPr { pr_id: kept.clone(), at: 20_000_000 });

    let committed = store.load().unwrap();
    assert_eq!(committed.pull_requests[&kept].tracking, PrTracking::Detached { at: 20_000_000 });
    assert_eq!(committed.pull_requests[&copy].tracking, PrTracking::Superseded { by: kept.clone() }, "the copy still follows it");
    drop(store);
    let mut store = DomainStore::open(&fixture.path(), &alondra_domain_rules::Rules).unwrap();
    assert_eq!(store.load().unwrap(), committed, "a restart reads the same records");
}

#[test]
fn review_a_displaced_record_can_still_be_found_by_its_locator() {
    let fixture = Fixture::new();
    let mut store = DomainStore::open(&fixture.path(), &alondra_domain_rules::Rules).unwrap();
    seeded(&mut store);
    register(&mut store, "task-1", &agent(), OLD, 42);
    let lumo = id(OLD, 42);
    execute(&mut store, &Actor::System, "observe", observe(&lumo, OLD, 1_000, confirmed("key-a", OLD)));
    gone(&mut store, &lumo);
    let committed = store.load().unwrap();
    let identity = &committed.pull_requests[&lumo].identity;
    assert!(identity.is_unresolvable());
    assert_eq!(identity.locator(), Some(&ProviderLocator("locator-key-a".into())));
    assert_eq!(identity.key(), Some(&ProviderPrKey("key-a".into())));
    drop(store);

    let mut store = DomainStore::open(&fixture.path(), &alondra_domain_rules::Rules).unwrap();
    assert_eq!(store.load().unwrap(), committed, "the locator survives a restart");
}

/// The table as it was when a record Alondra cannot check kept no locator.
#[test]
fn review_a_database_that_kept_no_locator_for_unresolvable_records_upgrades() {
    let fixture = Fixture::new();
    let mut store = DomainStore::open(&fixture.path(), &alondra_domain_rules::Rules).unwrap();
    seeded(&mut store);
    register(&mut store, "task-1", &agent(), OLD, 42);
    register(&mut store, "task-1", &agent(), SITE, 5);
    execute(&mut store, &Actor::System, "observe", observe(&id(OLD, 42), OLD, 1_000, confirmed("key-a", OLD)));
    drop(store);
    let declared: String = Connection::open(fixture.path()).unwrap()
        .query_row("SELECT sql FROM sqlite_schema WHERE type='table' AND name='pull_requests'", [], |row| row.get(0)).unwrap();
    let before = declared.replace("ELSE unresolvable_reason IS NOT NULL", "ELSE provider_locator IS NULL AND unresolvable_reason IS NOT NULL");
    assert_ne!(before, declared);
    Connection::open(fixture.path()).unwrap().execute_batch(&format!("PRAGMA foreign_keys=OFF;
        CREATE TABLE copied AS SELECT * FROM pull_requests; DROP TABLE pull_requests; {before};
        INSERT INTO pull_requests SELECT * FROM copied; DROP TABLE copied;")).unwrap();

    let mut store = DomainStore::open(&fixture.path(), &alondra_domain_rules::Rules).unwrap();
    let stored: String = store.conn.query_row("SELECT sql FROM sqlite_schema WHERE type='table' AND name='pull_requests'", [], |row| row.get(0)).unwrap();
    assert_eq!(stored, declared, "the table is the one the schema declares");
    assert!(store.load().unwrap().pull_requests[&id(OLD, 42)].identity.is_verified(), "its records are kept");
    gone(&mut store, &id(OLD, 42));
    assert!(store.load().unwrap().pull_requests[&id(OLD, 42)].identity.locator().is_some());
    assert_eq!(count(&store, "SELECT count(*) FROM pragma_foreign_key_check"), 0);
}

#[test]
fn review_removing_a_pr_keeps_the_project_of_its_tasks_metrics() {
    let fixture = Fixture::new();
    let mut store = DomainStore::open(&fixture.path(), &alondra_domain_rules::Rules).unwrap();
    ended_task_in_lumo(&mut store);
    let lumo = id(OLD, 42);
    execute(&mut store, &Actor::System, "reused", observe(&lumo, OLD, 1_000, PrAddressVerdict::NamesAnotherPr));
    assert!(store.load().unwrap().pull_requests[&lumo].identity.is_unresolvable());

    execute(&mut store, &user(), "detach", Command::DetachPr { pr_id: lumo.clone(), at: 2_000 });

    assert!(matches!(store.load().unwrap().pull_requests[&lumo].tracking, PrTracking::Detached { .. }));
    assert_eq!(metrics_project(&store).as_deref(), Some("lumo"), "the task still counts under the project it had");
}

#[test]
fn review_weeks_older_than_90_days_count_under_the_current_name() {
    let fixture = Fixture::new();
    let mut store = DomainStore::open(&fixture.path(), &alondra_domain_rules::Rules).unwrap();
    ended_task_in_lumo(&mut store);
    let finished: i64 = store.conn.query_row("SELECT finished_at FROM task_metrics WHERE task_id='task-1'", [], |row| row.get(0)).unwrap();
    let tx = store.conn.unchecked_transaction().unwrap();
    assert_eq!(crate::testing::tasks_into_weeks(&tx, finished + 91 * crate::testing::DAY_MS, 10).unwrap(), 1);
    tx.commit().unwrap();
    let weeks = |store: &DomainStore| -> Vec<(String, i64)> {
        store.conn.prepare("SELECT project,sum(tasks) FROM metric_weeks GROUP BY project").unwrap()
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?))).unwrap().collect::<rusqlite::Result<_>>().unwrap()
    };
    assert_eq!(weeks(&store), [("lumo".to_owned(), 1)]);

    let renamed = "ana/lumen";
    register(&mut store, "task-2", &Actor::Agent { agent_id: "agent-2".into() }, renamed, 42);
    execute(&mut store, &Actor::System, "observe", observe(&id(OLD, 42), OLD, 1_000, confirmed("key-a", renamed)));

    assert_eq!(weeks(&store), [("lumen".to_owned(), 1)], "the old weeks count under the new name");
}
