//! The task list moves only when a task's state moves: what a node does inside its turn reaches its
//! task's detail, never the list nor the changes feed of a client that has only the list open.
use alondra_daemon_engine::sdk_owner::Driver;
use alondra_daemon_engine::sdk_owner::tests::*;
use alondra_core::domain::*;
use alondra_core::views::{Client, Lang, Transport, VIEWS_PROTOCOL};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::sync::Arc;

const CLIENTS: [Client; 3] = [Client::Ios, Client::Mac, Client::Tui];
const LANGS: [Lang; 2] = [Lang::En, Lang::Es];
const TRANSPORTS: [Transport; 2] = [Transport::Loopback, Transport::Relay];

fn lists(client: Client) -> [&'static str; 2] {
    match client {
        Client::Mac => ["tasks", "sidebar"],
        _ => ["tasks", "home"],
    }
}

fn detail() -> String {
    format!("task:{TASK}")
}

fn client_name(client: Client) -> &'static str {
    match client {
        Client::Ios => "ios",
        Client::Mac => "mac",
        Client::Tui => "tui",
    }
}

fn lang_name(lang: Lang) -> &'static str {
    match lang {
        Lang::En => "en",
        Lang::Es => "es",
    }
}

/// A rendered view as a client receives it, without the cursor every live change rewrites.
#[derive(Clone, Debug, PartialEq)]
struct Seen {
    version: String,
    body: String,
}

struct Turn {
    slice: Slice,
    driver: Driver,
    client: Arc<crate::client_routes::Client>,
    runtime: tokio::runtime::Runtime,
    sampled: std::cell::RefCell<Option<String>>,
}

impl Turn {
    fn open() -> Self {
        let slice = Slice::new();
        slice.domain.enable_sdk(slice.backend.clone());
        let driver = slice.launch_working();
        let client = crate::client_routes::Client::open(Arc::clone(&slice.domain), &slice._home, Default::default());
        Self { slice, driver, client, runtime: tokio::runtime::Runtime::new().unwrap(), sampled: Default::default() }
    }

    fn agent(&self) -> String {
        self.driver.agent().0.clone()
    }

    fn observe(&self, message: &Value, at: Millis) {
        self.slice.domain.live.mods.apply_sdk(&self.agent(), message, at);
    }

    /// `alondra report-wait` writes the file; the live sampler is what notices it.
    fn report_wait(&self, wait: &Value) {
        let lifecycle = Slice::lifecycle(&self.slice.domain);
        let path = self.slice.backend.state_root.join(lifecycle.execution().unwrap().instance().as_str()).join("wait.json");
        std::fs::write(path, serde_json::to_vec(wait).unwrap()).unwrap();
        let state = self.slice.domain.state().unwrap();
        self.runtime.block_on(crate::views::feed::sample_live(&self.client, &state, &mut self.sampled.borrow_mut()));
    }

    fn render(&self, key: &str, client: Client, lang: Lang, transport: Transport) -> Seen {
        let read = self.slice.domain.read();
        let live = self.runtime.block_on(crate::views::live::gather(&self.client, read.state(), lang, &[key.to_owned()], false, false));
        let mut view = self.client.views.render(key, &read, &live, client, lang, transport).unwrap();
        view.cursor.clear();
        Seen { version: view.version.clone(), body: serde_json::to_string(&view).unwrap() }
    }

    fn every_view(&self) -> BTreeMap<(String, String), Seen> {
        let mut seen = BTreeMap::new();
        for client in CLIENTS {
            for lang in LANGS {
                for transport in TRANSPORTS {
                    let at = format!("{}/{}/{transport:?}", client_name(client), lang_name(lang));
                    for key in lists(client).into_iter().map(str::to_owned).chain([detail()]) {
                        seen.insert((at.clone(), key.clone()), self.render(&key, client, lang, transport));
                    }
                }
            }
        }
        seen
    }

    /// What the changes feed answers a client that holds these views at these versions.
    fn feed(&self, client: Client, lang: Lang, transport: Transport, views: &[(String, String)]) -> Value {
        let views = views.iter().map(|(key, version)| format!("{key}@{version}")).collect::<Vec<_>>().join(",");
        let views: String = url::form_urlencoded::byte_serialize(views.as_bytes()).collect();
        let query = format!("protocol={VIEWS_PROTOCOL}&client={}&lang={}&wait=0&views={views}", client_name(client), lang_name(lang));
        self.runtime.block_on(async {
            let response = crate::views::feed::handle(&self.client, "list-device", &query, transport).await;
            let bytes = axum::body::to_bytes(axum::response::IntoResponse::into_response(response).into_body(), 1 << 20).await.unwrap();
            serde_json::from_slice(&bytes).unwrap()
        })
    }
}

