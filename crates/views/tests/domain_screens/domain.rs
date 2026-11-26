use alondra_domain_model::*;
use alondra_domain_rules::Apply as _;
#[allow(unused_imports)]
use alondra_views as views;
#[allow(unused_imports)]
use crate::inbox_items;
use std::collections::{BTreeMap, BTreeSet};

#[test]
fn the_daemon_records_the_accounts_and_the_index_and_its_view_read_them() {
    use alondra_core::accounts::{AccountBilling, AccountId, AccountState, AccountStatus, Provider};
    let account = |id: &str, state| AccountStatus { antigravity: None,
        provider: Provider::Claude, id: AccountId::parse(id).unwrap(), name: id.to_uppercase(), state,
        native_identity: None, email: Some(format!("{id}@example.org")), plan: Some("max".into()), billing: AccountBilling::default(), sign_ins: 0, profile: None,
    };
    let mut s = DomainState::new("epoch");
    let accounts = vec![account("default", AccountState::SignedIn), account("work", AccountState::SignedOut)];
    assert_eq!(exec(&mut s, user(), Command::RecordAccounts { accounts: accounts.clone(), reserve: alondra_core::usage::default_reserve() }).unwrap_err().code, "forbidden");
    let twice = vec![account("work", AccountState::SignedIn), account("work", AccountState::SignedOut)];
    assert_eq!(exec(&mut s, Actor::System, Command::RecordAccounts { accounts: twice, reserve: alondra_core::usage::default_reserve() }).unwrap_err().code, "invalid_input");
    assert!(s.accounts.is_empty());
    exec(&mut s, Actor::System, Command::RecordAccounts { accounts: accounts.clone(), reserve: alondra_core::usage::default_reserve() }).unwrap();
    let index = ReadIndex::build(&s);
    assert_eq!(index.accounts(), accounts.as_slice());
    let restored: DomainState = serde_json::from_value(serde_json::to_value(&s).unwrap()).unwrap();
    assert_eq!(restored.accounts, accounts, "the accounts survive a restart");
    let live = alondra_views::LiveSnapshot::default();
    let context = alondra_views::RenderContext {
        domain: &s, index: &index, live: &live, client: alondra_views::Client::Tui, lang: alondra_core::text::Lang::Es,
        transport: alondra_views::Transport::Loopback, cursor: "", rendered_at: "",
    };
    let view = alondra_views::build("accounts", context).unwrap();
    let rows: Vec<(String, Option<String>)> = view.sections.iter().flat_map(|section| &section.items).filter_map(|item| match item {
        alondra_views::Item::Row(row) if row.account.is_some() => Some((row.title.clone(), row.subtitle.clone())), _ => None,
    }).collect();
    assert_eq!(rows, [
        ("DEFAULT".to_owned(), Some("Claude · default@example.org · max · peso 5, por plan".to_owned())),
        ("WORK".to_owned(), Some("Claude · work@example.org · max · peso 5, por plan".to_owned())),
    ]);
    assert!(!serde_json::to_string(&view).unwrap().contains("/accounts/"), "a view never shows a folder");
    exec(&mut s, Actor::System, Command::RecordAccounts { accounts: vec![], reserve: alondra_core::usage::default_reserve() }).unwrap();
    assert!(ReadIndex::build(&s).accounts().is_empty());
}

#[test]
fn clearing_takes_a_task_whose_request_closed_while_its_node_was_still_ending() {
    use alondra_views::{build, Client, Item, Lang, LiveSnapshot, RenderContext, Transport};
    let mut s = fixture();
    present(&mut s, "d", text());
    exec(&mut s, user(), Command::AcceptDelivery { presentation_id: "d".into(), targets: text() }).unwrap();
    end_node(&mut s, "t");
    exec(&mut s, orchestrator(), Command::CreateTask { priority: Default::default(), plan_items: None, continues: None, task_id: "c".into(), request_id: "r".into(),
        title: "c".into(), agent_id: None, review_prs: false, dependencies: BTreeSet::new(), stack: None,
        launch: Some(alondra_domain_model::LaunchPlan::new("node", "/tmp")) }).unwrap();
    exec(&mut s, user(), Command::CancelTask { task_id: "c".into(), reason: None }).unwrap();
    assert!(s.tasks[&"c".into()].ending() && !s.tasks[&"c".into()].state.terminal());
    exec(&mut s, orchestrator(), Command::CloseRequest { covered_by: vec![], outcome: Some(alondra_domain_model::CloseOutcome::TasksEnded), request_id: "r".into(), answer: None, withdrawn: false, title: None, headline: None, folded_into: None }).unwrap();
    let seen = s.revision;
    exec(&mut s, user(), Command::ClearCompleted { seen }).unwrap();
    let index = ReadIndex::build(&s);
    let live = LiveSnapshot::default();
    let view = build("tasks?section=finished&after=0", RenderContext {
        domain: &s, index: &index, live: &live, client: Client::Tui, lang: Lang::En,
        transport: Transport::Loopback, cursor: "c", rendered_at: "",
    }).unwrap();
    let listed = view.sections.iter().find(|section| section.id == "finished").unwrap();
    let rows: Vec<_> = listed.items.iter().filter(|item| matches!(item, Item::Row(_))).collect();
    assert!(rows.is_empty(), "cleared work is still listed: {rows:?}");
}

