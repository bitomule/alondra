//! A node that stops with nothing explaining its next step is Alondra's to fix, never the user's:
//! its first three stops in a row go back to it at once, the next one is a case the orchestrator
//! settles with a closed outcome, and past the limit the only outcome is a task closed with a result.
use alondra_domain_model::*;
use alondra_views as views;
use alondra_domain::EMPTY_TURN_WAKES;
use proptest::prelude::*;
use std::collections::BTreeSet;

/// The task says on every client that it closed without a delivery, with what got done.
#[test]
fn a_task_closed_for_its_stops_says_so_with_its_result_on_every_client() {
    use alondra_core::text::Lang;
    use alondra_views::{Client, RenderContext, Transport, build, live::LiveSnapshot};
    let render = |s: &DomainState, client: Client, lang: Lang| {
        let index = ReadIndex::build(s);
        let live = LiveSnapshot::default();
        serde_json::to_string(&build("task:t", RenderContext { domain: s, index: &index, live: &live, client, lang,
            transport: Transport::Loopback, cursor: "test", rendered_at: "" }).unwrap()).unwrap()
    };
    let mut s = first_case();
    settle(&mut s, orchestrator(), StallOutcome::Close { result: "Half the fix is on branch fix/queue.".into() }).unwrap();
    let mut unsettled = first_case();
    settle(&mut unsettled, Actor::System, StallOutcome::Unsettled).unwrap();
    for client in [Client::Ios, Client::Mac, Client::Tui] {
        let en = render(&s, client, Lang::En);
        assert!(en.contains("Cancelled") && en.contains("Half the fix is on branch fix/queue."), "{client:?}: {en}");
        let es = render(&s, client, Lang::Es);
        assert!(es.contains("Cancelada") && es.contains("Half the fix is on branch fix/queue."), "{client:?}: {es}");
        let es = render(&unsettled, client, Lang::Es);
        assert!(es.contains("Cancelada") && es.contains("El encargo quedó sin atender"), "{client:?}: {es}");
    }
}

fn orchestrator() -> Actor {
    Actor::Orchestrator { generation: 0 }
}

fn lifecycle(s: &DomainState) -> std::sync::Arc<alondra_domain::TaskLifecycle> {
    s.lifecycle_of(&"t".into()).unwrap()
}

fn end_empty(s: &mut DomainState, turn: &str) -> CommandOutcome {
    must(s, Actor::System, Command::RecordTurnBoundary { task_id: "t".into(), generation: 1, turn: turn.into(), failure: None, failed: None })
}

/// Starts `turn` with what the node was told last, the wake or the orchestrator's instruction.
fn start(s: &mut DomainState, turn: &str) {
    let input = lifecycle(s).pending_inputs().first().map(|input| input.as_str().to_owned()).expect("something to start the turn with");
    must(s, Actor::System, Command::RecordRuntimeFacts { task_id: "t".into(), generation: 1, facts: vec![
        RuntimeObservation::TurnStarted { turn: turn.into(), input: input.into() },
    ] });
}

fn open_case(s: &DomainState) -> Option<InteractionId> {
    s.interactions.values().find(|i| is_stall_case(i) && s.open(i)).map(|i| i.id.clone())
}

fn settle(s: &mut DomainState, actor: Actor, outcome: StallOutcome) -> Result<CommandOutcome, DomainError> {
    let case = open_case(s).expect("an open case");
    exec(s, actor, Command::SettleStall { case_id: case, task_id: "t".into(), outcome })
}

/// The node of task `t` stopped and was handed back its turn `EMPTY_TURN_WAKES` times in a row,
/// and stopped again: the first case is open.
fn first_case() -> DomainState {
    let mut s = world();
    sdk_turn(&mut s, "t", "a");
    end_empty(&mut s, "turn-1");
    assert_eq!(stops_until_a_case(&mut s), u32::from(EMPTY_TURN_WAKES));
    s
}

/// The node of task `t` starts with what it was told and stops, again and again, until a case
/// opens: how many of those stops were handed back to it.
fn stops_until_a_case(s: &mut DomainState) -> u32 {
    let mut handed_back = 0;
    while open_case(s).is_none() {
        assert!(matches!(last(s, "t"), LifecycleRecord::TurnEnded { wake: Some(_), .. } | LifecycleRecord::Answered { .. }), "{:?}", last(s, "t"));
        handed_back += u32::from(matches!(last(s, "t"), LifecycleRecord::TurnEnded { .. }));
        let turn = format!("turn-at-{}", s.revision);
        start(s, &turn);
        end_empty(s, &turn);
    }
    handed_back
}

pub(super) fn user() -> Actor {
    Actor::User { user_id: "u".into() }
}

pub(super) fn node(id: &str) -> Actor {
    Actor::Agent { agent_id: id.into() }
}

pub(super) fn exec(s: &mut DomainState, actor: Actor, command: Command) -> Result<CommandOutcome, DomainError> {
    s.seed_mergeable_for_presented(&command);
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

pub(super) fn last(s: &DomainState, task: &str) -> LifecycleRecord {
    s.lifecycle_journal[&TaskId::from(task)].last().unwrap().clone()
}