fn assistant(content: Value) -> Value {
    json!({"type": "assistant", "parent_tool_use_id": null, "message": {"role": "assistant", "content": content}})
}

fn says(text: &str) -> Value {
    assistant(json!([{"type": "text", "text": text}]))
}

fn runs(id: &str, command: &str) -> Value {
    assistant(json!([{"type": "tool_use", "id": id, "name": "Bash", "input": {"command": command}}]))
}

fn ends(id: &str) -> Value {
    json!({"type": "user", "parent_tool_use_id": null, "message": {"role": "user", "content": [
        {"type": "tool_result", "tool_use_id": id, "content": "ok"}]}})
}

fn background(tasks: Value) -> Value {
    json!({"type": "system", "subtype": "background_tasks_changed", "tasks": tasks})
}

/// Eight minutes of an open turn, one event every twenty seconds: what it says, the commands it
/// runs, its background work and its wait report.
fn normal_turn() -> Vec<(Millis, Value)> {
    let start: Millis = 1_794_300_000_000;
    let events = vec![
        says("I read the parser first."),
        runs("t1", "rg parse crates"), ends("t1"),
        says("The parser drops empty lines. I am fixing that."),
        runs("t2", "cargo fmt"), ends("t2"),
        runs("t3", "bazel test //crates/core:alondra_core_test"),
        background(json!([{"task_id": "b1", "task_type": "local_bash", "description": "bazel test //crates/..."}])),
        ends("t3"),
        says("The build runs in the background. Meanwhile I write the test."),
        runs("t4", "git diff --stat"), ends("t4"),
        json!({"type": "wait"}),
        says("The test fails without the fix."),
        runs("t5", "git apply fix.patch"), ends("t5"),
        background(json!([])),
        says("The checks passed."),
        runs("t6", "git push"), ends("t6"),
        says("Opening the PR."),
        runs("t7", "gh pr create"), ends("t7"),
        says("The PR is open."),
    ];
    events.into_iter().enumerate().map(|(index, event)| (start + 20_000 * index as Millis, event)).collect()
}

fn play(turn: &Turn, at: Millis, event: &Value) {
    if event["type"] == "wait" {
        turn.report_wait(&json!({"what": "the review", "progress": "1 of 2", "detail": "Waiting for the bot", "since_ms": at}));
    } else {
        turn.observe(event, at);
    }
}

#[test]
fn an_open_turn_moves_its_task_detail_and_never_the_task_list() {
    let mut turn = Turn::open();
    turn.observe(&json!({"type": "system", "subtype": "session_state_changed", "state": "running"}), 1_794_299_990_000);
    let first = turn.every_view();
    let mut details = std::collections::BTreeSet::from([first[&("ios/en/Loopback".to_owned(), detail())].version.clone()]);
    let mut previous = first.clone();
    for (at, event) in normal_turn() {
        play(&turn, at, &event);
        let now = turn.every_view();
        for ((client, key), seen) in &now {
            if *key == detail() { continue; }
            assert_eq!(seen, &first[&(client.clone(), key.clone())], "{client} {key} changed after {event}");
        }
        let detail_now = &now[&("ios/en/Loopback".to_owned(), detail())];
        if event["type"] == "assistant" || event["type"] == "wait" {
            assert_ne!(detail_now, &previous[&("ios/en/Loopback".to_owned(), detail())], "the task's detail shows {event}");
        }
        details.insert(detail_now.version.clone());
        previous = now;
    }
    let es = &previous[&("ios/es/Loopback".to_owned(), detail())].body;
    assert!(es.contains("The PR is open."), "the detail has the node's last words: {es}");
    let en = &previous[&("mac/en/Loopback".to_owned(), detail())].body;
    assert!(en.contains("Last message"), "the detail says when it said them: {en}");
    assert!(details.len() > 10, "the detail moved with the turn: {} versions", details.len());
    for client in CLIENTS {
        for lang in LANGS {
            for transport in TRANSPORTS {
                let at = format!("{}/{}/{transport:?}", client_name(client), lang_name(lang));
                let held: Vec<_> = lists(client).into_iter().map(str::to_owned).chain([detail()])
                    .map(|key| (key.clone(), first[&(at.clone(), key)].version.clone())).collect();
                let reply = turn.feed(client, lang, transport, &held);
                assert_eq!(reply["stale"], json!([detail()]), "only the detail is pushed to {at}: {reply}");
                let lists_only: Vec<_> = held.iter().filter(|(key, _)| *key != detail()).cloned().collect();
                let reply = turn.feed(client, lang, transport, &lists_only);
                assert_eq!(reply["stale"], json!([]), "a client with only the list open gets nothing: {reply}");
            }
        }
    }
    turn.driver.stop().unwrap();
}

