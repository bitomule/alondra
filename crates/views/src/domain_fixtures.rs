//! Domains the screen tests start from, built by the domain's own commands.
use alondra_domain_rules::*;

/// The inbox as the index lists it, each card with its buttons.
pub(crate) fn inbox_items(state: &DomainState) -> Vec<InboxItem> {
    ReadIndex::build(state).queued(state).map(std::borrow::Cow::into_owned).collect()
}

pub(crate) use prs::{answered_while_its_turn_waits, two_open_prs};
pub(crate) use mcp::waiting_for_linear;

mod prs {
    use alondra_domain_rules::*;
    #[allow(unused_imports)]
    use std::collections::{BTreeMap, BTreeSet};

const AGENT: &str = "a";

const TASK: &str = "t";

const PUBLIC: &str = "github.com/bitomule/alondra#17";

const FIRST_HEAD: &str = "1111111111111111111111111111111111111111";

const CLOUD_HEAD: &str = "6666666666666666666666666666666666666666";

fn exec(s: &mut DomainState, actor: Actor, command: Command) -> Result<CommandOutcome, DomainError> {
    s.execute(&actor, &CommandEnvelope {
        protocol_version: PROTOCOL_VERSION, data_epoch: s.data_epoch.clone(), command_id: format!("c-{}", s.revision).into(),
        expected_revision: s.revision, command,
    })
}

fn must(s: &mut DomainState, actor: Actor, command: Command) {
    let shown = format!("{command:?}");
    if let Err(error) = exec(s, actor, command) {
        panic!("{shown} was refused: {error}");
    }
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
        task_id: TASK.into(), request_id: "r".into(), title: "Fix the phone menu".into(), agent_id: None, review_prs: false,
        dependencies: BTreeSet::new(), stack: None, launch: Some(LaunchPlan::new("node", "/tmp")),
    });
    must(&mut s, Actor::System, Command::AdmitExecution { provider: alondra_core::accounts::Provider::Claude, task_id: TASK.into(), agent_id: AGENT.into(),
        conversation: "conversation".into(), instance: "instance".into(), host: "sdk-host".into(), model: None, account: None, leaves_limit: None });
    let LifecycleRecord::Planned { input } = s.lifecycle_journal[&TaskId::from(TASK)][0].clone() else { panic!("planned first") };
    must(&mut s, Actor::System, Command::RecordRuntimeFacts { task_id: TASK.into(), generation: 1, facts: vec![
        RuntimeObservation::ResourceAvailable,
        RuntimeObservation::TurnStarted { turn: TURN.into(), input },
    ] });
    s
}

const TURN: &str = "turn-1";

/// The node's turn ends after what it did in it.
fn turn_ends(s: &mut DomainState) {
    must(s, Actor::System, Command::RecordTurnBoundary { task_id: TASK.into(), generation: 1, turn: TURN.into(), failure: None, failed: None });
}

fn register(s: &mut DomainState, repository: &str, number: u64, head: &str) {
    must(s, node(), Command::RegisterPr {
        task_id: TASK.into(), pr_id: hosted_pr_id("github.com", repository, number), host: "github.com".into(), repository: repository.into(),
        number, head: head.into(), base: "main".into(), allowed_methods: vec![MergeMethod::Squash],
    });
}

fn result(id: &str, targets: Vec<DeliveryTarget>, is_final: bool) -> Command {
    Command::PresentResult {
        checklist: Vec::new(), attachments: vec![], task_id: TASK.into(), interaction_id: id.into(), body: "Done".into(),
        targets, is_final, nothing_to_review: false, card: CardSpec::delivery("Review the result", &[], is_final),
    }
}

fn pr(id: &str, head: &str) -> DeliveryTarget {
    DeliveryTarget::PullRequest { pr_id: id.into(), head: head.into() }
}

/// The node asked the user in writing and ended its turn on the card; the user answered, and the
/// delivery outbox has not told the node yet.
pub(crate) fn answered_while_its_turn_waits() -> DomainState {
    let mut s = at_work();
    must(&mut s, node(), Command::AskQuestion {
        attachments: vec![], interaction_id: "where".into(), scope: Scope::Task { task_id: TASK.into() }, recipient: Recipient::User,
        body: "Where does the ledger live?".into(), blocking: true, options: vec![],
        card: CardSpec::derived(CardKind::Provide, "Say where the ledger lives"),
    });
    turn_ends(&mut s);
    must(&mut s, user(), Command::AnswerQuestion { attachments: vec![], interaction_id: "where".into(), option_id: String::new(),
        inputs: BTreeMap::from([("text".to_owned(), InputValue::Text("One file per check".into()))]) });
    s
}

/// A task with two open PRs in two repositories, both in its final delivery.
pub(crate) fn two_open_prs() -> DomainState {
    let mut s = at_work();
    register(&mut s, "bitomule/alondra", 17, FIRST_HEAD);
    register(&mut s, "acme/storefront", 6, CLOUD_HEAD);
    must(&mut s, node(), result("final", vec![pr(PUBLIC, "")], true));
    turn_ends(&mut s);
    s
}
}

mod mcp {
    use alondra_domain_rules::*;
    #[allow(unused_imports)]
    use std::collections::{BTreeMap, BTreeSet};

fn server(name: &str, status: McpStatus) -> McpServerState {
    McpServerState { name: name.into(), status, error: None, source: None, transport: Some("http".into()), url: Some(format!("https://mcp.{name}.app/mcp")), traits: Default::default() }
}

fn report(task: &str, servers: Vec<McpServerState>) -> Command {
    Command::RecordMcpServers { task_id: task.into(), generation: 1, servers }
}

fn wait(task: &str, name: &str) -> Command {
    Command::WaitForMcp { task_id: task.into(), server: name.into() }
}

/// Two SDK tasks whose nodes both report Linear signed out, and a third node with it connected.
fn signed_out() -> DomainState {
    let mut s = world();
    must(&mut s, Actor::System, Command::RegisterAgent { agent_id: "c".into(), name: "c".into(), at: 0 });
    for (task, agent) in [("t", "a"), ("u", "b"), ("v", "c")] {
        sdk_turn(&mut s, task, agent);
    }
    for task in ["t", "u"] {
        must(&mut s, Actor::System, report(task, vec![server("linear", McpStatus::NeedsAuth), server("files", McpStatus::Connected)]));
    }
    must(&mut s, Actor::System, report("v", vec![server("linear", McpStatus::NeedsAuth), server("off", McpStatus::Disabled)]));
    s
}

/// Tasks t and u wait for Linear; Waiting for You shows one card for both.
pub(crate) fn waiting_for_linear() -> DomainState {
    let mut s = signed_out();
    must(&mut s, node("a"), wait("t", "linear"));
    must(&mut s, node("b"), wait("u", "linear"));
    s
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
}
