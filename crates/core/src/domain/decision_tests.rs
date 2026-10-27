//! Decisions Claude asks for through a task's SDK execution: what Alondra decides by itself, the
//! cards the user decides the rest on, and what each answer tells Claude.
use super::lifecycle_tests::{assert_aggregate_rules, deliver, exec, kinds, last, must, node, refused_unchanged, sdk_turn, task, user, world};
use super::*;
use serde_json::json;
use std::collections::BTreeMap;

fn alondra_server() -> McpServerOrigin {
    McpServerOrigin { name: ALONDRA_MCP_SERVER.into(), source: PLUGIN_SOURCE.into() }
}
fn card_prompt(hold: Hold, body: &str) -> DecisionPrompt {
    let options = hold.options_offering(hold == Hold::Permission);
    let card = match hold {
        Hold::Permission => CardSpec::new(CardKind::Authorize, body),
        Hold::Plan => CardSpec::new(CardKind::Review, "Review the plan").content(ContentKind::Plan, body),
        Hold::Question | Hold::Elicitation => CardSpec::new(CardKind::Provide, body),
    };
    DecisionPrompt::Card { body: body.into(), options, card }
}
fn question_prompt(question: &str, labels: &[&str]) -> DecisionPrompt {
    let options: Vec<QuestionOption> = labels.iter().enumerate()
        .map(|(at, label)| QuestionOption { id: format!("o{at}"), label: (*label).into(), description: None })
        .collect();
    DecisionPrompt::Card { body: question.into(), options, card: CardSpec::new(CardKind::Decide, question) }
}
fn request(epoch: u64, hold: Hold, tool: Option<&str>, input: serde_json::Value, mcp_server: Option<McpServerOrigin>, prompt: DecisionPrompt) -> Command {
    Command::RecordDecisionRequest { task_id: "t".into(), generation: 1, epoch, hold, tool: tool.map(Into::into), input, mcp_server, prompt }
}
fn bash(epoch: u64, command: &str) -> Command {
    request(epoch, Hold::Permission, Some("Bash"), json!({"command": command}), None, card_prompt(Hold::Permission, &format!("Run {command}")))
}
fn answer(card: &str, option: &str, note: Option<&str>) -> Command {
    let inputs = note.map(|note| BTreeMap::from([(ANSWER_TEXT.to_owned(), InputValue::Text(note.into()))])).unwrap_or_default();
    Command::AnswerQuestion { attachments: vec![], interaction_id: card.into(), option_id: option.into(), inputs }
}
fn card_of(epoch: u64) -> String {
    decision_card_id(&"t".into(), 1, epoch).0
}
fn held(s: &DomainState) -> Vec<String> {
    replay(&"t".into(), &s.lifecycle_journal[&TaskId::from("t")]).unwrap().cards().into_iter().map(|(card, _)| card.0.clone()).collect()
}
fn plan(checked: bool) -> String {
    if checked { "# Plan\n\n- [ ] Add the parser\n- [ ] Test it\n".into() } else { "# Plan\n\nAdd the parser and test it.".into() }
}

#[test]
fn alondras_own_tools_pass_only_from_the_alondra_plugins_server() {
    let ask = Some("mcp__plugin_alondra_alondra__ask");
    let server = |name: &str, source: &str| McpServerOrigin { name: name.into(), source: source.into() };
    for (tool, origin, allowed) in [
        (ask, Some(alondra_server()), true),
        (ask, Some(server(ALONDRA_MCP_SERVER, "user")), false),
        (ask, Some(server(ALONDRA_MCP_SERVER, "project")), false),
        (ask, Some(server("alondra", PLUGIN_SOURCE)), false),
        (ask, None, false),
        (Some("mcp__other__ask"), Some(alondra_server()), false),
        (Some("Bash"), Some(alondra_server()), false),
        (None, Some(alondra_server()), false),
    ] {
        let decided = decision_policy(Hold::Permission, tool, &json!({}), origin.as_ref(), None);
        assert_eq!(decided, allowed.then_some(DecisionRecord::Allow), "{tool:?} from {origin:?}");
    }
    for hold in [Hold::Question, Hold::Elicitation] {
        assert_eq!(decision_policy(hold, ask, &json!({}), Some(&alondra_server()), None), None, "{hold:?} is always asked");
    }
}

#[test]
fn what_nodes_may_never_do_and_a_plan_without_its_checklist_are_denied() {
    let deny = |tool: &str, input: serde_json::Value| decision_policy(Hold::Permission, Some(tool), &input, None, None);
    let Some(DecisionRecord::Deny { message }) = deny("Bash", json!({"command": "alondra answer 55 Merge"})) else { panic!("a user-only command") };
    assert!(message.contains("act as the user"), "{message}");
    let Some(DecisionRecord::Deny { message }) = deny("Bash", json!({"command": "sqlite3 /Users/x/.alondra/domain.db 'select 1'"})) else { panic!("a writable database") };
    assert!(message.contains("sqlite3 -readonly"), "{message}");
    assert_eq!(deny("Bash", json!({"command": "sqlite3 -readonly /Users/x/.alondra/domain.db 'select 1'"})), None);
    assert_eq!(deny("Bash", json!({"command": "make test"})), None);
    assert!(matches!(decision_policy(Hold::Plan, Some("ExitPlanMode"), &json!({}), None, Some(&plan(false))), Some(DecisionRecord::Deny { .. })));
    assert_eq!(decision_policy(Hold::Plan, Some("ExitPlanMode"), &json!({}), None, Some(&plan(true))), None);
}