#[test]
fn a_state_change_and_the_reason_of_a_waiting_state_move_the_task_list() {
    let mut turn = Turn::open();
    let working = turn.render("tasks", Client::Ios, Lang::En, Transport::Loopback);
    let ask = |id: &str| Command::AskQuestion {
        attachments: vec![], interaction_id: id.into(), scope: Scope::Task { task_id: TASK.into() }, recipient: Recipient::User,
        body: format!("Question {id}"), blocking: id == "q-1", options: vec![], card: CardSpec::derived(CardKind::Provide, &format!("Answer {id}")),
    };
    turn.slice.run(&Actor::Agent { agent_id: turn.driver.agent().clone() }, "ask-1", ask("q-1"));
    let one = turn.render("tasks", Client::Ios, Lang::En, Transport::Loopback);
    assert_ne!(one.version, working.version, "a task that needs the user moves the list");
    assert!(one.body.contains("Needs you"), "{}", one.body);
    turn.slice.run(&Actor::Agent { agent_id: turn.driver.agent().clone() }, "ask-2", ask("q-2"));
    let two = turn.render("tasks", Client::Ios, Lang::En, Transport::Loopback);
    assert_ne!(two.version, one.version, "the reason of a state that is not working moves the list");
    assert!(two.body.contains("2 open questions"), "{}", two.body);
    turn.driver.stop().unwrap();
}

/// Replays a recorded turn (`ALONDRA_TURN_TRANSCRIPT=<file>`, one `{"at": <ms>, "event": <SDK message>}`
/// per line) and prints how often the list and the detail of its task change, and what changed the list.
#[test]
#[ignore = "measures a recorded turn named by ALONDRA_TURN_TRANSCRIPT"]
fn measure_list_revisions_of_a_recorded_turn() {
    let path = std::env::var("ALONDRA_TURN_TRANSCRIPT").expect("ALONDRA_TURN_TRANSCRIPT");
    let events: Vec<(Millis, Value)> = std::fs::read_to_string(path).unwrap().lines()
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .filter_map(|line| Some((line["at"].as_i64()?, line["event"].clone()))).collect();
    let mut turn = Turn::open();
    let (start, end) = (events.first().unwrap().0, events.last().unwrap().0);
    turn.observe(&json!({"type": "system", "subtype": "session_state_changed", "state": "running"}), start);
    let key = |turn: &Turn, key: &str| turn.render(key, Client::Ios, Lang::Es, Transport::Relay).version;
    let (mut list, mut task) = (key(&turn, "tasks"), key(&turn, &detail()));
    let (mut list_changes, mut detail_changes) = (BTreeMap::<String, usize>::new(), 0);
    for (at, event) in &events {
        turn.observe(event, *at);
        let (now_list, now_task) = (key(&turn, "tasks"), key(&turn, &detail()));
        if now_list != list {
            let content = event["message"]["content"].as_array().and_then(|blocks| blocks.first()).cloned().unwrap_or_default();
            let kind = format!("{} {}", event["type"].as_str().unwrap_or("?"), content["type"].as_str().unwrap_or("text"));
            *list_changes.entry(kind).or_default() += 1;
        }
        detail_changes += usize::from(now_task != task);
        (list, task) = (now_list, now_task);
    }
    let minutes = ((end - start) as f64 / 60_000.0).max(1.0 / 60.0);
    let listed: usize = list_changes.values().sum();
    println!("turn: {} events over {minutes:.1} min", events.len());
    println!("list revisions: {listed} ({:.2}/min) by event: {list_changes:?}", listed as f64 / minutes);
    println!("detail revisions: {detail_changes} ({:.2}/min)", detail_changes as f64 / minutes);
    turn.driver.stop().unwrap();
}
