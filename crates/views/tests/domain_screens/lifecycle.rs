//! The aggregate rules: for a task with a journal, nothing but a lifecycle decision moves its
//! state, node, start or ending; a cancellation or a completion ends only with its receipts; and
//! a DTO edited by hand is refused, never repaired.
use alondra_domain_rules::*;
use alondra_views as views;
use crate::inbox_items;
use std::collections::{BTreeMap, BTreeSet};

/// The screen of task `id` in `lang`.
fn turn_end_shown(s: &DomainState, id: &str, lang: alondra_core::text::Lang) -> String {
    use alondra_views::{Client, RenderContext, Transport, build, live::LiveSnapshot};
    let index = ReadIndex::build(s);
    let live = LiveSnapshot::default();
    let view = build(&format!("task:{id}"), RenderContext { domain: s, index: &index, live: &live, client: Client::Tui, lang,
        transport: Transport::Loopback, cursor: "test", rendered_at: "" }).unwrap();
    serde_json::to_string(&view).unwrap()
}

/// The card view of `card` in `lang`.
fn card_shown(s: &DomainState, card: &InteractionId, lang: alondra_core::text::Lang) -> String {
    use alondra_views::{Client, RenderContext, Transport, build, live::LiveSnapshot};
    let index = ReadIndex::build(s);
    let live = LiveSnapshot::default();
    let view = build(&format!("card:{card}"), RenderContext { domain: s, index: &index, live: &live, client: Client::Tui, lang,
        transport: Transport::Loopback, cursor: "test", rendered_at: "" }).unwrap();
    serde_json::to_string(&view).unwrap()
}

#[test]
fn non_actionable_interrupted_turn_is_paused_and_a_message_resumes_it() {
    use alondra_core::text::Lang;
    let mut s = world();
    let card = sdk_turn(&mut s, "t", "a");
    must(&mut s, user(), Command::InterruptTurn { agent_id: "a".into() });
    let boundary = || Command::RecordTurnBoundary { task_id: "t".into(), generation: 1, turn: "turn-1".into(), failure: None, failed: None };
    let ended = must(&mut s, Actor::System, boundary());
    assert!(!s.interactions.contains_key(&InteractionId::from(card.as_str())));
    assert_eq!(notices(&ended), 0);
    assert_eq!(s.live_state(&"t".into()), Some(alondra_domain::LiveState::Paused));
    assert!(!holds_slot(&s, "t"));
    assert!(turn_end_shown(&s, "t", Lang::En).contains("Paused"));
    assert!(turn_end_shown(&s, "t", Lang::Es).contains("Pausada"));
    let before = s.lifecycle_journal.clone();
    assert_eq!(refused_unchanged(&mut s, Actor::System, boundary()), "conflict");
    assert_eq!(s.lifecycle_journal, before, "a repeated observation changes nothing");
    assert_aggregate_rules(&s);
    must(&mut s, Actor::System, Command::TellNode { task_id: "t".into(), text: "Continue with the tests".into(), from_user: true, answers: None });
    assert!(matches!(s.live_state(&"t".into()), Some(alondra_domain::LiveState::Working(_))));
    assert_aggregate_rules(&s);
}

#[test]
fn a_turn_that_ends_on_its_own_twice_says_nothing_to_the_user() {
    use alondra_core::text::Lang;
    let mut s = world();
    sdk_woken_turn(&mut s, "t", "a");
    must(&mut s, Actor::System, Command::RecordTurnBoundary { task_id: "t".into(), generation: 1, turn: "turn-1".into(), failure: None, failed: None });
    for lang in [Lang::En, Lang::Es] {
        let shown = turn_end_shown(&s, "t", lang);
        for said in ["without delivering or asking", "sin entregar ni preguntar", "what next?", "¿cómo sigue?"] {
            assert!(!shown.contains(said), "{lang:?}: {shown}");
        }
    }
}

#[test]
fn a_card_naming_a_task_with_a_long_title_shortens_the_title_and_says_all_it_asks_in_each_language() {
    use alondra_core::text::Lang;
    const TITLE: &str = "Parcel: quitar la barrera del login y los permisos de GitHub en el onboarding";
    let mut s = world();
    let card = failed_turn(&mut s, "t", "a");
    s.tasks.get_mut(&TaskId::from("t")).unwrap().title = TITLE.into();
    fail_turn(&mut s, "t", "turn-1", "billing_error", Some(400));
    let stored = s.interactions[&InteractionId::from(card.as_str())].card.effective_title();
    assert!(stored.contains("Parcel:") && stored.contains("…"), "{stored}");
    assert!(stored.ends_with("the Claude account has a billing problem"), "{stored}");
    assert!(stored.chars().count() <= CardSpec::TITLED_NEED_MAX, "{stored}");
    assert!(turn_end_shown(&s, "t", Lang::En).contains(stored));
    let es = turn_end_shown(&s, "t", Lang::Es);
    assert!(es.contains("problema de facturación") && es.contains("Parcel:"), "{es}");
}

