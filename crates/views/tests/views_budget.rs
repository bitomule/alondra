//! Every family of views renders a domain the size of the real one within its budget, cold, right
//! after a commit: what a row needs is read from the committed index, never by walking the domain
//! once per row.
//!
//! This crate holds only what is measured and compiles from scratch at the build's opt-level on
//! every edit below it, so the domain it measures and every JSON live in `budget_fixture`.
use alondra_domain_rules::{
    Actor, CardKind, CardSpec, Command, CommandEnvelope, DeliveryTarget, DomainState, InteractionId, PROTOCOL_VERSION,
    ReadSnapshot, Recipient, Scope,
};
use alondra_views::{Client, Family, Lang, RenderContext, Transport, build};
use alondra_views_budget_fixture::{
    Seed, Shape, TOKEN, absorb, first_cards, id, live, loaded, mcp_card, real_size, serialized, sized, unit, user,
};
use std::collections::{BTreeMap, HashSet};
use std::sync::Arc;
use std::time::Instant;

mod views_budget_meter;
use views_budget_meter::{SAMPLES, median_minstr};

const VIEW_BUDGET_MINSTR: f64 = 750.0;

fn keys() -> Vec<(String, Client)> {
    let one = |kind: &str, shape: Shape| id(kind, shape).replace(TOKEN, "u001");
    [
        ("home".to_owned(), Client::Ios),
        ("home".to_owned(), Client::Tui),
        (format!("home?focus={}", one("d", Shape::Review)), Client::Ios),
        (format!("card:{}", one("d", Shape::Review)), Client::Ios),
        (format!("card:{}", one("q", Shape::Asking)), Client::Tui),
        (format!("card:{}", one("d", Shape::Done)), Client::Ios),
        (format!("card:{}", one("pr", Shape::Review)), Client::Mac),
        (format!("card:{}", mcp_card()), Client::Tui),
        (format!("task:{}", one("t", Shape::WaitingMcp)), Client::Tui),
        (format!("task:{}", one("t", Shape::SessionEnded)), Client::Tui),
        (format!("document:{}", one("rev", Shape::Document)), Client::Ios),
        (format!("pull_request:{}", one("pr", Shape::Review)), Client::Ios),
        (format!("pull_request:{}", one("pr", Shape::Done)), Client::Mac),
        ("sidebar".to_owned(), Client::Mac),
        ("tasks".to_owned(), Client::Ios),
        ("tasks".to_owned(), Client::Mac),
        ("tasks".to_owned(), Client::Tui),
        ("tasks?section=finished&after=0".to_owned(), Client::Ios),
        (format!("task:{}", one("t", Shape::Running)), Client::Ios),
        (format!("task:{}", one("t", Shape::Review)), Client::Tui),
        (format!("task:{}", one("t", Shape::Done)), Client::Mac),
        (format!("request:{}", one("r", Shape::Asking)), Client::Ios),
        ("orchestrator".to_owned(), Client::Tui),
        ("schedules".to_owned(), Client::Ios),
        ("schedule:schedule-1".to_owned(), Client::Ios),
        ("new_request".to_owned(), Client::Ios),
        ("model_profile:codex-orchestrator".to_owned(), Client::Mac),
        ("model_profile:claude-implementation".to_owned(), Client::Ios),
        (format!("task_selection:{}", one("t", Shape::Running)), Client::Tui),
        ("issue_picker".to_owned(), Client::Ios),
        ("search?q=node".to_owned(), Client::Ios),
        ("machine".to_owned(), Client::Mac),
        ("mcp_servers".to_owned(), Client::Ios),
        ("mcp_servers".to_owned(), Client::Tui),
        ("accounts".to_owned(), Client::Tui),
        ("metrics?period=90d&compare=previous".to_owned(), Client::Ios),
        ("metrics?period=all&compare=release:0.3.29&project=alondra".to_owned(), Client::Mac),
    ]
    .into_iter()
    .collect()
}

#[test]
fn every_family_of_views_is_measured() {
    let measured: HashSet<Family> = keys().iter().map(|(key, _)| Family::of(key).expect("a key a view answers")).collect();
    let missing: Vec<_> = Family::ALL.iter().filter(|family| !measured.contains(family)).collect();
    assert!(missing.is_empty(), "views::build renders families the budget does not measure: {missing:?}");
}

