//! Where a view's time goes on a copy of a real domain: `ALONDRA_VIEWS_MEASURE_DB=<copy of
//! domain.db>` and run this ignored test from the Bazel-built `alondrad_test`.
use super::routes::respond;
use alondra_core::views::{self, Client, Lang, LiveSnapshot, RenderContext, Transport};
use std::time::Instant;

fn ms(start: Instant) -> f64 {
    start.elapsed().as_secs_f64() * 1000.0
}

fn cpu_ms() -> f64 {
    let mut usage: libc::rusage = unsafe { std::mem::zeroed() };
    unsafe { libc::getrusage(libc::RUSAGE_SELF, &mut usage) };
    let tv = |t: libc::timeval| t.tv_sec as f64 * 1000.0 + t.tv_usec as f64 / 1000.0;
    tv(usage.ru_utime) + tv(usage.ru_stime)
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "measures a copy of a real domain named by ALONDRA_VIEWS_MEASURE_DB"]
async fn measure_real_domain_views() {
    let source = std::env::var("ALONDRA_VIEWS_MEASURE_DB").expect("ALONDRA_VIEWS_MEASURE_DB");
    let home = crate::test_support::Directory::new();
    std::fs::create_dir_all(&home).unwrap();
    std::fs::copy(&source, home.join("domain.db")).unwrap();
    std::fs::set_permissions(home.join("domain.db"), std::os::unix::fs::PermissionsExt::from_mode(0o600)).unwrap();
    let opened = Instant::now();
    let daemon = crate::domain_bootstrap::DomainDaemon::open(&home).unwrap();
    println!("open daemon: {:.1} ms", ms(opened));
    let client = daemon.client();
    let state = client.domain.state().unwrap();
    println!("domain: {} tasks, {} agents, {} requests, {} interactions, {} PRs",
        state.tasks.len(), state.agents.len(), state.requests.len(), state.interactions.len(), state.pull_requests.len());

    let at = Instant::now();
    let copy = (*state).clone();
    println!("DomainState clone: {:.2} ms", ms(at));
    drop(copy);

    if let Some(task) = state.tasks.keys().next() {
        let at = Instant::now();
        state.task_actions(task);
        println!("task_actions: {:.1} ms", ms(at));
    }
    let at = Instant::now();
    let orders = state.inbox_orders();
    println!("inbox_orders: {:.1} ms ({} rows)", ms(at), orders.len());
    let at = Instant::now();
    for task in state.tasks.values() {
        std::hint::black_box((state.attention(task), state.open_blockers(&task.id)));
    }
    println!("attention and blockers of every task on demand: {:.1} ms", ms(at));
    let mut builds: Vec<f64> = (0..20).map(|_| {
        let at = Instant::now();
        std::hint::black_box(alondra_core::domain::ReadIndex::build(&state));
        ms(at)
    }).collect();
    builds.sort_by(f64::total_cmp);
    let index = alondra_core::domain::ReadIndex::build(&state);
    println!("ReadIndex::build: median {:.1} ms, max {:.1} ms ({} queued, {} waiting)",
        builds[builds.len() / 2], builds[builds.len() - 1], index.queue().len(), index.waiting().len());
    let at = Instant::now();
    let read = client.domain.read();
    std::hint::black_box((read.index().waiting(), read.index().counts(), read.index().acting()));
    println!("Waiting for You, counts and acting cards from the committed index: {:.3} ms", ms(at));
    let envelope = alondra_core::domain::CommandEnvelope {
        protocol_version: alondra_core::domain::PROTOCOL_VERSION,
        data_epoch: state.data_epoch.clone(),
        command_id: "measure-commit".into(),
        expected_revision: state.revision,
        command: alondra_core::domain::Command::RegisterAgent { agent_id: "measure-agent".into(), name: "Measure".into(), at: 0 },
    };
    let at = Instant::now();
    client.domain.execute(&alondra_core::domain::Actor::System, &envelope, None, || Ok(())).unwrap();
    println!("commit one command, index included: {:.1} ms", ms(at));
    let read = client.domain.read();
    let state = std::sync::Arc::clone(read.state());

    let at = Instant::now();
    let live = super::live::gather(&client, &state, Lang::Es, &["home".to_owned()], true, false).await;
    println!("live gather (home): {:.1} ms", ms(at));
    let at = Instant::now();
    let _ = serde_json::to_vec(&live).unwrap();
    println!("live serialize: {:.2} ms", ms(at));

    let at = Instant::now();
    let built = views::build("home", RenderContext {
        domain: &state, index: read.index(), live: &live, client: Client::Ios, lang: Lang::Es,
        transport: Transport::Loopback, cursor: "", rendered_at: "",
    }).unwrap();
    println!("views::build home (index given): {:.1} ms", ms(at));
    let mut card_ids: Vec<String> = built.sections.iter().flat_map(|s| s.card_ids.clone()).collect();
    let inbox: Vec<_> = read.index().queued(&state).collect();
    card_ids.extend(inbox.iter().rev().filter(|item| item.kind == "delivery" && !item.pending).take(5).map(|item| {
        let alondra_core::domain::InboxSubject::Interaction { interaction_id } = &item.subject;
        interaction_id.0.clone()
    }));

    let mut keys: Vec<(String, &str)> = vec![("home".into(), "ios"), ("home".into(), "tui"), ("sidebar".into(), "mac"),
        ("tasks".into(), "ios"), ("fleet".into(), "ios")];
    keys.extend(state.tasks.values().find(|task| !task.state.terminal()).map(|task| (format!("task:{}", task.id.0), "ios")));
    keys.extend(state.agents.keys().next().map(|agent| (format!("node:{}", agent.0), "ios")));
    for (key, client_kind) in &keys {
        for round in ["cold", "warm"] {
            let cpu = cpu_ms();
            let at = Instant::now();
            let response = respond(&client, key.as_str(), &format!("protocol=1&client={client_kind}&lang=es"), Transport::Loopback, None).await;
            println!("GET /views/{key} ({client_kind}) {round}: {:.1} ms, cpu {:.1} ms, status {}", ms(at), cpu_ms() - cpu, response.status());
        }
    }

    let empty = LiveSnapshot::default();
    let mut card_times = vec![];
    let cpu = cpu_ms();
    for id in &card_ids {
        let key = format!("card:{id}");
        let at = Instant::now();
        let response = respond(&client, &key, "protocol=1&client=ios&lang=es", Transport::Loopback, None).await;
        let total = ms(at);
        let at = Instant::now();
        views::build(&key, RenderContext {
            domain: &state, index: read.index(), live: &empty, client: Client::Ios, lang: Lang::Es,
            transport: Transport::Loopback, cursor: "", rendered_at: "",
        }).unwrap();
        card_times.push((total, ms(at), id.clone(), response.status()));
    }
    let cpu = cpu_ms() - cpu;
    card_times.sort_by(|a, b| b.0.total_cmp(&a.0));
    for (total, build, id, status) in &card_times {
        println!("GET /views/card:{id}: {total:.1} ms (views::build alone {build:.1} ms) {status}");
    }
    if !card_times.is_empty() {
        let sum: f64 = card_times.iter().map(|t| t.0).sum();
        println!("cards: {} views, {:.1} ms total, cpu {:.1} ms", card_times.len(), sum, cpu);
    }
    drop(client);
    drop(daemon);
}