#[test]
fn an_expired_sign_in_leaves_its_account_signed_out_until_claude_answers_on_it() {
    use alondra_core::accounts::AccountState::{SignedIn, SignedOut};
    let mut s = world();
    accounts_seen(&mut s, &[("default", SignedIn), ("work", SignedIn)]);
    sdk_turn_on(&mut s, "t", "a", "turn-1", Some("work"));
    sdk_turn_on(&mut s, "u", "b", "turn-1", None);
    let expired = fail_turn(&mut s, "t", "turn-1", "authentication_failed", Some(401));
    assert_eq!(notices(&expired), 1, "the user hears of it");
    assert_eq!(account_state(&s, "work"), (SignedOut, 0), "no new work starts on it");
    assert_eq!(account_state(&s, "default"), (SignedIn, 0), "the other account is not touched");
    let en = turn_end_shown(&s, "t", alondra_core::text::Lang::En);
    assert!(en.contains("Sign in to the Claude account WORK on the Mac"), "{en}");
    assert!(en.contains("Open Terminal on the Mac, run CLAUDE_CONFIG_DIR=~/.alondra/accounts/work claude and sign in with /login, then press I signed in."), "the command to run: {en}");
    let es = turn_end_shown(&s, "t", alondra_core::text::Lang::Es);
    assert!(es.contains("ejecuta CLAUDE_CONFIG_DIR=~/.alondra/accounts/work claude e inicia sesión con /login"), "{es}");
    let other = fail_turn(&mut s, "u", "turn-1", "authentication_failed", Some(401));
    assert_eq!(notices(&other), 1, "another account's sign-in is news of its own");
    assert_eq!(listed_holds(&s).len(), 2, "one card per account");
    let index = ReadIndex::build(&s);
    assert_eq!(index.accounts().iter().find(|a| a.id.as_str() == "work").map(|a| a.state), Some(SignedOut), "every screen reads it");
    accounts_seen(&mut s, &[("default", SignedIn), ("work", SignedIn)]);
    assert_eq!(account_state(&s, "work"), (SignedOut, 0), "its sign-in check does not undo what Claude said while a task waits on it");
    let card = s.claude_held().into_iter().find(|held| held.task.0 == "t").unwrap().card;
    must(&mut s, user(), Command::AnswerQuestion { attachments: vec![], interaction_id: card, option_id: SIGNED_IN.into(), inputs: BTreeMap::new() });
    turn_on_resumed(&mut s, "t", "turn-2");
    must(&mut s, Actor::System, Command::RecordRuntimeFacts { task_id: "t".into(), generation: 1, facts: vec![RuntimeObservation::ModelAnswered] });
    assert_eq!(account_state(&s, "work"), (SignedIn, 1), "signed in again: its credential's next generation");
    assert_eq!(held(&s), [("u".into(), ClaudeHold::SignedOut)], "the default account's sign-in still waits");
    accounts_seen(&mut s, &[("default", SignedIn), ("work", SignedIn)]);
    assert_eq!(account_state(&s, "work"), (SignedIn, 1), "the generation is kept");
    assert_aggregate_rules(&s);
}

#[test]
fn the_screens_show_one_sign_in_card_and_name_the_wait_of_the_other_task() {
    use alondra_views::{Client, RenderContext, Transport, build, live::LiveSnapshot, text::Lang};
    let mut s = world();
    sdk_turn(&mut s, "t", "a");
    sdk_turn(&mut s, "u", "b");
    fail_turn(&mut s, "t", "turn-1", "authentication_failed", Some(401));
    fail_turn(&mut s, "u", "turn-1", "authentication_failed", Some(401));
    let index = ReadIndex::build(&s);
    let live = LiveSnapshot::default();
    let render = |key: &str| {
        let view = build(key, RenderContext { domain: &s, index: &index, live: &live, client: Client::Tui, lang: Lang::Es,
            transport: Transport::Loopback, cursor: "test", rendered_at: "" }).unwrap();
        serde_json::to_string(&view).unwrap()
    };
    let home = render("home");
    assert!(home.contains("\"id\":\"waiting\",\"title\":\"Te espera\",\"count\":1,"), "one card in Waiting for You: {home}");
    assert!(home.contains("La sesión de Claude Code caducó y esperan 2 tareas."), "{home}");
    let other = render("task:u");
    assert!(other.contains("Te necesita") && other.contains("Espera a que inicies sesión en Claude"), "{other}");
}