#[test]
fn every_view_renders_a_real_size_domain_cold_within_the_budget() {
    let mut state = real_size();
    let envelope = CommandEnvelope {
        protocol_version: PROTOCOL_VERSION,
        data_epoch: state.data_epoch.clone(),
        command_id: "budget-commit".into(),
        expected_revision: state.revision,
        command: Command::RegisterAgent { agent_id: "budget-agent".into(), name: "Budget".into(), at: 2 },
    };
    state.execute(&Actor::System, &envelope).expect("one more commit");
    let start = Instant::now();
    let committed = ReadSnapshot::build(Arc::new(state));
    let index_ms = start.elapsed().as_secs_f64() * 1000.0;
    let (state, index) = (committed.state(), committed.index());
    let live = live(state);
    println!("{} tasks, {} nodes, {} requests, {} cards, {} pull requests, {} waiting, index {index_ms:.2} ms",
        state.tasks.len(), state.agents.len(), state.requests.len(), state.interactions.len(), state.pull_requests.len(), index.waiting().len());
    let mut over = vec![];
    for (key, client) in keys() {
        let spent = median_minstr(|| build(&key, RenderContext {
            domain: state, index, live: &live, client, lang: Lang::Es, transport: Transport::Loopback,
            cursor: "budget", rendered_at: "",
        }).unwrap_or_else(|error| panic!("{key} ({client:?}) does not render: {error:?}")));
        println!("view {key} ({client:?}): {spent:?}M instructions");
        if let Some(spent) = spent.filter(|spent| *spent > VIEW_BUDGET_MINSTR) {
            over.push(format!("{key} ({client:?}): {spent:.0}M"));
        }
    }
    assert!(over.is_empty(), "views over {VIEW_BUDGET_MINSTR}M instructions: {over:#?}");
}

/// The live domain on 6 November had ~1,000 requests, ~1,500 tasks and ~6,300 cards.
const LIVE_SCALE: usize = 5;
/// Instructions a user's action may cost the domain on a domain the size of the live one: deciding
/// its command on a copy of the state, building the read index every screen reads after it, and
/// letting the state it replaced go. Each of those grows with the domain once, never with its
/// square; SQLite's part does not grow with it. An action takes about 620M: the budget leaves 60%
/// for the domain to grow.
const ACTION_BUDGET_MINSTR: f64 = 1_000.0;

fn envelope_on(state: &DomainState, id: &str, command: Command) -> CommandEnvelope {
    CommandEnvelope {
        protocol_version: PROTOCOL_VERSION, data_epoch: state.data_epoch.clone(), command_id: id.into(),
        expected_revision: state.revision, command,
    }
}

/// One command per kind of action a screen offers on a task or a card.
fn actions(state: &mut DomainState) -> Vec<(&'static str, Actor, Command)> {
    let unread: InteractionId = "budget-unread".into();
    let notice = Command::PublishInformation {
        attachments: vec![], interaction_id: unread.clone(), scope: Scope::Task { task_id: "t-running-u000".into() },
        recipient: Recipient::User, body: "Synthetic news".into(), card: CardSpec::derived(CardKind::Know, "Read the synthetic news"),
    };
    let envelope = envelope_on(state, "budget-notice", notice);
    state.execute(&Actor::System, &envelope).expect("the notice is published");
    let review = vec![DeliveryTarget::PullRequest { pr_id: "pr-review-u000".into(), head: "head-1".into() }];
    vec![
        ("read", user(), Command::MarkRead { interaction_id: unread }),
        ("answer", user(), Command::AnswerQuestion {
            attachments: vec![], interaction_id: "q-asking-u000".into(), option_id: "yes".into(), inputs: BTreeMap::new(),
        }),
        ("accept", user(), Command::AcceptDelivery { presentation_id: "d-review-u000".into(), targets: review.clone() }),
        ("request changes", user(), Command::SubmitFeedback {
            attachments: vec![], presentation_id: "d-review-u000".into(), targets: review, body: "Tighten the copy".into(),
            instruction: Default::default(), merge_failure: None,
        }),
        ("message", Actor::System, Command::TellNode { task_id: "t-running-u000".into(), text: "Also check the tests".into(), from_user: true, answers: None }),
        ("priority", user(), Command::SetPriority { task_id: "t-running-u000".into(), priority: alondra_domain_rules::TaskPriority::High }),
        ("cancel", user(), Command::CancelTask { task_id: "t-running-u001".into(), reason: None }),
        ("reopen", user(), Command::ReopenTask { task_id: "t-done-u000".into() }),
    ]
}