#[test]
fn a_plan_claude_never_wrote_is_sent_back_to_be_written() {
    for unwritten in [None, Some(""), Some(" \n")] {
        assert_eq!(decision_policy(Hold::Plan, Some("ExitPlanMode"), &json!({}), None, unwritten),
            Some(DecisionRecord::Deny { message: crate::domain::UNWRITTEN_PLAN.into() }), "{unwritten:?}");
    }
}

#[test]
fn a_permission_card_is_decided_by_the_user_and_settles_inside_its_turn() {
    let mut s = world();
    sdk_turn(&mut s, "t", "a");
    let outcome = must(&mut s, Actor::System, bash(7, "make test"));
    let card = card_of(7);
    assert_eq!(outcome.created_ids, vec![card.clone()]);
    assert_eq!(last(&s, "t"), LifecycleRecord::DecisionAsked { card: card.as_str().into(), epoch: 7, reason: DecisionReason::Permission });
    assert_eq!(s.holds.get(&InteractionId::from(card.as_str())), Some(&Hold::Permission));
    assert_eq!(held(&s), vec![card.clone()]);
    assert_eq!(s.tasks[&TaskId::from("t")].state, TaskState::NeedsYou);
    assert_aggregate_rules(&s);
    must(&mut s, user(), answer(&card, Hold::ALLOW, None));
    assert_eq!(last(&s, "t"), LifecycleRecord::Decided { card: card.as_str().into(), epoch: 7, decision: DecisionRecord::Allow });
    assert!(held(&s).is_empty());
    assert_eq!(s.tasks[&TaskId::from("t")].state, TaskState::Running);
    assert_aggregate_rules(&s);
    let settle = || Command::RecordDecisionSettled { task_id: "t".into(), generation: 1, epoch: 7 };
    must(&mut s, Actor::System, settle());
    assert_eq!(kinds(&s, "t").iter().rev().take(2).cloned().collect::<Vec<_>>(), ["decision_continued", "decision_settled"]);
    assert_eq!(replay(&"t".into(), &s.lifecycle_journal[&TaskId::from("t")]).unwrap().activity(), alondra_domain::ActivityView::Working);
    assert_eq!(refused_unchanged(&mut s, Actor::System, settle()), "conflict", "a decision settles once");
    assert_aggregate_rules(&s);
}

/// Every answer a card offers, and what Claude receives for it.
#[test]
fn every_answer_becomes_the_decision_claude_receives() {
    let permission = || card_prompt(Hold::Permission, "Run make test");
    let cases: Vec<(Hold, DecisionPrompt, &str, Option<&str>, DecisionRecord)> = vec![
        (Hold::Permission, permission(), Hold::ALLOW, None, DecisionRecord::Allow),
        (Hold::Permission, permission(), Hold::ALWAYS, None, DecisionRecord::Allow),
        (Hold::Permission, permission(), Hold::DENY, None, DecisionRecord::Deny { message: "The user denied it".into() }),
        (Hold::Permission, permission(), Hold::DENY, Some("Use make check"), DecisionRecord::Deny { message: "The user denied it: Use make check".into() }),
        (Hold::Plan, card_prompt(Hold::Plan, &plan(true)), Hold::APPROVE, None, DecisionRecord::Allow),
        (Hold::Plan, card_prompt(Hold::Plan, &plan(true)), Hold::REQUEST_CHANGES, None, DecisionRecord::Deny { message: "The user requests changes to the plan".into() }),
        (Hold::Plan, card_prompt(Hold::Plan, &plan(true)), Hold::REQUEST_CHANGES, Some("Split it"), DecisionRecord::Deny { message: "The user requests changes to the plan: Split it".into() }),
        (Hold::Question, question_prompt("Which database?", &["SQLite", "Postgres"]), "o1", None, DecisionRecord::Answers { answers: vec!["Postgres".into()] }),
        (Hold::Elicitation, card_prompt(Hold::Elicitation, "The server asks for a token"), Hold::ACCEPT, Some("abc"), DecisionRecord::Accept { text: "abc".into() }),
        (Hold::Elicitation, card_prompt(Hold::Elicitation, "The server asks for a token"), Hold::ACCEPT, None, DecisionRecord::Accept { text: String::new() }),
        (Hold::Elicitation, card_prompt(Hold::Elicitation, "The server asks for a token"), Hold::DECLINE, None, DecisionRecord::Decline),
    ];
    for (hold, prompt, option, note, expected) in cases {
        let mut s = world();
        sdk_turn(&mut s, "t", "a");
        must(&mut s, Actor::System, request(3, hold, None, json!({}), None, prompt));
        must(&mut s, user(), answer(&card_of(3), option, note));
        assert_eq!(last(&s, "t"), LifecycleRecord::Decided { card: card_of(3).as_str().into(), epoch: 3, decision: expected }, "{hold:?} {option} {note:?}");
        if hold == Hold::Plan && option == Hold::APPROVE {
            assert!(s.checklists.contains_key(&TaskId::from("t")), "an approved plan is what the delivery is held to");
        }
        assert_aggregate_rules(&s);
    }
}