#[test]
fn a_turn_that_stays_open_after_claude_accepted_its_interrupt_can_be_forced_to_stop_once() {
    use alondra_core::text::Lang;
    let mut s = world();
    sdk_turn(&mut s, "t", "a");
    let force = || Command::ForceStopTurn { agent_id: "a".into() };
    let accepted = || Command::RecordInterruptAccepted { task_id: "t".into(), generation: 1, turn: "turn-1".into() };
    assert_eq!(refused_unchanged(&mut s, user(), force()), "conflict", "an interrupt comes first");
    assert_eq!(exec(&mut s.clone(), user(), force()).unwrap_err().refusal, Some(Refusal::InterruptNotAccepted));
    must(&mut s, user(), Command::InterruptTurn { agent_id: "a".into() });
    assert_eq!(refused_unchanged(&mut s, user(), force()), "conflict", "until Claude accepts the interrupt");
    assert_eq!(refused_unchanged(&mut s, user(), accepted()), "forbidden", "only Alondra records what Claude did");
    must(&mut s, Actor::System, accepted());
    assert_eq!(last(&s, "t"), LifecycleRecord::InterruptAccepted { turn: "turn-1".into() });
    assert_eq!(refused_unchanged(&mut s, node("a"), force()), "forbidden");
    let revision = replay(&"t".into(), &s.lifecycle_journal[&TaskId::from("t")]).unwrap().revision();
    must(&mut s, user(), force());
    assert_eq!(last(&s, "t"), LifecycleRecord::ControlRequested {
        control: format!("control-t-{}", revision + 1),
        operation: ControlRecord::ForceStop { turn: "turn-1".into() },
    });
    assert_eq!(exec(&mut s.clone(), user(), force()).unwrap_err().refusal, Some(Refusal::AlreadyStopping), "once");
    assert_aggregate_rules(&s);

    must(&mut s, Actor::System, Command::RecordHostEnded { task_id: "t".into(), generation: 1, resumes: false, ended_work: vec![], by_alondra: false });
    assert!(!s.interactions.keys().any(|id| id.0.starts_with(SESSION_ENDED_PREFIX) || id.0.starts_with(TURN_ENDED_PREFIX)));
    assert_eq!(s.live_state(&"t".into()), Some(alondra_domain::LiveState::Paused));
    assert!(turn_end_shown(&s, "t", Lang::En).contains("Paused"));
    assert!(turn_end_shown(&s, "t", Lang::Es).contains("Pausada"));
    must(&mut s, Actor::System, Command::TellNode { task_id: "t".into(), text: "Continue".into(), from_user: true, answers: None });
    assert!(matches!(last(&s, "t"), LifecycleRecord::Told { .. }));
    assert_aggregate_rules(&s);
}

#[test]
fn a_turn_claude_ended_as_failed_asks_how_to_go_on_and_names_its_error() {
    use alondra_core::text::Lang;
    let mut s = world();
    let card = sdk_turn(&mut s, "t", "a");
    let ended = must(&mut s, Actor::System, Command::RecordTurnBoundary { task_id: "t".into(), generation: 1, turn: "turn-1".into(), failure: None, failed: Some("error_max_turns".into()) });
    let failed = &s.interactions[&InteractionId::from(card.as_str())];
    assert_eq!(failed.card.effective_title(), "Claude's turn ended with an error (`error_max_turns`): tell it how to go on");
    assert_eq!(options(&s, &card.as_str().into()), [CARRY_ON, CANCEL_TASK]);
    assert!(ended.effects.iter().any(|e| matches!(e, DomainEffect::NotifyUser { interaction_id, .. } if interaction_id.0 == card)));
    assert!(held(&s).is_empty(), "nothing waits for the API");
    assert_eq!(s.tasks[&TaskId::from("t")].state, TaskState::NeedsYou);
    let en = turn_end_shown(&s, "t", Lang::En);
    assert!(en.contains("Claude's turn ended with an error (`error_max_turns`): tell it how to go on"), "{en}");
    let es = turn_end_shown(&s, "t", Lang::Es);
    assert!(es.contains("El turno de Claude terminó con un error (`error_max_turns`): dile cómo seguir"), "{es}");
    assert!(es.contains("Dile cómo seguir, escríbele qué hacer o cancela la tarea."), "{es}");
    assert_aggregate_rules(&s);

    let mut s = world();
    let card = sdk_turn(&mut s, "t", "a");
    must(&mut s, user(), Command::InterruptTurn { agent_id: "a".into() });
    must(&mut s, Actor::System, Command::RecordTurnBoundary { task_id: "t".into(), generation: 1, turn: "turn-1".into(), failure: None, failed: Some("error_during_execution".into()) });
    assert!(!s.interactions.contains_key(&InteractionId::from(card.as_str())), "the interrupt is recorded as status");

    let mut s = world();
    let card = sdk_turn(&mut s, "t", "a");
    must(&mut s, Actor::System, Command::RecordTurnBoundary { task_id: "t".into(), generation: 1, turn: "turn-1".into(),
        failure: Some(SdkApiFailure { error: "billing_error".into(), status: Some(400), resets_at: None, ..Default::default() }), failed: Some("success_is_error".into()) });
    assert_eq!(s.interactions[&InteractionId::from(card.as_str())].card.why, REFUSED_WHY, "the API's failure comes first");
}