#[test]
fn every_action_of_the_user_fits_its_budget_on_a_live_size_domain() {
    let mut state = sized(LIVE_SCALE);
    assert!(state.requests.len() >= 1000 && state.tasks.len() >= 1400 && state.interactions.len() >= 6000,
        "the domain is smaller than the live one: {} requests, {} tasks, {} cards", state.requests.len(), state.tasks.len(), state.interactions.len());
    println!("copy of the state: {:?}M instructions, copy and read index: {:?}M",
        median_minstr(|| state.clone()), median_minstr(|| ReadSnapshot::build(Arc::new(state.clone()))));
    state.seed_every_open_pr_mergeable();
    let mut over = vec![];
    for (name, actor, command) in actions(&mut state) {
        let envelope = envelope_on(&state, &format!("budget-{name}"), command);
        let spent = median_minstr(|| {
            let (next, outcome) = state.transition(&actor, &envelope).unwrap_or_else(|error| panic!("{name}: {error:?}"));
            assert!(outcome.changed, "{name} changed nothing");
            drop(std::hint::black_box(ReadSnapshot::build(Arc::new(next))));
        });
        println!("{name}: {spent:?}M instructions");
        if let Some(spent) = spent.filter(|spent| *spent > ACTION_BUDGET_MINSTR) {
            over.push(format!("{name}: {spent:.0}M"));
        }
    }
    assert!(over.is_empty(), "actions over {ACTION_BUDGET_MINSTR}M instructions on {} tasks and {} cards: {over:#?}", state.tasks.len(), state.interactions.len());
}

/// What each app waits for before it can draw its first card, right after a commit: the iPhone's
/// `home` and the Mac's `sidebar`, then the first card they list unless it came inside them.
#[test]
fn the_first_card_comes_with_the_first_answer() {
    let mut state = real_size();
    let envelope = CommandEnvelope {
        protocol_version: PROTOCOL_VERSION,
        data_epoch: state.data_epoch.clone(),
        command_id: "first-card-commit".into(),
        expected_revision: state.revision,
        command: Command::RegisterAgent { agent_id: "first-card-agent".into(), name: "First card".into(), at: 2 },
    };
    state.execute(&Actor::System, &envelope).expect("one more commit");
    let committed = ReadSnapshot::build(Arc::new(state));
    let (state, index) = (committed.state(), committed.index());
    let live = live(state);
    let render = |key: &str, client: Client| {
        let mut samples: Vec<(f64, Vec<u8>)> = (0..SAMPLES).map(|_| {
            let start = Instant::now();
            let view = build(key, RenderContext {
                domain: state, index, live: &live, client, lang: Lang::Es, transport: Transport::Loopback,
                cursor: "budget", rendered_at: "",
            }).unwrap_or_else(|error| panic!("{key} ({client:?}) does not render: {error:?}"));
            let bytes = serialized(&view);
            (start.elapsed().as_secs_f64() * 1000.0, bytes)
        }).collect();
        samples.sort_by(|a, b| a.0.total_cmp(&b.0));
        samples.swap_remove(SAMPLES / 2)
    };
    for (root, client) in [("home", Client::Ios), ("sidebar", Client::Mac)] {
        let (root_ms, root_view) = render(root, client);
        let cards = first_cards(&root_view, client);
        let first = cards.first.expect("cards wait for the user");
        let (card_ms, card_view) = render(&first, client);
        let trips = if cards.embedded.first() == Some(&first) { 1 } else { 2 };
        println!("{client:?}: {root} {root_ms:.3} ms and {} bytes with {} cards inside, {first} {card_ms:.3} ms and {} bytes: {trips} trips, {:.3} ms of daemon to the first card",
            root_view.len(), cards.embedded.len(), card_view.len(), if trips == 1 { root_ms } else { root_ms + card_ms });
        assert_eq!(trips, 1, "{root} ({client:?}) brings its first card: {:?}", cards.embedded);
        assert_eq!(cards.embedded.len(), 3, "{root} ({client:?}) brings the front card and the two behind it");
    }
}

/// The number a unit whose task keeps a long journal carries.
const LONG: usize = 900;