#[test]
fn questions_asked_together_are_decided_once_the_last_is_answered() {
    let mut s = world();
    sdk_turn(&mut s, "t", "a");
    let question = |need: &str, labels: &[&str]| GroupQuestion {
        need: need.into(), why: String::new(), recommended: None,
        options: labels.iter().enumerate().map(|(at, label)| QuestionOption { id: format!("o{at}"), label: (*label).into(), description: None }).collect(),
    };
    let prompt = DecisionPrompt::Questions { questions: vec![question("Which database?", &["SQLite", "Postgres"]), question("Which port?", &["5432", "6543"])] };
    let outcome = must(&mut s, Actor::System, request(4, Hold::Question, Some("AskUserQuestion"), json!({}), None, prompt));
    let group = InteractionId(card_of(4));
    let (first, second) = (QuestionGroup::card_id(&group, 1), QuestionGroup::card_id(&group, 2));
    assert_eq!(outcome.created_ids, vec![first.0.clone(), second.0.clone()]);
    assert_eq!(held(&s), vec![first.0.clone()], "one decision, held by the group's first card");
    must(&mut s, user(), answer(&second.0, "o0", None));
    assert_eq!(held(&s), vec![first.0.clone()], "nothing is decided before the last answer");
    assert_aggregate_rules(&s);
    must(&mut s, user(), Command::ReopenAnswer { interaction_id: second.clone() });
    must(&mut s, user(), answer(&second.0, "o1", None));
    must(&mut s, user(), answer(&first.0, "o1", None));
    assert_eq!(last(&s, "t"), LifecycleRecord::Decided { card: first.clone(), epoch: 4, decision: DecisionRecord::Answers { answers: vec!["Postgres".into(), "6543".into()] } });
    assert_eq!(exec(&mut s, user(), Command::ReopenAnswer { interaction_id: second.clone() }).unwrap_err().code, "question_already_answered");
    assert!(held(&s).is_empty());
    assert_aggregate_rules(&s);
}

#[test]
fn questions_asked_together_are_answered_in_any_order_and_decided_once() {
    let question = |need: &str, labels: &[&str]| GroupQuestion {
        need: need.into(), why: String::new(), recommended: None,
        options: labels.iter().enumerate().map(|(at, label)| QuestionOption { id: format!("o{at}"), label: (*label).into(), description: None }).collect(),
    };
    let asked = |s: &mut DomainState| {
        sdk_turn(s, "t", "a");
        let prompt = DecisionPrompt::Questions { questions: vec![question("Which database?", &["SQLite", "Postgres"]), question("Which port?", &["5432", "6543"])] };
        must(s, Actor::System, request(4, Hold::Question, Some("AskUserQuestion"), json!({}), None, prompt));
        let group = InteractionId(card_of(4));
        (QuestionGroup::card_id(&group, 1), QuestionGroup::card_id(&group, 2))
    };
    let decided = |s: &DomainState| s.lifecycle_journal[&TaskId::from("t")].iter().filter(|record| matches!(record, LifecycleRecord::Decided { .. })).count();

    let mut s = world();
    let (first, second) = asked(&mut s);
    must(&mut s, user(), answer(&first.0, "o1", None));
    assert_eq!(last(&s, "t"), LifecycleRecord::CardMoved { from: first.clone(), to: second.clone() });
    assert_eq!(held(&s), vec![second.0.clone()], "the decision waits on the question still open");
    assert_eq!(decided(&s), 0);
    assert_aggregate_rules(&s);
    must(&mut s, user(), answer(&second.0, "o0", None));
    assert_eq!(last(&s, "t"), LifecycleRecord::Decided { card: second.clone(), epoch: 4, decision: DecisionRecord::Answers { answers: vec!["Postgres".into(), "5432".into()] } });
    assert_eq!(decided(&s), 1);
    assert!(held(&s).is_empty());
    assert_aggregate_rules(&s);

    let mut s = world();
    let (first, second) = asked(&mut s);
    must(&mut s, user(), answer(&first.0, "o0", None));
    must(&mut s, user(), Command::ReopenAnswer { interaction_id: first.clone() });
    assert_eq!(held(&s), vec![second.0.clone()], "a reopened answer leaves the decision where it waits");
    must(&mut s, user(), answer(&second.0, "o1", None));
    assert_eq!(last(&s, "t"), LifecycleRecord::CardMoved { from: second.clone(), to: first.clone() });
    assert_eq!(held(&s), vec![first.0.clone()], "the reopened question holds it again");
    assert_aggregate_rules(&s);
    must(&mut s, user(), answer(&first.0, "o1", None));
    assert_eq!(last(&s, "t"), LifecycleRecord::Decided { card: first.clone(), epoch: 4, decision: DecisionRecord::Answers { answers: vec!["Postgres".into(), "6543".into()] } });
    assert_eq!(decided(&s), 1);
    assert_aggregate_rules(&s);
}