#[test]
fn a_message_claude_never_reads_leaves_the_queue_and_asks_whether_to_send_it_again() {
    use alondra_core::text::Lang;
    let mut s = world();
    let (card, text) = dropped(&mut s);
    let replayed = replay(&"t".into(), &s.lifecycle_journal[&TaskId::from("t")]).unwrap();
    assert!(replayed.pending_inputs().is_empty(), "nothing waits for Claude to read it");
    let asked = &s.interactions[&card];
    assert_eq!(asked.card.effective_title(), "Claude didn't get to read your message");
    assert_eq!(asked.card.content.as_ref().map(|content| content.body.as_str()), Some(text.as_str()), "the card carries the whole message");
    assert_eq!(options(&s, &card), [SEND_AGAIN, DISCARD]);
    assert_eq!(s.tasks[&TaskId::from("t")].state, TaskState::NeedsYou);
    let es = turn_end_shown(&s, "t", Lang::Es);
    assert!(es.contains("Claude no llegó a leer tu mensaje"), "{es}");
    let shown = {
        use alondra_views::{Client, RenderContext, Transport, build, live::LiveSnapshot};
        let index = ReadIndex::build(&s);
        let live = LiveSnapshot::default();
        let view = build(&format!("card:{}", card.0), RenderContext { domain: &s, index: &index, live: &live, client: Client::Tui, lang: Lang::Es,
            transport: Transport::Loopback, cursor: "test", rendered_at: "" }).unwrap();
        serde_json::to_string(&view).unwrap()
    };
    assert!(shown.contains("Enviar otra vez") && shown.contains("Descartar") && shown.contains("Add the tests first"), "{shown}");
    assert_eq!(exec(&mut s, Actor::System, Command::RecordDroppedInput { task_id: "t".into(), generation: 1, input: "other".into(), text: "other".into() })
        .unwrap_err().code, "conflict", "an input that is not pending is never refused");
    assert_aggregate_rules(&s);

    answer(&mut s, &card, SEND_AGAIN);
    let Some(LifecycleRecord::Answered { input: again, text: Some(sent), .. }) = s.lifecycle_journal[&TaskId::from("t")].last().cloned() else { panic!("sent again") };
    assert_eq!(sent, text, "the same words go again");
    let replayed = replay(&"t".into(), &s.lifecycle_journal[&TaskId::from("t")]).unwrap();
    assert_eq!(replayed.pending_inputs(), [&alondra_domain::InputId::new(again).unwrap()]);
    assert!(!s.open(&s.interactions[&card]), "the card is answered");
    assert_aggregate_rules(&s);
}

/// The sandbox's «Publish the release once it is merged», 24 October: its PR merged while its node
/// ran the release, and the task read «Delivered», as if it waited for a review. It works, after
/// the merge, on the phone, the Mac and the terminal.
#[test]
fn a_task_of_the_sdk_that_works_after_its_merge_reads_so_on_every_client() {
    use alondra_views::{Client, RenderContext, Transport, build, live::LiveSnapshot, text::Lang};
    let mut s = world();
    working_after_the_merge(&mut s);
    assert_eq!(s.live_state(&"t".into()), Some(alondra_domain::LiveState::Working(alondra_domain::Worker::AfterMerge)));
    let index = ReadIndex::build(&s);
    let live = LiveSnapshot::default();
    for client in [Client::Ios, Client::Mac, Client::Tui] {
        for (lang, said) in [(Lang::En, "Working · "), (Lang::Es, "Trabajando · ")] {
            let view = build("task:t", RenderContext { domain: &s, index: &index, live: &live, client, lang,
                transport: Transport::Loopback, cursor: "test", rendered_at: "" }).unwrap();
            let shown = serde_json::to_string(&view).unwrap();
            assert!(shown.contains(said) && !shown.contains("\"Delivered\"") && !shown.contains("\"Entregada\""), "{client:?}: {shown}");
        }
    }
    assert_aggregate_rules(&s);
}

#[test]
fn non_actionable_force_stop_after_merge_pauses_without_discarding_the_work_owed() {
    use alondra_core::text::Lang;
    let mut s = world();
    working_after_the_merge(&mut s);
    let ended = force_stopped(&mut s);
    assert_eq!(notices(&ended), 0);
    assert!(!s.interactions.keys().any(|id| id.0.starts_with(SESSION_ENDED_PREFIX)));
    assert!(turn_end_shown(&s, "t", Lang::Es).contains("Pausada"));
    assert!(s.post_merge_owed(&s.tasks[&TaskId::from("t")]));
    assert_aggregate_rules(&s);
    must(&mut s, Actor::System, Command::TellNode { task_id: "t".into(), text: "Continue the release".into(), from_user: true, answers: None });
    assert!(s.post_merge_owed(&s.tasks[&TaskId::from("t")]));
    assert_aggregate_rules(&s);
}

