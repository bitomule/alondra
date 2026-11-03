//! Moving a task to another account: the task stays itself, only the rule's accounts are offered,
//! and the end of the execution it leaves hands over to the next node without asking anyone.
use super::lifecycle_tests::{exec, last, must, task, user, world};
use super::*;
use crate::accounts::{AccountBilling, AccountId, AccountKey, AccountState, AccountStatus, Provider};
use crate::model_selection::*;
use std::collections::{BTreeMap, BTreeSet};

fn account(provider: Provider, id: &str, state: AccountState) -> AccountStatus {
    AccountStatus { provider, id: AccountId::parse(id).unwrap(), name: format!("{id} name"), state, native_identity: None, email: None, plan: None, billing: AccountBilling::default(), sign_ins: 0 }
}
fn catalog(provider: Provider, id: &str, model: &str) -> AccountModelCatalog {
    AccountModelCatalog { account: AccountKey::new(provider, AccountId::parse(id).unwrap()), revision: format!("{id}-{model}"), measured_at: 1, native_default: Some(model.into()), models: vec![CatalogModel {
        model: model.into(), display_name: model.into(), reasoning_efforts: vec!["high".into()], default_reasoning_effort: Some("high".into()), context_window: Some(100_000), input_modalities: vec!["text".into()], is_default: true, hidden: false,
    }] }
}
fn claude() -> AccountKey { AccountKey::native(Provider::Claude) }
fn codex() -> AccountKey { AccountKey::native(Provider::Codex) }

/// Task `t`, its turn running on Codex, with Claude signed in and a Claude account turned off.
fn on_codex() -> DomainState {
    let mut s = world();
    must(&mut s, Actor::System, Command::RecordAccounts { accounts: vec![
        account(Provider::Claude, "default", AccountState::SignedIn), account(Provider::Codex, "default", AccountState::SignedIn), account(Provider::Claude, "off", AccountState::Disabled),
    ], reserve: crate::usage::default_reserve() });
    for (provider, id, model) in [(Provider::Claude, "default", "opus"), (Provider::Codex, "default", "gpt"), (Provider::Claude, "off", "opus")] {
        must(&mut s, Actor::System, Command::RecordModelCatalog { catalog: catalog(provider, id, model) });
    }
    task(&mut s, "t", None);
    task(&mut s, "later", None);
    must(&mut s, Actor::Orchestrator { generation: 0 }, Command::SetDependencies { task_id: "later".into(), dependencies: BTreeSet::from(["t".into()]), stack: None });
    must(&mut s, user(), Command::SetPriority { task_id: "t".into(), priority: TaskPriority::High });
    must(&mut s, Actor::System, Command::AdmitExecution { provider: Provider::Codex, task_id: "t".into(), agent_id: "a".into(), conversation: "conversation".into(),
        instance: "instance".into(), host: "sdk-host".into(), model: None, account: None, leaves_limit: None });
    let mut selection = TaskSelection::automatic(ModelProfile::Implementation);
    selection.native_tool_denials = Some(vec![]);
    selection.effective = Some(ModelSelection { model: ModelKey { provider: Provider::Codex, model: "gpt".into() }, reasoning_effort: Some("high".into()), catalog_revision: "default-gpt".into() });
    must(&mut s, Actor::System, Command::RecordTaskSelection { task_id: "t".into(), selection, pending: None });
    let LifecycleRecord::Planned { input } = s.lifecycle_journal[&TaskId::from("t")][0].clone() else { panic!("planned first") };
    must(&mut s, Actor::System, Command::RecordRuntimeFacts { task_id: "t".into(), generation: 1, facts: vec![
        RuntimeObservation::ResourceAvailable, RuntimeObservation::TurnStarted { turn: "turn".into(), input },
    ] });
    s
}
fn move_to(s: &mut DomainState, account: AccountKey) -> Result<CommandOutcome, DomainError> {
    let stamp = s.task_selection_stamp(&"t".into()).unwrap();
    exec(s, user(), Command::MoveTask { task_id: "t".into(), expected_selection: stamp, account })
}
/// What makes the task itself: what a move must leave as it was.
fn identity(s: &DomainState) -> (String, RequestId, TaskPriority, BTreeSet<TaskId>, BTreeSet<TaskId>, Option<Checklist>) {
    let t = &s.tasks[&TaskId::from("t")];
    (t.title.clone(), t.request_id.clone(), t.priority, t.dependencies.clone(), s.tasks[&TaskId::from("later")].dependencies.clone(), s.checklists.get(&TaskId::from("t")).cloned())
}