/// The views of a domain with a task whose journal is as long as the longest real one read every
/// lifecycle as the domain keeps it: building the index and rendering every view applies no
/// event, however long a journal is.
#[test]
fn reading_every_view_replays_no_journal() {
    const TOLD: usize = 2000;
    let mut long = Seed(unit(Shape::WaitingMcp));
    let task = id("t", Shape::WaitingMcp);
    for notice in 0..TOLD {
        long.run(Actor::System, Command::TellNode { task_id: task.as_str().into(), text: format!("Synthetic notice {notice}"), from_user: false, answers: None });
    }
    let mut state = real_size();
    absorb(&mut state, &long.0, LONG);
    state.revision = state.revision.max(state.interactions.values().map(|i| i.order).max().unwrap_or(0)) + 1;
    let mut state = loaded(state);
    let long_task = id("t", Shape::WaitingMcp).replace(TOKEN, &format!("u{LONG:03}"));
    let envelope = CommandEnvelope {
        protocol_version: PROTOCOL_VERSION,
        data_epoch: state.data_epoch.clone(),
        command_id: "long-journal-commit".into(),
        expected_revision: state.revision,
        command: Command::TellNode { task_id: long_task.as_str().into(), text: "One more notice".into(), from_user: false, answers: None },
    };
    state.execute(&Actor::System, &envelope).expect("one more commit");
    let journal = state.lifecycle_journal[&long_task.as_str().into()].len();
    let state = Arc::new(state);
    let live = live(&state);
    let mut keys = keys();
    keys.extend([
        (format!("task:{long_task}"), Client::Ios),
        (format!("task:{long_task}"), Client::Tui),
    ]);
    let read = || {
        let committed = ReadSnapshot::build(Arc::clone(&state));
        for (key, client) in &keys {
            build(key, RenderContext {
                domain: committed.state(), index: committed.index(), live: &live, client: *client, lang: Lang::Es,
                transport: Transport::Loopback, cursor: "budget", rendered_at: "",
            }).unwrap_or_else(|error| panic!("{key} ({client:?}) does not render: {error:?}"));
        }
    };
    let before = alondra_domain::measure::applied();
    read();
    let applied = alondra_domain::measure::applied() - before;
    let mut samples: Vec<f64> = (0..SAMPLES).map(|_| {
        let start = Instant::now();
        read();
        start.elapsed().as_secs_f64() * 1000.0
    }).collect();
    samples.sort_by(f64::total_cmp);
    println!("journal of {journal} records and {} views: {applied} events applied, median {:.3} ms, max {:.3} ms",
        keys.len(), samples[SAMPLES / 2], samples[SAMPLES - 1]);
    assert_eq!(applied, 0, "reading the index and {} views over a journal of {journal} records applied {applied} events", keys.len());
}

/// What a task's view says of a crash report: the failure its button sends the report as, if it
/// offers one.
fn crash_report_failure(state: DomainState, shape: Shape, client: Client) -> Option<String> {
    let committed = ReadSnapshot::build(Arc::new(state));
    let (state, index) = (committed.state(), committed.index());
    let live = live(state);
    let view = build(&format!("task:{}", id("t", shape)), RenderContext {
        domain: state, index, live: &live, client, lang: Lang::En, transport: Transport::Loopback, cursor: "budget", rendered_at: "",
    }).expect("the task renders");
    let text = String::from_utf8(serialized(&view)).expect("JSON is UTF-8");
    let at = text.find(r#""local":{"kind":"crash_report","failure":""#)?;
    let rest = &text[at + r#""local":{"kind":"crash_report","failure":""#.len()..];
    Some(rest[..rest.find('"')?].to_owned())
}

#[test]
fn the_terminal_offers_a_crash_report_only_on_a_task_whose_session_ended_on_its_own() {
    assert_eq!(crash_report_failure(unit(Shape::SessionEnded), Shape::SessionEnded, Client::Tui).as_deref(), Some("session-ended"));
    for client in [Client::Ios, Client::Mac] {
        assert_eq!(crash_report_failure(unit(Shape::SessionEnded), Shape::SessionEnded, client), None, "{client:?} has no preview yet");
    }
    for shape in [Shape::WaitingMcp, Shape::Running, Shape::Done] {
        assert_eq!(crash_report_failure(unit(shape), shape, Client::Tui), None, "{shape:?}");
    }
}