#[test]
fn native_request_rejection_keeps_its_selection_and_catalog_and_waits_for_explicit_user_action() {
    use alondra_core::accounts::{AccountKey, AccountState, Provider};
    use alondra_core::model_selection::{AccountModelCatalog, CatalogModel, ModelKey, ModelProfile, ModelSelection, TaskSelection};
    use alondra_core::text::Lang;
    for (code, why, english, spanish) in [
        ("codex_turn_failed", NATIVE_FAILED_WHY, "Codex's turn ended with an error", "El turno de Codex terminó con un error"),
        ("codex_request_rejected", NATIVE_REQUEST_REJECTED_WHY, "Codex rejected this task's request", "Codex rechazó la solicitud de esta tarea"),
        ("codex_context_exceeded", NATIVE_CONTEXT_EXCEEDED_WHY, "exceeds the Codex model's context window", "supera la ventana de contexto del modelo de Codex"),
    ] {
        let mut state = world(); mixed_default_accounts(&mut state, AccountState::SignedIn);
        let account = AccountKey::native(Provider::Codex);
        must(&mut state, Actor::System, Command::RecordModelCatalog { catalog: AccountModelCatalog { account: account.clone(), revision: "native-request-fixture".into(), measured_at: 1, native_default: Some("fixture-model".into()), models: vec![CatalogModel { model: "fixture-model".into(), display_name: "Fixture model".into(), reasoning_efforts: vec![], default_reasoning_effort: None, context_window: None, input_modalities: vec!["text".into()], is_default: true, hidden: false }] } });
        sdk_turn_on_provider(&mut state, Provider::Codex, "t", "a", "turn-1", None);
        let mut intent = TaskSelection::automatic(ModelProfile::General); intent.requested = Some(ModelKey { provider: Provider::Codex, model: "fixture-model".into() }); intent.account = Some(account.clone());
        intent.effective = Some(ModelSelection { model: intent.requested.clone().unwrap(), reasoning_effort: None, catalog_revision: "native-request-fixture".into() });
        must(&mut state, Actor::System, Command::RecordTaskSelection { task_id: "t".into(), selection: intent.clone(), pending: None });
        let accounts = state.accounts.clone(); let catalog = state.model_catalogs.clone(); let usage = state.account_usage.clone();
        let execution = state.lifecycle_of(&"t".into()).unwrap().execution().cloned();
        must(&mut state, Actor::System, Command::RecordTurnBoundary { task_id: "t".into(), generation: 1, turn: "turn-1".into(), failure: None, failed: Some(code.into()) });
        let card = turn_ended_card_id(&"t".into(), 1, "turn-1");
        assert_eq!(state.interactions[&card].card.why, why); assert_eq!(options(&state, &card), [CARRY_ON, CANCEL_TASK]);
        assert_eq!(state.tasks[&TaskId::from("t")].selection.as_ref(), Some(&intent)); assert_eq!(state.tasks[&TaskId::from("t")].account_key(), account);
        assert_eq!(state.accounts, accounts); assert_eq!(state.model_catalogs, catalog); assert_eq!(state.account_usage, usage); assert!(state.claude_held().is_empty());
        let lifecycle = state.lifecycle_of(&"t".into()).unwrap(); assert_eq!(lifecycle.execution().cloned(), execution); assert!(lifecycle.pending_inputs().is_empty(), "no automatic retry input or account/model fallback is created");
        let shown = turn_end_shown(&state, "t", Lang::En); assert!(shown.contains(english), "{shown}"); assert!(!shown.contains("Claude's turn ended"));
        let shown = turn_end_shown(&state, "t", Lang::Es); assert!(shown.contains(spanish), "{shown}");
        assert_aggregate_rules(&state);
    }
}

pub(super) fn user() -> Actor {
    Actor::User { user_id: "u".into() }
}

fn orchestrator() -> Actor {
    Actor::Orchestrator { generation: 0 }
}

pub(super) fn node(id: &str) -> Actor {
    Actor::Agent { agent_id: id.into() }
}

pub(super) fn exec(s: &mut DomainState, actor: Actor, command: Command) -> Result<CommandOutcome, DomainError> {
    s.execute(&actor, &CommandEnvelope {
        protocol_version: PROTOCOL_VERSION, data_epoch: s.data_epoch.clone(), command_id: format!("c-{}", s.revision).into(),
        expected_revision: s.revision, command,
    })
}

pub(super) fn must(s: &mut DomainState, actor: Actor, command: Command) -> CommandOutcome {
    let shown = format!("{command:?}");
    match exec(s, actor, command) {
        Ok(outcome) => outcome,
        Err(error) => panic!("{shown} was refused: {error}"),
    }
}

pub(super) fn task(s: &mut DomainState, id: &str, agent: Option<&str>) {
    must(s, orchestrator(), Command::CreateTask { priority: Default::default(),
        task_id: id.into(), request_id: "r".into(), title: format!("Task {id}"), agent_id: agent.map(Into::into), review_prs: false,
        dependencies: BTreeSet::new(), stack: None, launch: Some(LaunchPlan::new("node", "/tmp")), plan_items: None, continues: None,
    });
}