#[test]
fn a_move_keeps_the_task_itself_and_is_offered_only_where_its_rule_accepts_it() {
    let mut s = on_codex();
    assert!(s.move_task_action(&"t".into(), claude()).enabled);
    assert_eq!(s.move_task_action(&"t".into(), codex()).disabled_reason.as_deref(), Some("same_account"));
    assert_eq!(s.move_task_action(&"t".into(), AccountKey::new(Provider::Claude, AccountId::parse("off").unwrap())).disabled_reason.as_deref(), Some("account_unavailable"));
    let before = identity(&s);
    let tasks = s.tasks.len();
    move_to(&mut s, claude()).unwrap();
    assert_eq!(identity(&s), before, "id, title, request, priority, dependencies and checklist stay the task's own");
    assert_eq!(s.tasks.len(), tasks, "a move creates no task and cancels none");
    let t = &s.tasks[&TaskId::from("t")];
    assert_eq!(t.state, TaskState::Running);
    assert_eq!(t.agent_id, Some("a".into()), "the node it leaves runs until its execution ends");
    assert_eq!(t.account_key(), codex());
    assert_eq!(t.selection.as_ref().unwrap().account, Some(claude()));
    assert_eq!(t.selection.as_ref().unwrap().effective, None, "a Codex model does not bind the Claude node");
    let pending = t.pending_selection.clone().unwrap();
    assert_eq!((pending.account, pending.model.model), (claude(), ModelKey { provider: Provider::Claude, model: "opus".into() }));
    assert_eq!(s.moving_to(&"t".into()), Some(&claude()));
    assert_eq!(move_to(&mut s, AccountKey::new(Provider::Claude, AccountId::parse("off").unwrap())).unwrap_err().code, "selection_pending");
}

#[test]
fn the_host_a_task_moves_from_ends_into_the_move_without_asking_anyone() {
    let mut s = on_codex();
    move_to(&mut s, claude()).unwrap();
    let cards = s.interactions.len();
    must(&mut s, Actor::System, Command::RecordHostEnded { task_id: "t".into(), generation: 1, resumes: false, by_alondra: false, ended_work: vec![] });
    let LifecycleRecord::HostEnded { resume: Some(_), text: Some(text), .. } = last(&s, "t") else { panic!("the move goes on by itself: {:?}", last(&s, "t")) };
    assert!(text.starts_with("The user moved this task to claude/default"), "{text}");
    assert_eq!(s.interactions.len(), cards, "no card asks how the task goes on");
    let lifecycle = s.lifecycle_of(&"t".into()).unwrap();
    assert_eq!(lifecycle.pending_inputs().len(), 1, "the next node starts with the move");
    let admit = |account: Option<&str>| Command::AdmitExecution { provider: Provider::Claude, task_id: "t".into(), agent_id: "b".into(), conversation: "next".into(),
        instance: "next".into(), host: "sdk-host".into(), model: None, account: account.map(|account| AccountId::parse(account).unwrap()), leaves_limit: None };
    assert_eq!(exec(&mut s.clone(), Actor::System, admit(Some("off"))).unwrap_err().code, "active_account_immutable", "only the account the move names takes the task");
    must(&mut s, Actor::System, admit(None));
    assert_eq!(s.tasks[&TaskId::from("t")].account_key(), claude());
    assert_eq!(s.moving_to(&"t".into()), None);
}

