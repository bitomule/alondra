//! What the screens show of a paused node of the Agent SDK and what they offer for it: the engine's
//! pauses (`engine-sdk`'s `engine_tests::pauses_tests`) driven step by step, rendered by `views`.
use alondra_engine_sdk::engine_tests::sdk_node_fixture::*;
use alondra_engine_sdk::engine_tests::wired;
use alondra_engine_admission::AdmissionOf as _;
use alondra_engine_admission::admission::Key;
use alondra_engine_admission::pauses::Reason;
use alondra_domain_rules::AgentId;

/// Pausing interrupts the turn with what the node goes on with already told: no card asks the user
/// how it goes on, its task says it is paused, and resuming it is the user's one step.
#[tokio::test]
async fn pausing_an_sdk_node_raises_no_card_and_its_task_shows_it_paused() {
    wired();
    let mut node = SdkNode::working();
    node.pause();
    node.step();
    let state = node.domain.state().unwrap();
    let cards: Vec<_> = state.interactions.keys()
        .filter(|id| [alondra_domain_rules::TURN_ENDED_PREFIX, alondra_domain_rules::STALLED_PREFIX].iter().any(|prefix| id.0.starts_with(prefix)))
        .collect();
    assert!(cards.is_empty(), "a pause asks nothing: {cards:?}");
    let read = node.domain.read();
    let live = alondra_views::live::LiveSnapshot::default();
    let tasks = alondra_views::build("tasks", alondra_views::RenderContext {
        domain: read.state(), index: read.index(), live: &live, client: alondra_views::Client::Tui,
        lang: alondra_views::text::Lang::En, transport: alondra_views::Transport::Loopback, cursor: "test", rendered_at: "",
    }).unwrap();
    let tasks = serde_json::to_string(&tasks).unwrap();
    assert!(tasks.contains("\"Paused\""), "{tasks}");
    assert!(read.index().waiting().is_empty(), "the pause waits in nobody's inbox: {:?}", read.index().waiting());
    let screen = serde_json::to_string(&alondra_views::build(&format!("task:{SDK_TASK}"), alondra_views::RenderContext {
        domain: read.state(), index: read.index(), live: &live, client: alondra_views::Client::Tui,
        lang: alondra_views::text::Lang::En, transport: alondra_views::Transport::Loopback, cursor: "test", rendered_at: "",
    }).unwrap()).unwrap();
    assert!(screen.contains("Paused") && !screen.contains("Resume “"), "its screen says it is paused and shows no card: {screen}");
    drop(read);

    assert!(node.domain.admission().resume_now(&node.domain, &node.agent(), alondra_core::now_millis()));
    node.gate_look().await;
    node.until_told("resumed you");
    assert_eq!(node.prompts().len(), 2, "{:?}", node.prompts());
}

/// 17 November, 16:28: a message sent as the user's words to a node the user paused at 15:28 queued
/// its return, and the node started again at 16:51 with nobody pressing Resume. A pause of the user
/// ends only with a later Resume of the user's: the message waits with the task's inputs, the node
/// stays paused whatever slots free, and the Resume brings it back with the message.
#[tokio::test]
async fn the_users_own_message_to_a_paused_sdk_node_waits_for_the_users_resume() {
    wired();
    let mut node = SdkNode::working();
    node.pause();
    node.set_limit(1);
    let body = serde_json::from_value(serde_json::json!({"text": "Use the short changelog format"})).unwrap();
    crate::domain_messages::deliver(&node.domain, &node.home, &user(), "The user", &AgentId(node.agent()), body, true).await.unwrap();
    node.gate_look().await;
    node.step();
    assert!(node.domain.admission().paused(&node.domain, &node.agent()).is_some(), "the message does not end the user's pause");
    assert!(!node.gate().queue.iter().any(|waiting| waiting.key == Key::Resume(node.agent())), "nor does it queue the node's return");
    assert_eq!(node.dispatched_inputs(), 0, "nothing reaches the host while it is paused");
    let tasks = tasks_view(&node);
    assert!(offered(&tasks, &format!("resume:{}", node.agent())).is_some(), "its task offers Resume: {tasks}");

    assert!(node.domain.admission().resume_now(&node.domain, &node.agent(), alondra_core::now_millis()));
    node.gate_look().await;
    assert!(node.domain.admission().paused(&node.domain, &node.agent()).is_none(), "the user's Resume brings it back");
    node.until_told("Use the short changelog format");
}

/// 6 Nov 07:50: pausing working nodes was refused with invalid_input. The Pause a working node of
/// the Agent SDK offers carries its revision and is taken; the same id written by hand without it
/// is no action at all.
#[tokio::test]
async fn the_pause_a_working_sdk_node_offers_is_taken_and_one_without_its_revision_is_no_action() {
    wired();
    use alondra_views::action_id::{ActionId, Verb};
    let mut node = SdkNode::working();
    let agent = node.agent();
    let tasks = {
        let read = node.domain.read();
        let live = alondra_views::live::LiveSnapshot::default();
        serde_json::to_string(&alondra_views::build("tasks", alondra_views::RenderContext {
            domain: read.state(), index: read.index(), live: &live, client: alondra_views::Client::Tui,
            lang: alondra_views::text::Lang::En, transport: alondra_views::Transport::Loopback, cursor: "test", rendered_at: "",
        }).unwrap()).unwrap()
    };
    let start = tasks.find(&format!("\"pause:{agent}@")).unwrap_or_else(|| panic!("a working node offers Pause: {tasks}")) + 1;
    let offered = &tasks[start..start + tasks[start..].find('"').unwrap()];
    let action = ActionId::parse(offered).unwrap();
    assert_eq!((action.verb, action.subject.as_str()), (Verb::Pause, agent.as_str()));
    assert!(ActionId::parse(&format!("pause:{agent}")).is_err(), "an id without its revision is not one the apps send");

    node.domain.admission().pause_by_user(&node.domain, &action.subject, alondra_core::now_millis()).unwrap();
    node.step_until(SdkNode::turn_ended);
    let card = node_pause_card(&node.domain).expect("the pause has its card");
    assert_eq!(answer_resume(&node.domain, &card), Ok(()));
}