pub(super) fn world() -> DomainState {
    let mut s = DomainState::new("epoch");
    for agent in ["a", "b"] {
        must(&mut s, Actor::System, Command::RegisterAgent { agent_id: agent.into(), name: agent.into(), at: 0 });
    }
    must(&mut s, user(), Command::CreateRequest {
        request_id: "r".into(), body: "Work".into(), auto_merge: false, maintenance: false, message: false,
    });
    must(&mut s, orchestrator(), Command::TitleRequest { request_id: "r".into(), title: "Work".into() });
    s
}

fn lifecycle(s: &DomainState, task: &str) -> (TaskState, bool, Option<AgentId>, Option<TaskEnding>) {
    let t = &s.tasks[&TaskId::from(task)];
    (t.state, t.started, t.agent_id.clone(), t.ending)
}

/// The texts the store keeps for a journal's inputs.
fn writes_texts(id: &TaskId, records: &Journal) -> BTreeMap<alondra_domain::InputId, String> {
    journal_writes(id, records, 0).unwrap().into_iter()
        .filter_map(|write| Some((write.input?, write.text?)))
        .collect()
}

/// After every accepted command, the DTO of every task agrees with the replay of its journal,
/// running the derivations again moves nothing (only decisions do), and the events the store keeps
/// for each journal give back exactly its records.
pub(super) fn assert_aggregate_rules(s: &DomainState) {
    s.validate_journal().unwrap_or_else(|error| panic!("{error}"));
    let mut again = s.clone();
    again.derive_states();
    for (id, task) in &s.tasks {
        assert_eq!(&again.tasks[id], task, "derive_states moved the task {id} without a decision");
    }
    for card in s.interactions.values() {
        assert!(!(card.withdrawn && DomainState::answered_question(card)), "the card {} is answered and withdrawn, which the store refuses", card.id);
    }
    for (id, records) in &s.lifecycle_journal {
        let writes = journal_writes(id, records, 0).unwrap();
        let initial = writes[0].input.clone().expect("the journal is created with its initial input");
        let events: Vec<_> = writes.into_iter().filter_map(|write| write.event).collect();
        let texts = writes_texts(id, records);
        assert_eq!(&records_from_events(id, &initial, &events, &texts).unwrap(), records, "the store's events of {id} do not give back its records");
    }
    for (review, held) in &s.plan_reviews {
        for task in held.open() {
            let lifecycle = replay(task, &s.lifecycle_journal[task]).unwrap();
            assert!(lifecycle.plan_reviews().iter().any(|(id, _)| *id == review), "the lifecycle of {task} does not hold it for the review {review}");
        }
    }
}

/// An SDK task whose observed turn started: admitted, resource available, the planned input taken.
pub(super) fn sdk_turn(s: &mut DomainState, id: &str, node: &str) -> String {
    sdk_turn_named(s, id, node, "turn-1")
}

fn sdk_turn_named(s: &mut DomainState, id: &str, node: &str, turn: &str) -> String {
    sdk_turn_on(s, id, node, turn, None)
}

/// An SDK task whose observed turn started on `account`.
fn sdk_turn_on(s: &mut DomainState, id: &str, node: &str, turn: &str, account: Option<&str>) -> String {
    sdk_turn_on_provider(s,alondra_core::accounts::Provider::Claude,id,node,turn,account)
}

fn sdk_turn_on_provider(s: &mut DomainState, provider: alondra_core::accounts::Provider, id: &str, node: &str, turn: &str, account: Option<&str>) -> String {
    task(s, id, None);
    must(s, Actor::System, Command::AdmitExecution { provider,
        task_id: id.into(), agent_id: node.into(), conversation: format!("conversation-{id}"), instance: format!("instance-{id}"), host: "sdk-host".into(), model: None,
        account: account.map(|account| alondra_core::accounts::AccountId::parse(account).unwrap()), leaves_limit: None,
    });
    let LifecycleRecord::Planned { input } = s.lifecycle_journal[&TaskId::from(id)][0].clone() else { panic!("planned first") };
    must(s, Actor::System, Command::RecordRuntimeFacts { task_id: id.into(), generation: 1, facts: vec![
        RuntimeObservation::ResourceAvailable,
        RuntimeObservation::TurnStarted { turn: turn.into(), input },
    ] });
    format!("{TURN_ENDED_PREFIX}{id}-1-{turn}")
}

/// Ends `turn` of the SDK task `id` with nothing that explains its next step, which wakes its
/// node, and starts `next`, the turn that wake opens: ending that one too opens the case for the
/// orchestrator whose id this returns.
pub(super) fn woken(s: &mut DomainState, id: &str, turn: &str, next: &str) -> String {
    must(s, Actor::System, Command::RecordTurnBoundary { task_id: id.into(), generation: 1, turn: turn.into(), failure: None, failed: None });
    let LifecycleRecord::TurnEnded { wake: Some(input), text: Some(text), .. } = last(s, id) else { panic!("the node is woken: {:?}", last(s, id)) };
    assert_eq!(text, WAKE_INSTRUCTION);
    let input = rewoken(s, id, &format!("{turn}-woken"), input);
    must(s, Actor::System, Command::RecordRuntimeFacts { task_id: id.into(), generation: 1, facts: vec![
        RuntimeObservation::TurnStarted { turn: next.into(), input },
    ] });
    format!("{STALLED_PREFIX}{id}-1-{next}")
}