#[test]
fn alondra_and_forbidden_tools_are_decided_without_a_card() {
    let ask = Some("mcp__plugin_alondra_alondra__ask");
    let mut s = world();
    sdk_turn(&mut s, "t", "a");
    let cards = s.interactions.len();
    must(&mut s, Actor::System, request(1, Hold::Permission, ask, json!({}), Some(alondra_server()), card_prompt(Hold::Permission, "Ask the user")));
    assert_eq!(last(&s, "t"), LifecycleRecord::DecidedByPolicy { epoch: 1, decision: DecisionRecord::Allow });
    must(&mut s, Actor::System, bash(2, "alondra send orchestrator hello"));
    let LifecycleRecord::DecidedByPolicy { epoch: 2, decision: DecisionRecord::Deny { message } } = last(&s, "t") else { panic!("denied by policy") };
    assert!(message.contains("act as the user"), "{message}");
    must(&mut s, Actor::System, request(3, Hold::Plan, Some("ExitPlanMode"), json!({}), None, card_prompt(Hold::Plan, &plan(false))));
    assert!(matches!(last(&s, "t"), LifecycleRecord::DecidedByPolicy { epoch: 3, decision: DecisionRecord::Deny { .. } }));
    assert_eq!(s.interactions.len(), cards, "no card for what Alondra decides");
    assert!(held(&s).is_empty());
    assert_aggregate_rules(&s);
    let imitation = McpServerOrigin { name: ALONDRA_MCP_SERVER.into(), source: "user".into() };
    must(&mut s, Actor::System, request(5, Hold::Permission, ask, json!({}), Some(imitation), card_prompt(Hold::Permission, "Ask the user")));
    assert_eq!(held(&s), vec![card_of(5)], "a tool that only imitates the name is put to the user");
    assert_aggregate_rules(&s);
}

#[test]
fn an_aborted_request_takes_its_card_and_a_late_answer_is_refused() {
    let mut s = world();
    sdk_turn(&mut s, "t", "a");
    must(&mut s, Actor::System, bash(6, "make deploy"));
    must(&mut s, Actor::System, Command::RecordDecisionAborted { task_id: "t".into(), generation: 1, epoch: 6 });
    assert_eq!(last(&s, "t"), LifecycleRecord::DecisionAborted { epoch: 6, unheard_card: decision_cancelled_card_id(&"t".into(), 1, 6) });
    assert!(s.interactions[&InteractionId(card_of(6))].withdrawn);
    assert!(held(&s).is_empty(), "the turn that asked still runs: nothing else is asked");
    assert!(!s.interactions.contains_key(&decision_cancelled_card_id(&"t".into(), 1, 6)));
    assert_eq!(refused_unchanged(&mut s, user(), answer(&card_of(6), Hold::ALLOW, None)), "withdrawn");
    assert_aggregate_rules(&s);
}

#[test]
fn a_request_aborted_after_its_turn_ended_asks_how_the_task_goes_on() {
    use crate::views::{Client, RenderContext, Transport, build, live::LiveSnapshot, text::Lang};
    let mut s = world();
    sdk_turn(&mut s, "t", "a");
    must(&mut s, Actor::System, bash(6, "make deploy"));
    must(&mut s, Actor::System, Command::RecordTurnBoundary { task_id: "t".into(), generation: 1, turn: "turn-1".into(), failure: None, failed: None });
    assert_eq!(held(&s), vec![card_of(6)], "the request explains the next step");
    let outcome = must(&mut s, Actor::System, Command::RecordDecisionAborted { task_id: "t".into(), generation: 1, epoch: 6 });
    let cancelled = decision_cancelled_card_id(&"t".into(), 1, 6);
    assert_eq!(held(&s), vec![cancelled.0.clone()]);
    assert!(crate::views::id::valid(&cancelled.0));
    assert!(outcome.effects.iter().any(|effect| matches!(effect, DomainEffect::NotifyUser { interaction_id, .. } if *interaction_id == cancelled)));
    assert_aggregate_rules(&s);
    let index = ReadIndex::build(&s);
    let live = LiveSnapshot::default();
    let view = build("task:t", RenderContext { domain: &s, index: &index, live: &live, client: Client::Tui, lang: Lang::Es,
        transport: Transport::Loopback, cursor: "test", rendered_at: "" }).unwrap();
    let view = serde_json::to_string(&view).unwrap();
    assert!(view.contains("dejó de esperar tu decisión"), "{view}");
    must(&mut s, user(), answer(&cancelled.0, CARRY_ON, None));
    assert!(matches!(last(&s, "t"), LifecycleRecord::Answered { .. }));
    assert_aggregate_rules(&s);
}