/// A node that waits for a slot says so with its place in the queue, and nobody is asked anything.
#[tokio::test]
async fn a_turn_waiting_for_a_slot_reads_waiting_for_capacity_and_raises_no_card() {
    wired();
    let mut node = SdkNode::resting();
    node.set_limit(0);
    let cards = node.cards();
    let delivered = node.dispatched_inputs();
    node.tell(SDK_TASK, "Go on");
    node.step();
    assert_eq!(node.dispatched_inputs(), delivered, "nothing more reached the host");
    assert_eq!(node.cards(), cards);
    assert_eq!(node.gate().working, 0, "an input held back takes no slot, so it never blocks what frees one");
    let read = node.domain.read();
    let live = alondra_views::live::LiveSnapshot { pacing: node.domain.admission().live(&node.domain, alondra_core::now_millis()), ..Default::default() };
    let tasks = alondra_views::build("tasks", alondra_views::RenderContext {
        domain: read.state(), index: read.index(), live: &live, client: alondra_views::Client::Tui,
        lang: alondra_views::text::Lang::En, transport: alondra_views::Transport::Loopback, cursor: "test", rendered_at: "",
    }).unwrap();
    let tasks = serde_json::to_string(&tasks).unwrap();
    assert!(tasks.contains("Waiting"), "{tasks}");
    drop(read);

    node.set_limit(1);
    node.gate_look().await;
    node.until_told("Go on");
}

#[cfg(test)]
fn tasks_view(node: &SdkNode) -> String {
    let read = node.domain.read();
    let live = alondra_views::live::LiveSnapshot { pacing: node.domain.admission().live(&node.domain, alondra_core::now_millis()), ..Default::default() };
    serde_json::to_string(&alondra_views::build("tasks", alondra_views::RenderContext {
        domain: read.state(), index: read.index(), live: &live, client: alondra_views::Client::Tui,
        lang: alondra_views::text::Lang::En, transport: alondra_views::Transport::Loopback, cursor: "test", rendered_at: "",
    }).unwrap()).unwrap()
}

#[cfg(test)]
fn offered(view: &str, prefix: &str) -> Option<alondra_views::action_id::ActionId> {
    let start = view.find(&format!("\"{prefix}@"))? + 1;
    alondra_views::action_id::ActionId::parse(&view[start..start + view[start..].find('"')?]).ok()
}

/// 17 November: four tasks the user had resumed waited to come back and their task offered no
/// action, not even Pause. Pause takes a node coming back out of the queue, and only the user's
/// Resume puts it back.
#[tokio::test]
async fn a_node_coming_back_from_a_pause_offers_pause_and_pausing_it_takes_it_out_of_the_queue() {
    wired();
    let mut node = SdkNode::working();
    node.pause();
    node.set_limit(0);
    let agent = node.agent();
    assert!(node.domain.admission().resume_now(&node.domain, &agent, alondra_core::now_millis()));
    node.gate_look().await;
    assert!(node.gate().queue.iter().any(|waiting| waiting.key == Key::Resume(agent.clone())), "it waits to come back");

    let tasks = tasks_view(&node);
    let pause = offered(&tasks, &format!("pause:{agent}")).unwrap_or_else(|| panic!("a node coming back offers Pause: {tasks}"));
    node.domain.admission().pause_by_user(&node.domain, &pause.subject, alondra_core::now_millis()).unwrap();
    node.gate_look().await;
    assert!(!node.gate().queue.iter().any(|waiting| waiting.key == Key::Resume(agent.clone())), "it left the queue");
    assert_eq!(node.domain.admission().paused(&node.domain, &agent).map(|paused| paused.reason), Some(Reason::ByUser));
    node.set_limit(1);
    node.gate_look().await;
    assert!(node.domain.admission().paused(&node.domain, &agent).is_some(), "a free slot does not bring it back");
    let tasks = tasks_view(&node);
    assert!(offered(&tasks, &format!("resume:{agent}")).is_some(), "it offers Resume: {tasks}");
}

/// A resting node whose next turn waits for a slot is in the queue too: Pause takes it out.
#[tokio::test]
async fn a_node_waiting_for_its_turn_offers_pause_and_pausing_it_takes_it_out_of_the_queue() {
    wired();
    let mut node = SdkNode::resting();
    node.set_limit(0);
    node.tell(SDK_TASK, "Go on");
    node.step();
    assert_eq!(node.waiting_turns(), [SDK_TASK]);
    let agent = node.agent();
    let tasks = tasks_view(&node);
    let pause = offered(&tasks, &format!("pause:{agent}")).unwrap_or_else(|| panic!("a node waiting for its turn offers Pause: {tasks}"));
    node.domain.admission().pause_by_user(&node.domain, &pause.subject, alondra_core::now_millis()).unwrap();
    node.gate_look().await;
    assert!(node.waiting_turns().is_empty(), "it left the queue");
    node.set_limit(1);
    node.gate_look().await;
    assert!(node.domain.admission().paused(&node.domain, &agent).is_some(), "a free slot does not bring it back");
}