/// The node of the SDK task `id`, woken once with `input`, starts and ends empty the turns of
/// its other wakes, `{prefix}-2` and on: this returns the input of its last wake.
pub(super) fn rewoken(s: &mut DomainState, id: &str, prefix: &str, mut input: String) -> String {
    for n in 2..=alondra_domain::EMPTY_TURN_WAKES {
        let turn = format!("{prefix}-{n}");
        must(s, Actor::System, Command::RecordRuntimeFacts { task_id: id.into(), generation: 1, facts: vec![
            RuntimeObservation::TurnStarted { turn: turn.clone(), input: input.clone() },
        ] });
        must(s, Actor::System, Command::RecordTurnBoundary { task_id: id.into(), generation: 1, turn, failure: None, failed: None });
        let LifecycleRecord::TurnEnded { wake: Some(next), .. } = last(s, id) else { panic!("wake {n} in a row: {:?}", last(s, id)) };
        input = next;
    }
    input
}

/// An SDK task whose `turn` is the one Alondra woke its node for after an empty turn before it:
/// ending it with nothing that explains the next step opens the case for the orchestrator whose
/// id this returns.
pub(super) fn sdk_woken_turn_named(s: &mut DomainState, id: &str, node: &str, turn: &str) -> String {
    sdk_turn_named(s, id, node, "turn-0");
    woken(s, id, "turn-0", turn)
}

pub(super) fn sdk_woken_turn(s: &mut DomainState, id: &str, node: &str) -> String {
    sdk_woken_turn_named(s, id, node, "turn-1")
}

/// An SDK task whose next failed boundary asks how to continue.
pub(super) fn failed_turn(s: &mut DomainState, id: &str, node: &str) -> String {
    sdk_turn(s, id, node)
}

fn holds_slot(s: &DomainState, id: &str) -> bool {
    replay(&id.into(), &s.lifecycle_journal[&TaskId::from(id)]).unwrap().holds_slot()
}

fn fail_turn(s: &mut DomainState, id: &str, turn: &str, error: &str, status: Option<u16>) -> CommandOutcome {
    must(s, Actor::System, Command::RecordTurnBoundary {
        task_id: id.into(), generation: 1, turn: turn.into(),
        failure: Some(SdkApiFailure { error: error.into(), status, resets_at: None, ..Default::default() }), failed: None })
}

/// The cards of Claude's holds Waiting for You lists.
fn listed_holds(s: &DomainState) -> Vec<String> {
    inbox_items(s).into_iter().filter(|item| item.pending).map(|item| {
        let InboxSubject::Interaction { interaction_id, .. } = item.subject;
        interaction_id.0
    }).filter(|id| s.claude_hold(&InteractionId::from(id.as_str())).is_some()).collect()
}

fn notices(outcome: &CommandOutcome) -> usize {
    outcome.effects.iter().filter(|e| matches!(e, DomainEffect::NotifyUser { .. })).count()
}

fn held(s: &DomainState) -> Vec<(String, ClaudeHold)> {
    s.claude_held().into_iter().map(|held| (held.task.0, held.hold)).collect()
}

/// The task goes on with the input its resume gave it: a new turn starts on it.
fn turn_on_resumed(s: &mut DomainState, id: &str, turn: &str) {
    let Some(LifecycleRecord::Resumed { input, .. }) = s.lifecycle_journal[&TaskId::from(id)].last().cloned() else { panic!("{id} was not resumed") };
    must(s, Actor::System, Command::RecordRuntimeFacts { task_id: id.into(), generation: 1, facts: vec![
        RuntimeObservation::TurnStarted { turn: turn.into(), input },
    ] });
}

fn accounts_seen(s: &mut DomainState, states: &[(&str, alondra_core::accounts::AccountState)]) {
    let accounts = states.iter().map(|(id, state)| alondra_core::accounts::AccountStatus {
        provider: alondra_core::accounts::Provider::Claude, id: alondra_core::accounts::AccountId::parse(id).unwrap(), name: id.to_uppercase(), state: *state,
        native_identity: None, email: None, plan: None, billing: Default::default(), sign_ins: 0,
        profile: (*id != "default").then(|| format!("~/.alondra/accounts/{id}")),
    }).collect();
    must(s, Actor::System, Command::RecordAccounts { accounts, reserve: alondra_core::usage::default_reserve() });
}

fn account_state(s: &DomainState, id: &str) -> (alondra_core::accounts::AccountState, u64) {
    let status = s.accounts.iter().find(|status| status.id.as_str() == id).unwrap();
    (status.state, status.sign_ins)
}