#[test]
fn only_alondra_records_decisions_of_the_tasks_current_sdk_execution() {
    let mut s = world();
    sdk_turn(&mut s, "t", "a");
    assert_eq!(refused_unchanged(&mut s, user(), bash(1, "make")), "forbidden");
    assert_eq!(refused_unchanged(&mut s, node("a"), bash(1, "make")), "forbidden");
    let Command::RecordDecisionRequest { task_id, epoch, hold, tool, input, mcp_server, prompt, .. } = bash(1, "make") else { unreachable!() };
    let stale = Command::RecordDecisionRequest { task_id, generation: 2, epoch, hold, tool, input, mcp_server, prompt };
    assert_eq!(refused_unchanged(&mut s, Actor::System, stale), "stale_execution");
    let together = DecisionPrompt::Questions { questions: vec![] };
    assert_eq!(refused_unchanged(&mut s, Actor::System, request(1, Hold::Permission, Some("Bash"), json!({}), None, together)), "invalid_input");
    must(&mut s, Actor::System, bash(1, "make"));
    assert_eq!(refused_unchanged(&mut s, Actor::System, bash(1, "make")), "invalid_input", "one card per request");
    assert_eq!(refused_unchanged(&mut s, Actor::System, Command::RecordDecisionSettled { task_id: "t".into(), generation: 1, epoch: 1 }), "conflict", "nothing was decided yet");
    assert_eq!(refused_unchanged(&mut s, Actor::System, Command::RecordDecisionAborted { task_id: "t".into(), generation: 1, epoch: 9 }), "conflict", "nothing was asked at 9");
    task(&mut s, "u", Some("b"));
    must(&mut s, node("b"), Command::StartTask { task_id: "u".into() });
    let bg = Command::RecordDecisionSettled { task_id: "u".into(), generation: 1, epoch: 1 };
    assert_eq!(refused_unchanged(&mut s, Actor::System, bg), "invalid_input");
    assert!(exec(&mut s, Actor::System, Command::RecordDecisionAborted { task_id: "missing".into(), generation: 1, epoch: 1 }).is_err());
}

/// The notification of an SDK permission card answers the same request the card does: its buttons
/// are the card's own options, bound to the card's id and revision, which carry the request's
/// generation and epoch.
#[test]
fn the_push_of_an_sdk_permission_card_offers_its_answers_bound_to_the_same_request() {
    use crate::views::{Client, Lang, LiveSnapshot, RenderContext, Transport, build, push_actions};
    let mut s = world();
    sdk_turn(&mut s, "t", "a");
    must(&mut s, Actor::System, bash(7, "make test"));
    let card = card_of(7);
    let read = ReadSnapshot::build(std::sync::Arc::new(s.clone()));
    let pushed = push_actions(&read, &card, Lang::En).unwrap();
    let options: Vec<&str> = pushed.iter().filter_map(|action| action.option.as_deref()).collect();
    assert_eq!(options, ["allow", "always", "deny"], "{pushed:?}");
    let view = build(&format!("card:{card}"), RenderContext {
        domain: &s, index: read.index(), live: &LiveSnapshot::default(), client: Client::Ios, lang: Lang::En,
        transport: Transport::Relay, cursor: "", rendered_at: "",
    }).unwrap();
    let rendered = serde_json::to_string(&view).unwrap();
    for action in pushed.iter().filter(|action| action.option.is_some()) {
        assert!(action.id.starts_with(&format!("ans:{card}@")), "bound to the card and its revision: {}", action.id);
        assert!(rendered.contains(&action.id), "the card offers the same action: {}", action.id);
    }
}

// What the native Claude 2.1.300 did with a background subagent: it asks for a permission, its
// root turn ends, the user allows it, the subagent goes on with the answer and ends, and Claude
// takes up its notification in a turn of its own, which rests on the task's one card.
#[test]
fn a_background_subagents_answered_permission_goes_on_in_it_and_the_turn_it_wakes_ends_on_one_card() {
    let mut s = world();
    sdk_turn(&mut s, "t", "a");
    must(&mut s, Actor::System, Command::RecordRuntimeFacts { task_id: "t".into(), generation: 1, facts: vec![
        RuntimeObservation::WorkStarted { turn: "turn-1".into(), work: "claude-task:agent".into() },
    ] });
    must(&mut s, Actor::System, bash(7, "echo draft > parser-draft.txt"));
    must(&mut s, Actor::System, Command::RecordTurnBoundary { task_id: "t".into(), generation: 1, turn: "turn-1".into(), failure: None, failed: None });
    must(&mut s, user(), answer(&card_of(7), Hold::ALLOW, None));
    must(&mut s, Actor::System, Command::RecordDecisionSettled { task_id: "t".into(), generation: 1, epoch: 7 });
    assert_eq!(kinds(&s, "t").iter().rev().take(2).cloned().collect::<Vec<_>>(), ["decision_continued", "decision_settled"],
        "the subagent that asked goes on with the answer: no root turn owes it");
    must(&mut s, Actor::System, Command::RecordRuntimeFacts { task_id: "t".into(), generation: 1, facts: vec![
        RuntimeObservation::WorkEnded { work: "claude-task:agent".into(), continuation: "claude-notification:agent".into() },
        RuntimeObservation::TurnResumed { turn: "turn-2".into(), continuation: "claude-notification:agent".into() },
    ] });
    super::lifecycle_tests::woken(&mut s, "t", "turn-2", "turn-3");
    must(&mut s, Actor::System, Command::RecordTurnBoundary { task_id: "t".into(), generation: 1, turn: "turn-3".into(), failure: None, failed: None });
    assert_eq!(s.interactions.keys().filter(|id| id.0.starts_with(TURN_ENDED_PREFIX)).map(|id| id.0.as_str()).collect::<Vec<_>>(),
        [format!("{TURN_ENDED_PREFIX}t-1-turn-3")]);
    assert_eq!(s.tasks[&TaskId::from("t")].state, TaskState::NeedsYou, "the task waits on the user, not on a turn that never comes");
    assert_aggregate_rules(&s);
}