fn median(mut samples: Vec<(f64, f64)>) -> (f64, f64) {
    samples.sort_by(|a, b| a.0.total_cmp(&b.0));
    samples[samples.len() / 2]
}

/// What one domain change costs the readers that run in the background: the badge and the
/// retraction, the push gate and payload, the Mac's notices, the changes feed of an iPhone (home
/// and three cards), a Mac (sidebar) and a terminal (home), and a `known` view that did not change.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "measures a copy of a real domain named by ALONDRA_VIEWS_MEASURE_DB"]
async fn measure_background_per_change() {
    const ROUNDS: usize = 9;
    let source = std::env::var("ALONDRA_VIEWS_MEASURE_DB").expect("ALONDRA_VIEWS_MEASURE_DB");
    let home = crate::test_support::Directory::new();
    std::fs::create_dir_all(&home).unwrap();
    std::fs::copy(&source, home.join("domain.db")).unwrap();
    std::fs::set_permissions(home.join("domain.db"), std::os::unix::fs::PermissionsExt::from_mode(0o600)).unwrap();
    let daemon = crate::domain_bootstrap::DomainDaemon::open(&home).unwrap();
    let client = daemon.client();
    let state = client.domain.state().unwrap();
    let index = alondra_core::domain::ReadIndex::build(&state);
    let inbox: Vec<_> = index.queued(&state).map(std::borrow::Cow::into_owned).collect();
    let id_of = |item: &alondra_core::domain::InboxItem| {
        let alondra_core::domain::InboxSubject::Interaction { interaction_id } = &item.subject;
        interaction_id.0.clone()
    };
    let mut cards: Vec<String> = inbox.iter().filter(|item| item.pending).map(id_of).collect();
    cards.extend(inbox.iter().rev().filter(|item| !item.pending && item.kind != "information").map(id_of));
    cards.truncate(3);
    println!("domain: {} tasks, {} agents, {} interactions, {} waiting; cards measured: {cards:?}",
        state.tasks.len(), state.agents.len(), state.interactions.len(), inbox.iter().filter(|item| item.pending).count());
    let unknown = "v0000000000000000";
    let phone_views = std::iter::once("home".to_owned()).chain(cards.iter().map(|id| format!("card:{id}")))
        .map(|key| format!("{key}@{unknown}")).collect::<Vec<_>>().join(",");
    let feeds = [
        ("phone", format!("protocol=1&client=ios&lang=es&wait=0&views={phone_views}")),
        ("mac", format!("protocol=1&client=mac&lang=es&wait=0&views=sidebar@{unknown}")),
        ("tui", format!("protocol=1&client=tui&lang=es&wait=0&views=home@{unknown}")),
    ];
    let card = cards.first().cloned();
    let mut samples: std::collections::BTreeMap<&str, Vec<(f64, f64)>> = Default::default();
    let mut measure = |name: &'static str, cpu: f64, at: Instant| samples.entry(name).or_default().push((ms(at), cpu_ms() - cpu));
    for round in 0..ROUNDS {
        let state = client.domain.state().unwrap();
        let envelope = alondra_core::domain::CommandEnvelope {
            protocol_version: alondra_core::domain::PROTOCOL_VERSION,
            data_epoch: state.data_epoch.clone(),
            command_id: format!("measure-change-{round}").into(),
            expected_revision: state.revision,
            command: alondra_core::domain::Command::RegisterAgent {
                agent_id: format!("measure-agent-{round}").into(), name: "Measure".into(), at: 0,
            },
        };
        client.domain.execute(&alondra_core::domain::Actor::System, &envelope, None, || Ok(())).unwrap();
        // The live sampler answers the commit on its own thread, and the readers this measures used
        // to walk the whole domain: settle and walk it once, untimed, so each reader starts alike.
        tokio::time::sleep(std::time::Duration::from_millis(300)).await;
        std::hint::black_box(alondra_core::domain::ReadIndex::build(&client.domain.state().unwrap()));

        let (cpu, at) = (cpu_ms(), Instant::now());
        let read = client.domain.read();
        std::hint::black_box((views::waiting_cards(read.index()), views::inbox_badge(read.index())));
        measure("badge and retraction", cpu, at);

        if let Some(card) = card.as_deref() {
            let (cpu, at) = (cpu_ms(), Instant::now());
            let read = client.domain.read();
            std::hint::black_box(crate::push_gate::check(&read, None, Some(card), std::time::Duration::ZERO).await);
            measure("push gate (one push)", cpu, at);
            let (cpu, at) = (cpu_ms(), Instant::now());
            let read = client.domain.read();
            std::hint::black_box(crate::domain_delivery::user_push_payload(&read, Some(card), Lang::Es));
            measure("push payload (one phone)", cpu, at);
        }

        let (cpu, at) = (cpu_ms(), Instant::now());
        std::hint::black_box(crate::notices::notices(&client.domain, Lang::Es).unwrap());
        measure("Mac notices", cpu, at);

        let (cpu, at) = (cpu_ms(), Instant::now());
        let state = client.domain.state().unwrap();
        let keys = feeds.iter().flat_map(|(_, query)| query.split("views=").nth(1).unwrap().split(','))
            .map(|view| view.rsplit_once('@').unwrap().0.to_owned()).collect::<Vec<_>>();
        std::hint::black_box(super::live::gather(&client, &state, Lang::Es, &keys, false, false).await);
        measure("live::gather, first read after the change (out of scope)", cpu, at);

        let (cpu, at) = (cpu_ms(), Instant::now());
        for (device, query) in &feeds {
            let response = super::feed::handle(&client, device, query, Transport::Loopback).await;
            assert_eq!(response.status(), axum::http::StatusCode::OK);
        }
        measure("changes feed (phone, Mac and terminal)", cpu, at);

        let (cpu, at) = (cpu_ms(), Instant::now());
        let second_phone = feeds[0].1.replace(unknown, "v1111111111111111");
        let response = super::feed::handle(&client, "second-phone", &second_phone, Transport::Loopback).await;
        assert_eq!(response.status(), axum::http::StatusCode::OK);
        measure("changes feed of a second phone on the same views", cpu, at);

        let first = respond(&client, "home", "protocol=1&client=ios&lang=es", Transport::Loopback, None).await;
        let body = axum::body::to_bytes(first.into_body(), usize::MAX).await.unwrap();
        let version = serde_json::from_slice::<serde_json::Value>(&body).unwrap()["version"].as_str().unwrap().to_owned();
        let (cpu, at) = (cpu_ms(), Instant::now());
        let known = respond(&client, "home", &format!("protocol=1&client=ios&lang=es&known={version}"), Transport::Loopback, None).await;
        measure("home with known version", cpu, at);
        let body = axum::body::to_bytes(known.into_body(), usize::MAX).await.unwrap();
        assert_eq!(serde_json::from_slice::<serde_json::Value>(&body).unwrap()["unchanged"], true);
    }
    for (name, values) in samples {
        let (wall, cpu) = median(values);
        println!("{name}: median {wall:.2} ms, cpu {cpu:.2} ms");
    }
    drop(client);
    drop(daemon);
}
