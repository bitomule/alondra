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

    let at = Instant::now();
    let inbox = state.inbox_projection();
    println!("inbox_projection: {:.1} ms ({} rows, {} pending)", ms(at), inbox.len(),
        inbox.iter().filter(|item| item.pending).count());
    let deliveries = inbox.iter().filter(|item| item.kind == "delivery").count();
    let actions: usize = inbox.iter().map(|item| item.available_actions.len()).sum();
    println!("inbox rows: {deliveries} deliveries, {actions} actions");
    if let Some(task) = state.tasks.keys().next() {
        let at = Instant::now();
        state.task_actions(task);
        println!("task_actions: {:.1} ms", ms(at));
    }
    let at = Instant::now();
    let orders = state.inbox_orders();
    println!("inbox_orders: {:.1} ms ({} rows)", ms(at), orders.len());
    let at = Instant::now();
    let projection = state.projection();
    println!("projection: {:.1} ms", ms(at));

    let at = Instant::now();
    for task in state.tasks.values() {
        std::hint::black_box((state.attention(task), state.open_blockers(&task.id), projection.waits_for_user(&task.id)));
    }
    println!("attention, blockers and waits of every task on demand: {:.1} ms", ms(at));
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