/// The node delivered its result while a background subagent still ran, and its turn ended.
fn delivered_with_background_work(s: &mut DomainState) {
    sdk_turn(s, "t", "a");
    must(s, Actor::System, Command::RecordRuntimeFacts { task_id: "t".into(), generation: 1, facts: vec![
        RuntimeObservation::WorkStarted { turn: "turn-1".into(), work: "claude-task:workflow".into() },
    ] });
    deliver(s, "t", "a", "result-7");
    must(s, Actor::System, Command::RecordTurnBoundary { task_id: "t".into(), generation: 1, turn: "turn-1".into(), failure: None, failed: None });
}
fn told_it_asked(s: &DomainState) -> bool {
    s.lifecycle_journal[&TaskId::from("t")].iter().any(|record| matches!(record,
        LifecycleRecord::HandedBack { text: Some(text), .. } if *text == delivery_withdrawn_for_questions()))
}
fn delivery_stands(s: &DomainState) {
    let t = &s.tasks[&TaskId::from("t")];
    assert_eq!(t.final_presentation, Some("result-7".into()), "the delivery is still the task's final one");
    assert!(!s.interactions[&InteractionId::from("result-7")].withdrawn);
    assert!(!told_it_asked(s), "the node is not told it asked");
}

// 23 Oct, t-ai-uiux-research: after its final delivery the node's background Workflow kept
// fetching, and the node was told «You asked the user something after your final delivery, so that
// delivery was withdrawn» though it had asked nothing. A permission prompt of that background work
// or an MCP server's elicitation is a decision about a tool call, not a question of the node.
#[test]
fn a_permission_or_elicitation_after_the_final_delivery_leaves_the_delivery_standing() {
    let mut s = world();
    delivered_with_background_work(&mut s);
    let permission = must(&mut s, Actor::System, bash(7, "gh api graphql -f query=@q.graphql"));
    assert_eq!(permission.created_ids, vec![card_of(7)], "the user still decides the permission");
    delivery_stands(&s);
    let elicitation = must(&mut s, Actor::System, request(8, Hold::Elicitation, None, json!({}), None, card_prompt(Hold::Elicitation, "Sign in to Atlassian")));
    assert_eq!(elicitation.created_ids, vec![card_of(8)]);
    delivery_stands(&s);
    must(&mut s, user(), answer(&card_of(7), Hold::ALLOW, None));
    delivery_stands(&s);
    assert_aggregate_rules(&s);
}

#[test]
fn a_question_claude_asks_after_the_final_delivery_still_withdraws_it() {
    let mut s = world();
    delivered_with_background_work(&mut s);
    must(&mut s, Actor::System, request(7, Hold::Question, Some("AskUserQuestion"), json!({}), None,
        question_prompt("Measure Android too?", &["Yes", "No"])));
    assert_eq!(s.tasks[&TaskId::from("t")].final_presentation, None);
    assert!(s.interactions[&InteractionId::from("result-7")].withdrawn);
    assert!(told_it_asked(&s), "the node hears why");
    assert_aggregate_rules(&s);
}

const WORKTREE: &str = "/repo/.claude/worktrees/t";

/// A command Claude Code runs from `cwd`, carded as the daemon cards it: «allow in this task»
/// offered for what stays in the task's worktree.
fn bash_in(epoch: u64, command: &str, cwd: &str) -> Command {
    let input = json!({"command": command});
    let prompt = crate::allow_for_task::Prompt { tool: Some("Bash"), input: &input, cwd: Some(cwd), persistent_refused: false, ask_rule: false };
    let offer = crate::allow_for_task::assess(&prompt, &[std::path::PathBuf::from(WORKTREE)]);
    let options = Hold::Permission.permission_offering(false, offer.offered());
    let mut card = CardSpec::new(CardKind::Authorize, format!("Run {command}"));
    card.allow_for_task = Some(offer);
    request(epoch, Hold::Permission, Some("Bash"), input, None, DecisionPrompt::Card { body: format!("Run {command}"), options, card })
}
fn settle(s: &mut DomainState, epoch: u64) {
    must(s, Actor::System, Command::RecordDecisionSettled { task_id: "t".into(), generation: 1, epoch });
}
/// Asks at `epoch`: the card the user is shown, or `None` when Alondra allowed it by itself.
fn ask(s: &mut DomainState, epoch: u64, command: &str, cwd: &str) -> Option<String> {
    let outcome = must(s, Actor::System, bash_in(epoch, command, cwd));
    if outcome.created_ids.is_empty() {
        assert_eq!(last(s, "t"), LifecycleRecord::DecidedByPolicy { epoch, decision: DecisionRecord::Allow }, "{command}");
        settle(s, epoch);
        return None;
    }
    Some(outcome.created_ids[0].clone())
}
fn decide(s: &mut DomainState, epoch: u64, card: &str, option: &str) {
    must(s, user(), answer(card, option, None));
    assert_eq!(last(s, "t"), LifecycleRecord::Decided { card: card.into(), epoch, decision: DecisionRecord::Allow });
    settle(s, epoch);
}
fn allowed(s: &DomainState) -> Vec<(String, u32)> {
    s.tasks[&TaskId::from("t")].allowed.iter().map(|(card, grant)| (card.0.clone(), grant.uses)).collect()
}