fn options(s: &DomainState, card: &InteractionId) -> Vec<String> {
    let InteractionPayload::Question { options, .. } = &s.interactions[card].payload else { panic!("a question") };
    options.iter().map(|option| option.id.clone()).collect()
}

fn answer(s: &mut DomainState, card: &InteractionId, option: &str) {
    must(s, user(), Command::AnswerQuestion { attachments: vec![], interaction_id: card.clone(), option_id: option.into(), inputs: BTreeMap::new() });
}

pub(super) fn last(s: &DomainState, task: &str) -> LifecycleRecord {
    s.lifecycle_journal[&TaskId::from(task)].last().unwrap().clone()
}

pub(super) fn refused_unchanged(s: &mut DomainState, actor: Actor, command: Command) -> String {
    let before = s.clone();
    let code = exec(s, actor, command).unwrap_err().code;
    assert_eq!(*s, before, "a refused command changed the state");
    code
}

/// A task whose node rests on its turn card, told `text` and waiting for Claude to read it.
fn told(s: &mut DomainState, text: &str) -> String {
    let card = failed_turn(s, "t", "a");
    must(s, Actor::System, Command::RecordTurnBoundary { task_id: "t".into(), generation: 1, turn: "turn-1".into(), failure: None, failed: Some("fixture_error".into()) });
    must(s, user(), Command::AnswerQuestion { attachments: vec![], interaction_id: card.as_str().into(), option_id: CARRY_ON.into(),
        inputs: BTreeMap::from([(ANSWER_TEXT.to_owned(), InputValue::Text(text.into()))]) });
    let Some(LifecycleRecord::Answered { input, .. }) = s.lifecycle_journal[&TaskId::from("t")].last().cloned() else { panic!("answered") };
    input
}

/// The card an older release opened for an input Claude never read, in the journal as it was recorded.
fn dropped(s: &mut DomainState) -> (InteractionId, String) {
    let input = told(s, "Add the tests first");
    let Some(LifecycleRecord::Answered { text: Some(text), .. }) = s.lifecycle_journal[&TaskId::from("t")].last().cloned() else { panic!("answered") };
    let card = s.put_dropped_input(&TaskId::from("t"), 1, input.as_str(), &text).unwrap();
    (card, text)
}

/// An SDK task whose node declared work after the merge and whose PR was accepted and merged while
/// its turn runs that work.
fn working_after_the_merge(s: &mut DomainState) {
    sdk_turn(s, "t", "a");
    must(s, node("a"), Command::RegisterPr {
        task_id: "t".into(), pr_id: "pr".into(), host: "github.com".into(), repository: "owner/repo".into(), number: 1,
        head: "head1".into(), base: "main".into(), allowed_methods: vec![MergeMethod::Squash],
    });
    must(s, node("a"), Command::DeclarePostMergeWork { task_id: "t".into(), reason: Some("release.sh publishes 0.3.0".into()), waits_for: vec![] });
    let targets = vec![DeliveryTarget::PullRequest { pr_id: "pr".into(), head: "head1".into() }];
    must(s, node("a"), Command::PresentResult {
        checklist: Vec::new(), attachments: vec![], task_id: "t".into(), interaction_id: "final".into(), body: "Done".into(),
        targets: targets.clone(), is_final: true, nothing_to_review: false, card: CardSpec::delivery("Review the result", &[], true),
    });
    must(s, user(), Command::AcceptDelivery { presentation_id: "final".into(), targets });
    must(s, Actor::System, Command::ObservePrState {
        pr_id: "pr".into(), head: "head1".into(), base: "main".into(), state: PrState::Merged, merged_head: Some("head1".into()),
    });
    assert!(s.post_merge_owed(&s.tasks[&TaskId::from("t")]));
}

/// The user forces the running turn of `t` to stop and its host ends.
fn force_stopped(s: &mut DomainState) -> CommandOutcome {
    must(s, user(), Command::InterruptTurn { agent_id: "a".into() });
    must(s, Actor::System, Command::RecordInterruptAccepted { task_id: "t".into(), generation: 1, turn: "turn-1".into() });
    must(s, user(), Command::ForceStopTurn { agent_id: "a".into() });
    must(s, Actor::System, Command::RecordHostEnded { task_id: "t".into(), generation: 1, resumes: true, ended_work: vec![], by_alondra: false })
}

fn mixed_default_accounts(s: &mut DomainState, codex_state: alondra_core::accounts::AccountState) {
    use alondra_core::accounts::*;
    let accounts=[Provider::Claude,Provider::Codex].into_iter().map(|provider|AccountStatus {provider,id:AccountId::default_account(),name:format!("{provider:?} personal"),state:if provider==Provider::Codex {codex_state} else {AccountState::SignedIn},native_identity: None, email:None,plan:None,billing:AccountBilling::default(),sign_ins: 0, profile: None}).collect();
    must(s,Actor::System,Command::RecordAccounts {accounts,reserve:alondra_core::usage::default_reserve()});
}