fn user() -> Actor {
    Actor::User {
        user_id: "u".into(),
    }
}

fn agent() -> Actor {
    Actor::Agent {
        agent_id: "a".into(),
    }
}

/// The task waits in Closing for its node; Alondra records the node closed and the task is done.
fn end_node(s: &mut DomainState, task: &str) {
    assert_eq!(s.tasks[&task.into()].state, TaskState::Closing);
    exec(s, Actor::System, Command::RecordNodeEnd { task_id: task.into(), end: NodeEnd::Closed, agent_id: None }).unwrap();
}

/// Every card in Waiting for You has something the user can do now and nothing holding it back,
/// whatever state a test reached (the property of the cards design, checked after every command).
fn assert_cards_usable(s: &DomainState) {
    for item in inbox_items(&s).iter().filter(|item| item.pending) {
        assert!(item.blocked_by.is_empty(), "{} is listed while held back", item.id);
        assert!(item.available_actions.iter().any(|action| action.enabled), "{} is listed with nothing the user can do", item.id);
    }
}

fn exec(s: &mut DomainState, a: Actor, c: Command) -> Result<CommandOutcome, DomainError> {
    let outcome = s.execute(
        &a,
        &CommandEnvelope {
            protocol_version: 2,
            data_epoch: s.data_epoch.clone(),
            command_id: format!("c-{}", s.revision).into(),
            expected_revision: s.revision,
            command: c,
        },
    );
    assert_cards_usable(s);
    outcome
}

fn fixture() -> DomainState {
    let mut s = DomainState::new("epoch");
    exec(
        &mut s,
        Actor::System,
        Command::RegisterAgent {
            agent_id: "a".into(),
            name: "Instance A".into(), at: 0
        },
    )
    .unwrap();
    exec(
        &mut s,
        user(),
        Command::CreateRequest {
            request_id: "r".into(),
            body: "Build requested result".into(), auto_merge: false, maintenance: false, message: false,
        },
    )
    .unwrap();
    exec(
        &mut s,
        orchestrator(),
        Command::CreateTask { priority: Default::default(), plan_items: None, continues: None,
            task_id: "t".into(),
            request_id: "r".into(),
            title: "Result".into(),
            agent_id: Some("a".into()),
            review_prs: true,
            dependencies: BTreeSet::new(),
            stack: None,
            launch: Some(alondra_domain_model::LaunchPlan::new("node", "/tmp")),
        },
    )
    .unwrap();
    s.requests.get_mut(&"r".into()).unwrap().title = Some("Requested result".into());
    s
}

fn text() -> Vec<DeliveryTarget> {
    vec![DeliveryTarget::Text {
        text: "The result".into(),
    }]
}

fn present(s: &mut DomainState, id: &str, targets: Vec<DeliveryTarget>) {
    exec(
        s,
        agent(),
        Command::PresentResult {
            checklist: Vec::new(),
            attachments: vec![],
            task_id: "t".into(),
            interaction_id: id.into(),
            body: "Result ready".into(),
            targets,
            is_final: true, nothing_to_review: false, card: alondra_domain_model::CardSpec::delivery("Review the result", &[], true),
        },
    )
    .unwrap();
}

fn orchestrator() -> Actor {
    Actor::Orchestrator { generation: 0 }
}

fn node(s: &mut DomainState, event: NodeEvent) {
    exec(s, Actor::System, Command::MoveNode { agent_id: "a".into(), event}).unwrap();
}

fn pending(s: &DomainState, id: &str) -> bool {
    inbox_items(&s).iter().any(|item| item.id == format!("interaction:{id}") && item.pending)
}