#[test]
fn a_deletion_allowed_in_the_task_runs_again_only_as_the_same_command_from_the_same_folder() {
    let mut s = world();
    sdk_turn(&mut s, "t", "a");
    let reset = "git reset --hard origin/main";
    let card = ask(&mut s, 1, reset, WORKTREE).expect("the first time it asks");
    let options: Vec<String> = match &s.interactions[&InteractionId::from(card.as_str())].payload {
        InteractionPayload::Question { options, .. } => options.iter().map(|option| option.id.clone()).collect(),
        _ => panic!("a question"),
    };
    assert_eq!(options, [Hold::ALLOW, Hold::FOR_TASK, Hold::DENY]);
    decide(&mut s, 1, &card, Hold::FOR_TASK);
    assert_eq!(allowed(&s), [(card.clone(), 0)]);
    assert_eq!(ask(&mut s, 2, reset, WORKTREE), None, "the same command from the same folder is allowed");
    assert_eq!(allowed(&s), [(card.clone(), 1)]);
    let other = ask(&mut s, 3, "git reset --hard HEAD~1", WORKTREE).expect("another target asks again");
    decide(&mut s, 3, &other, Hold::ALLOW);
    let elsewhere = ask(&mut s, 4, reset, &format!("{WORKTREE}/crates")).expect("the same command from another folder asks again");
    decide(&mut s, 4, &elsewhere, Hold::ALLOW);
    let branch = ask(&mut s, 5, "git branch -D scratch-a", WORKTREE).unwrap();
    decide(&mut s, 5, &branch, Hold::FOR_TASK);
    let next = ask(&mut s, 6, "git branch -D scratch-b", WORKTREE).expect("another branch asks again");
    decide(&mut s, 6, &next, Hold::ALLOW);
    let rm = ask(&mut s, 7, "rm -rf build/a", WORKTREE).unwrap();
    decide(&mut s, 7, &rm, Hold::FOR_TASK);
    let next = ask(&mut s, 8, "rm -rf build/b", WORKTREE).expect("a deletion never covers another");
    decide(&mut s, 8, &next, Hold::ALLOW);
    assert_eq!(allowed(&s).len(), 3);
    assert_aggregate_rules(&s);
}

#[test]
fn a_test_allowed_in_the_task_runs_again_with_other_arguments_but_not_another_subcommand() {
    let mut s = world();
    sdk_turn(&mut s, "t", "a");
    let card = ask(&mut s, 1, "bazelisk test //crates/core:alondra_core_test", WORKTREE).unwrap();
    decide(&mut s, 1, &card, Hold::FOR_TASK);
    assert_eq!(ask(&mut s, 2, "bazelisk test --test_output=errors //crates/alondrad:alondrad_test", WORKTREE), None);
    assert_eq!(ask(&mut s, 3, "bazelisk test //crates/...", &format!("{WORKTREE}/crates")), None, "anywhere in the worktree");
    let run = ask(&mut s, 4, "bazelisk run //tools:release", WORKTREE).expect("another subcommand asks again");
    decide(&mut s, 4, &run, Hold::ALLOW);
    let outside = ask(&mut s, 5, "bazelisk test //x", "/elsewhere").expect("outside the task's folders it asks again");
    decide(&mut s, 5, &outside, Hold::ALLOW);
    assert_eq!(allowed(&s), [(card, 2)]);
}

#[test]
fn a_command_outside_the_worktree_cannot_be_allowed_in_the_task() {
    let mut s = world();
    sdk_turn(&mut s, "t", "a");
    let card = ask(&mut s, 1, "rm -rf /Users/me/Downloads/old", WORKTREE).unwrap();
    let InteractionPayload::Question { options, .. } = &s.interactions[&InteractionId::from(card.as_str())].payload else { panic!("a question") };
    assert_eq!(options.iter().map(|option| option.id.as_str()).collect::<Vec<_>>(), [Hold::ALLOW, Hold::DENY]);
    assert_eq!(refused_unchanged(&mut s, user(), answer(&card, Hold::FOR_TASK, None)), "invalid_input");
}