#[test]
fn a_question_open_when_the_task_moves_stays_open_and_its_answer_reaches_the_next_node() {
    let mut s = on_codex();
    let options = vec![QuestionOption { id: "o0".into(), label: "Keep the parser".into(), description: None }, QuestionOption { id: "o1".into(), label: "Rewrite it".into(), description: None }];
    must(&mut s, Actor::System, Command::RecordDecisionRequest { task_id: "t".into(), generation: 1, epoch: 1, hold: Hold::Question, tool: Some("AskUserQuestion".into()),
        input: serde_json::json!({}), mcp_server: None, prompt: DecisionPrompt::Card { body: "Which parser?".into(), options, card: CardSpec::new(CardKind::Decide, "Which parser?") } });
    let card = decision_card_id(&"t".into(), 1, 1);
    move_to(&mut s, claude()).unwrap();
    must(&mut s, Actor::System, Command::RecordHostEnded { task_id: "t".into(), generation: 1, resumes: false, by_alondra: false, ended_work: vec![] });
    assert!(s.open(&s.interactions[&card]), "the question is still the user's to answer");
    assert!(s.lifecycle_journal[&TaskId::from("t")].iter().any(|record| matches!(record, LifecycleRecord::QuestionKept { card: kept } if *kept == card)));
    let lifecycle = s.lifecycle_of(&"t".into()).unwrap();
    assert!(lifecycle.pending_inputs().is_empty(), "the next node waits for the answer it needs");
    assert!(lifecycle.cards().iter().any(|(held, _)| **held == card));
    assert!(!s.interactions.keys().any(|id| id.0.starts_with("session-ended-")), "no card asks how the task goes on");
    must(&mut s, user(), Command::AnswerQuestion { attachments: vec![], interaction_id: card.clone(), option_id: "o1".into(), inputs: BTreeMap::new() });
    let LifecycleRecord::Answered { card: answered, text: Some(text), .. } = last(&s, "t") else { panic!("the answer is told: {:?}", last(&s, "t")) };
    assert_eq!(answered, card);
    assert!(text.contains("«Which parser?»: Rewrite it"), "{text}");
    assert_eq!(s.lifecycle_of(&"t".into()).unwrap().pending_inputs().len(), 1);
}

#[test]
fn a_task_that_never_ran_just_starts_on_the_account_it_moved_to() {
    let mut s = on_codex();
    task(&mut s, "queued", None);
    let mut picked = TaskSelection::automatic(ModelProfile::Implementation);
    picked.request_model(Some(ModelKey { provider: Provider::Codex, model: "gpt".into() }));
    s.tasks.get_mut(&TaskId::from("queued")).unwrap().selection = Some(picked);
    let stamp = s.task_selection_stamp(&"queued".into()).unwrap();
    must(&mut s, user(), Command::MoveTask { task_id: "queued".into(), expected_selection: stamp, account: claude() });
    let queued = &s.tasks[&TaskId::from("queued")];
    assert_eq!(queued.pending_selection, None, "nothing runs that has to end first");
    let selection = queued.selection.clone().unwrap();
    assert_eq!((selection.account, selection.requested, selection.allow_model_substitution), (Some(claude()), None, true));
}

#[test]
fn a_move_within_a_provider_keeps_the_model_the_task_ran_on_when_the_account_has_it() {
    let mut s = on_codex();
    let mut accounts = s.accounts.clone();
    accounts.push(account(Provider::Codex, "work", AccountState::SignedIn));
    must(&mut s, Actor::System, Command::RecordAccounts { accounts, reserve: crate::usage::default_reserve() });
    let mut work = catalog(Provider::Codex, "work", "gpt");
    work.native_default = None;
    work.models[0].is_default = false;
    must(&mut s, Actor::System, Command::RecordModelCatalog { catalog: work });
    let destination = AccountKey::new(Provider::Codex, AccountId::parse("work").unwrap());
    move_to(&mut s, destination.clone()).unwrap();
    let t = &s.tasks[&TaskId::from("t")];
    assert_eq!(t.pending_selection.as_ref().map(|pending| (&pending.account, &pending.model.model.model)), Some((&destination, &"gpt".to_owned())));
    assert_eq!(t.selection.as_ref().unwrap().requested, None, "keeping the model is not a choice the user made");
}