#[test]
fn a_revoked_grant_asks_again_and_a_task_that_ends_forgets_what_it_allowed() {
    let mut s = world();
    sdk_turn(&mut s, "t", "a");
    let reset = "git reset --hard origin/main";
    let card = ask(&mut s, 1, reset, WORKTREE).unwrap();
    decide(&mut s, 1, &card, Hold::FOR_TASK);
    let revoke = Command::RevokeTaskGrant { task_id: "t".into(), card: card.as_str().into() };
    assert_eq!(refused_unchanged(&mut s, node("a"), revoke.clone()), "forbidden", "only the user takes it back");
    must(&mut s, user(), revoke.clone());
    assert!(allowed(&s).is_empty());
    assert_eq!(refused_unchanged(&mut s, user(), revoke), "not_found", "it is revoked once");
    let again = ask(&mut s, 2, reset, WORKTREE).expect("revoked, it asks again");
    decide(&mut s, 2, &again, Hold::FOR_TASK);
    assert_eq!(allowed(&s).len(), 1);
    must(&mut s, user(), Command::CancelTask { task_id: "t".into() });
    assert!(s.tasks[&TaskId::from("t")].over());
    assert!(allowed(&s).is_empty(), "nothing stays allowed once the task ends");
    assert_aggregate_rules(&s);
}


#[test]
fn a_consumed_native_plan_starts_its_new_turn_before_core_settles_the_decision() {
    for background in [false, true] {
        let mut s = world();
        sdk_turn(&mut s, "t", "a");
        if background {
            must(&mut s, Actor::System, Command::RecordRuntimeFacts { task_id: "t".into(), generation: 1,
                facts: vec![RuntimeObservation::WorkStarted { turn: "turn-1".into(), work: "native-child".into() }] });
        }
        must(&mut s, Actor::System, request(7, Hold::Plan, Some("ExitPlanMode"), json!({}), None, card_prompt(Hold::Plan, &plan(true))));
        must(&mut s, Actor::System, Command::RecordTurnBoundary { task_id: "t".into(), generation: 1, turn: "turn-1".into(), failure: None, failed: None });
        must(&mut s, user(), answer(&card_of(7), Hold::APPROVE, None));
        let before = s.lifecycle_of(&"t".into()).unwrap().cursor().unwrap();
        must(&mut s, Actor::System, Command::RecordRuntimeFacts { task_id: "t".into(), generation: 1,
            facts: vec![RuntimeObservation::TurnUnprompted { turn: "native-implementation".into() }] });
        must(&mut s, Actor::System, Command::RecordDecisionSettled { task_id: "t".into(), generation: 1, epoch: 7 });
        assert_eq!(s.lifecycle_of(&"t".into()).unwrap().cursor(), Some(before + 3), "two provider observations produce three real guarded lifecycle events");
        assert_eq!(kinds(&s, "t").iter().rev().take(3).cloned().collect::<Vec<_>>(), ["decision_continued", "decision_settled", "turn_unprompted"]);
        let wake = alondra_domain::decision_continuation(1, 7).as_str().to_owned();
        assert_eq!(refused_unchanged(&mut s, Actor::System, Command::RecordRuntimeFacts { task_id: "t".into(), generation: 1,
            facts: vec![RuntimeObservation::WorkContinued { continuation: wake }] }), "invalid_input", "generic work cannot consume a decision's continuation");
        assert!(s.lifecycle_of(&"t".into()).unwrap().pending_inputs().is_empty(), "the initial mission and approved decision leave no unconsumed input");
        assert_eq!(s.lifecycle_of(&"t".into()).unwrap().activity(), alondra_domain::ActivityView::Working);
        let targets = vec![DeliveryTarget::Text { text: "Parser and tests completed".into() }];
        must(&mut s, node("a"), Command::PresentResult {
            attachments: vec![], task_id: "t".into(), interaction_id: "native-delivery".into(), body: "Parser and tests completed".into(),
            checklist: vec![
                ChecklistMark { item: "1".into(), evidence: vec![Evidence { kind: EvidenceKind::File, value: "src/parser.rs".into(), detail: None }] },
                ChecklistMark { item: "2".into(), evidence: vec![Evidence { kind: EvidenceKind::Test, value: "parser_accepts_valid_input".into(), detail: None }] },
            ],
            card: CardSpec::delivery("Review the parser and tests", &targets, true), targets, is_final: true, nothing_to_review: false,
        });
        must(&mut s, Actor::System, Command::RecordTurnBoundary { task_id: "t".into(), generation: 1, turn: "native-implementation".into(), failure: None, failed: None });
        assert_eq!(s.lifecycle_of(&"t".into()).unwrap().activity(), if background { alondra_domain::ActivityView::OwnWork } else { alondra_domain::ActivityView::Resting });
        assert_eq!(s.lifecycle_journal[&TaskId::from("t")].iter().filter(|record| matches!(record, LifecycleRecord::DecisionContinued { epoch: 7 })).count(), 1);
        assert_aggregate_rules(&s);
    }
}
