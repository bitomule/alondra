use alondra_core::domain::*;
use std::{alloc::{GlobalAlloc, Layout, System}, collections::BTreeSet, sync::atomic::{AtomicBool, AtomicUsize, Ordering}, time::Instant};
mod inbox_graphs;

struct Allocations;
static COUNTING: AtomicBool = AtomicBool::new(false);
static BYTES: AtomicUsize = AtomicUsize::new(0);
static CALLS: AtomicUsize = AtomicUsize::new(0);
static LIVE: AtomicUsize = AtomicUsize::new(0);
static PEAK: AtomicUsize = AtomicUsize::new(0);
fn allocated(size: usize) {
    if COUNTING.load(Ordering::Relaxed) {
        CALLS.fetch_add(1, Ordering::Relaxed);
        BYTES.fetch_add(size, Ordering::Relaxed);
        let live = LIVE.fetch_add(size, Ordering::Relaxed) + size;
        PEAK.fetch_max(live, Ordering::Relaxed);
    }
}
unsafe impl GlobalAlloc for Allocations {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let pointer = unsafe { System.alloc(layout) };
        if !pointer.is_null() { allocated(layout.size()); }
        pointer
    }
    unsafe fn dealloc(&self, pointer: *mut u8, layout: Layout) {
        if COUNTING.load(Ordering::Relaxed) { LIVE.fetch_sub(layout.size(), Ordering::Relaxed); }
        unsafe { System.dealloc(pointer, layout) }
    }
    unsafe fn realloc(&self, pointer: *mut u8, old: Layout, size: usize) -> *mut u8 {
        let result = unsafe { System.realloc(pointer, old, size) };
        if !result.is_null() && COUNTING.load(Ordering::Relaxed) {
            LIVE.fetch_sub(old.size(), Ordering::Relaxed);
            allocated(size);
        }
        result
    }
}
#[global_allocator] static ALLOCATOR: Allocations = Allocations;
fn commit(state: &mut DomainState, actor: Actor, command: Command) {
    let envelope = CommandEnvelope { protocol_version: PROTOCOL_VERSION,
        data_epoch: state.data_epoch.clone(), command_id: format!("seed-{}", state.revision).into(),
        expected_revision: state.revision, command };
    state.execute(&actor, &envelope).unwrap();
}
fn user() -> Actor { Actor::User { user_id: "fixture-user".into() } }
fn cpu_seconds() -> f64 {
    let mut usage = std::mem::MaybeUninit::<libc::rusage>::uninit();
    assert_eq!(unsafe { libc::getrusage(libc::RUSAGE_SELF, usage.as_mut_ptr()) }, 0);
    let usage = unsafe { usage.assume_init() };
    usage.ru_utime.tv_sec as f64 + usage.ru_stime.tv_sec as f64
        + (usage.ru_utime.tv_usec + usage.ru_stime.tv_usec) as f64 / 1_000_000.0
}
fn main() {
    if std::env::args().any(|argument| argument == "--inbox-graphs") { profile_inbox_graphs(); return; }
    if std::env::args().any(|argument| argument == "--full-inbox") { profile_full_inbox(); return; }
    if std::env::args().any(|argument| argument == "--inbox-orders") { profile_inbox_orders(); return; }
    let mut state = DomainState::new("synthetic-projection-epoch");
    commit(&mut state, Actor::System, Command::RegisterAgent { agent_id: "agent".into(), name: "Synthetic agent".into(), at: 0 });
    commit(&mut state, user(), Command::CreateRequest { request_id: "request".into(), body: "Synthetic request".into(), auto_merge: false, maintenance: false, message: false });
    commit(&mut state, Actor::Orchestrator { generation: 0 }, Command::CreateTask { task_id: "task".into(), request_id: "request".into(), title: "Synthetic task".into(), agent_id: Some("agent".into()), review_prs: true, dependencies: BTreeSet::new(), stack: None, launch: Some(alondra_core::domain::LaunchPlan::new("node", "/tmp")) });
    for index in 0..500 {
        commit(&mut state, Actor::System, Command::PublishInformation { interaction_id: format!("information-{index}").into(), scope: Scope::Task { task_id: "task".into() }, recipient: Recipient::User,
            body: format!("Synthetic item {index}. {}", "Synthetic measurement content. ".repeat(256)), attachments: vec![], card: CardSpec::derived(CardKind::Know, "Read synthetic update") });
    }
    assert_eq!(state.interactions.len(), 500, "the workload must not be coalesced to one information item");
    let body_bytes = state.interactions.values().map(|item| item.body.len()).sum::<usize>();
    let expected = state.task_actions(&"task".into()).unwrap();
    for _ in 0..3 { std::hint::black_box(state.task_actions(&"task".into()).unwrap()); }
    let mut samples = Vec::new();
    let mut errors = 0;
    for _ in 0..20 {
        BYTES.store(0, Ordering::Relaxed); CALLS.store(0, Ordering::Relaxed);
        LIVE.store(0, Ordering::Relaxed); PEAK.store(0, Ordering::Relaxed);
        let cpu = cpu_seconds(); let start = Instant::now();
        COUNTING.store(true, Ordering::Relaxed);
        let actual = state.task_actions(&"task".into()).unwrap();
        if actual != expected { errors += 1; }
        std::hint::black_box(actual);
        COUNTING.store(false, Ordering::Relaxed);
        let wall_ms = start.elapsed().as_secs_f64() * 1000.0;
        let cpu_ms = (cpu_seconds() - cpu) * 1000.0;
        samples.push(serde_json::json!({"wall_ms":wall_ms,"cpu_ms":cpu_ms,"allocated_bytes":BYTES.load(Ordering::Relaxed),"allocations":CALLS.load(Ordering::Relaxed),"peak_temporary_bytes":PEAK.load(Ordering::Relaxed),"remaining_temporary_bytes":LIVE.load(Ordering::Relaxed)}));
    }
    println!("{}", serde_json::json!({"scope":"task-action-projection","interactions":state.interactions.len(),"body_bytes":body_bytes,"samples":samples,"errors":errors,"actions":expected}));
    assert_eq!(errors, 0);
    if std::env::args().any(|argument| argument == "--check-cost") {
        for sample in &samples {
            assert!(sample["allocated_bytes"].as_u64().unwrap() < 20_000_000,
                "projecting three actions must not allocate over 20 MB for this 4 MB fixture");
        }
    }
}

fn inbox_orders(state: &DomainState) -> Vec<(String, Revision)> {
    state.inbox_orders()
}

fn inbox_fixture(count: usize, body_size: usize, history: usize) -> DomainState {
    let mut state = DomainState::new("synthetic-inbox-epoch");
    commit(&mut state, Actor::System, Command::RegisterAgent { agent_id: "agent".into(), name: "Synthetic agent".into(), at: 0 });
    commit(&mut state, user(), Command::CreateRequest { request_id: "request".into(), body: "Synthetic request".into(), auto_merge: false, maintenance: false, message: false });
    commit(&mut state, Actor::Orchestrator { generation: 0 }, Command::CreateTask {
        task_id: "task".into(), request_id: "request".into(), title: "Synthetic task".into(), agent_id: Some("agent".into()),
        review_prs: true, dependencies: BTreeSet::new(), stack: None, launch: Some(alondra_core::domain::LaunchPlan::new("node", "/synthetic")),
    });
    commit(&mut state, Actor::System, Command::PublishInformation {
        interaction_id: "template".into(), scope: Scope::Task { task_id: "task".into() }, recipient: Recipient::User,
        body: "Synthetic information".into(), attachments: vec![], card: CardSpec::derived(CardKind::Know, "Read synthetic update"),
    });
    let template = state.interactions.remove(&"template".into()).unwrap();
    for index in 0..count {
        let mut item = template.clone();
        item.id = format!("information-{index:05}").into();
        item.body = "x".repeat(body_size);
        item.order = state.revision + index as u64;
        item.revision = item.order;
        state.interactions.insert(item.id.clone(), item);
    }
    state.revision += count as u64;
    if history > 0 {
        commit(&mut state, Actor::Agent { agent_id: "agent".into() }, Command::CreateDocument {
            task_id: "task".into(), document_id: "document".into(), title: "Synthetic document".into(), revision_id: "document-template".into(),
            content_digest: "1".repeat(64), content_reference: "library-file:synthetic/document.txt".into(), role: "implementation".into(),
        });
        let template = state.document_revisions.remove(&"document-template".into()).unwrap();
        for index in 0..history {
            let mut revision = template.clone();
            revision.id = format!("document-revision-{index:05}").into();
            revision.number = index as u64 + 1;
            revision.content_digest = format!("{index:064x}");
            revision.content_reference = format!("library-file:synthetic/{}.txt", revision.content_digest);
            state.documents.get_mut(&"document".into()).unwrap().current_revision = revision.id.clone();
            state.document_revisions.insert(revision.id.clone(), revision);
        }
    }
    state.validate().unwrap();
    assert_eq!(state.interactions.len(), count);
    assert_eq!(state.document_revisions.len(), history);
    state
}

fn profile_inbox_orders() {
    for count in [100, 1_000, 10_000] {
        for (body_size, history) in [(64, 0), (8_192, count)] {
            let state = inbox_fixture(count, body_size, history);
            let expected = inbox_orders(&state);
            assert_eq!(expected.len(), count);
            assert_eq!(expected.iter().map(|(_, order)| *order).collect::<std::collections::BTreeSet<_>>().len(), count);
            let input_hash = {
                use sha2::{Digest, Sha256};
                format!("{:x}", Sha256::digest(serde_json::to_vec(&state).unwrap()))
            };
            std::hint::black_box(inbox_orders(&state));
            let mut samples = Vec::new();
            let mut errors = 0;
            for _ in 0..3 {
                BYTES.store(0, Ordering::Relaxed); CALLS.store(0, Ordering::Relaxed);
                LIVE.store(0, Ordering::Relaxed); PEAK.store(0, Ordering::Relaxed);
                let cpu = cpu_seconds(); let start = Instant::now();
                COUNTING.store(true, Ordering::Relaxed);
                let actual = inbox_orders(&state);
                if actual != expected { errors += 1; }
                std::hint::black_box(&actual);
                drop(actual);
                COUNTING.store(false, Ordering::Relaxed);
                samples.push(serde_json::json!({"wall_ms":start.elapsed().as_secs_f64()*1000.0,
                    "cpu_ms":(cpu_seconds()-cpu)*1000.0,"allocated_bytes":BYTES.load(Ordering::Relaxed),
                    "allocations":CALLS.load(Ordering::Relaxed),"peak_temporary_bytes":PEAK.load(Ordering::Relaxed),
                    "remaining_temporary_bytes":LIVE.load(Ordering::Relaxed)}));
            }
            let output_hash = {
                use sha2::{Digest, Sha256};
                format!("{:x}", Sha256::digest(serde_json::to_vec(&expected).unwrap()))
            };
            println!("{}", serde_json::json!({"scope":"inbox-orders-projection","interactions":count,
                "interaction_body_bytes":count*body_size,"document_revisions":history,
                "document_metadata_string_bytes":state.document_revisions.values().map(|r|r.id.0.len()+r.document_id.0.len()+r.content_digest.len()+r.content_reference.len()).sum::<usize>(),
                "input_sha256":input_hash,"samples":samples,"errors":errors,"outputs":expected.len(),
                "output_sha256":output_hash}));
            assert_eq!(errors, 0);
        }
    }
}

fn profile_full_inbox() {
    use sha2::{Digest, Sha256};
    let state = inbox_fixture(1_000, 8_192, 1_000);
    let expected = state.inbox_projection();
    assert_eq!(expected.len(), 1_000);
    let input_hash = format!("{:x}", Sha256::digest(serde_json::to_vec(&state).unwrap()));
    let output_hash = format!("{:x}", Sha256::digest(serde_json::to_vec(&expected).unwrap()));
    std::hint::black_box(state.inbox_projection());
    let mut samples = vec![];
    let mut errors = 0;
    for _ in 0..3 {
        BYTES.store(0, Ordering::Relaxed); CALLS.store(0, Ordering::Relaxed);
        LIVE.store(0, Ordering::Relaxed); PEAK.store(0, Ordering::Relaxed);
        let cpu = cpu_seconds(); let start = Instant::now();
        COUNTING.store(true, Ordering::Relaxed);
        let actual = state.inbox_projection();
        if actual != expected { errors += 1; }
        std::hint::black_box(&actual); drop(actual);
        COUNTING.store(false, Ordering::Relaxed);
        samples.push(serde_json::json!({"wall_ms":start.elapsed().as_secs_f64()*1000.0,
            "cpu_ms":(cpu_seconds()-cpu)*1000.0,"allocated_bytes":BYTES.load(Ordering::Relaxed),
            "allocations":CALLS.load(Ordering::Relaxed),"peak_temporary_bytes":PEAK.load(Ordering::Relaxed),
            "remaining_temporary_bytes":LIVE.load(Ordering::Relaxed)}));
    }
    println!("{}", serde_json::json!({"scope":"full-inbox-control", "interactions":1_000,
        "interaction_body_bytes":8_192_000,"document_revisions":1_000,"outputs":expected.len(),
        "input_sha256":input_hash,"output_sha256":output_hash,"errors":errors,"samples":samples}));
    assert_eq!(errors, 0);
}


fn measured_samples<T: PartialEq>(mut read: impl FnMut() -> T, expected: &T) -> (Vec<serde_json::Value>, usize) {
    std::hint::black_box(read());
    let mut samples = vec![];
    let mut errors = 0;
    for _ in 0..3 {
        BYTES.store(0, Ordering::Relaxed); CALLS.store(0, Ordering::Relaxed);
        LIVE.store(0, Ordering::Relaxed); PEAK.store(0, Ordering::Relaxed);
        let cpu = cpu_seconds(); let start = Instant::now();
        COUNTING.store(true, Ordering::Relaxed);
        let actual = read();
        if &actual != expected { errors += 1; }
        std::hint::black_box(&actual); drop(actual);
        COUNTING.store(false, Ordering::Relaxed);
        samples.push(serde_json::json!({"wall_ms": start.elapsed().as_secs_f64() * 1000.0,
            "cpu_ms": (cpu_seconds() - cpu) * 1000.0, "allocated_bytes": BYTES.load(Ordering::Relaxed),
            "allocations": CALLS.load(Ordering::Relaxed), "peak_temporary_bytes": PEAK.load(Ordering::Relaxed),
            "remaining_temporary_bytes": LIVE.load(Ordering::Relaxed)}));
    }
    assert_eq!(errors, 0);
    assert!(samples.iter().all(|sample| sample["remaining_temporary_bytes"] == 0));
    (samples, errors)
}

fn digest(value: &impl serde::Serialize) -> String {
    use sha2::{Digest, Sha256};
    format!("{:x}", Sha256::digest(serde_json::to_vec(value).unwrap()))
}

fn profile_inbox_graphs() {
    let selected = std::env::args().find_map(|argument| argument.strip_prefix("--graph-case=").map(str::to_owned));
    let mut executed = 0;
    for (shape, count, body_size, full) in [
        ("stack-chain", 64, 64, false), ("stack-chain", 128, 64, false),
        ("fanout", 100, 64, false), ("fanout", 1_000, 64, false),
        ("fanin", 100, 64, false), ("fanin", 1_000, 64, false),
        ("mixed", 5, 64, false), ("unblocked", 1_000, 64, false),
        ("fanin", 1_000, 8_192, true),
    ] {
        let case = format!("{shape}-{count}-{}", if full { "full" } else { "orders" });
        if selected.as_ref().is_some_and(|selected| selected != &case) { continue; }
        executed += 1;
        let fixture = inbox_graphs::fixture(shape, count, body_size);
        let graph = fixture.inspect();
        let input_hash = digest(&fixture.state);
        let expected_order = fixture.orders();
        let expected_blockers: Vec<_> = fixture.state.interactions.values()
            .filter(|item| matches!(item.payload, InteractionPayload::Delivery { .. }))
            .map(|item| (item.id.clone(), fixture.blockers[&item.id].clone())).collect();
        let lookup = if expected_blockers.is_empty() { None } else {
            let (samples, errors) = measured_samples(|| inbox_graphs::delivery_blockers(&fixture.state), &expected_blockers);
            Some(serde_json::json!({"scope": "delivery-blocker-lookup", "samples": samples, "errors": errors,
                "outputs": expected_blockers.len(), "output_sha256": digest(&expected_blockers)}))
        };
        let projection = if full {
            let expected = fixture.state.inbox_projection();
            let ids: Vec<_> = expected.iter().map(|item| item.id.clone()).collect();
            assert_eq!(ids, expected_order.iter().map(|(id, _)| id.clone()).collect::<Vec<_>>());
            let (samples, errors) = measured_samples(|| fixture.state.inbox_projection(), &expected);
            serde_json::json!({"scope": "full-inbox-graph", "samples": samples, "errors": errors,
                "outputs": expected.len(), "output_sha256": digest(&expected)})
        } else {
            let (samples, errors) = measured_samples(|| fixture.state.inbox_orders(), &expected_order);
            serde_json::json!({"scope": "inbox-orders-graph", "samples": samples, "errors": errors,
                "outputs": expected_order.len(), "output_sha256": digest(&expected_order)})
        };
        println!("{}", serde_json::json!({"case": case, "shape": shape, "interactions": count,
            "interaction_body_bytes": count * body_size, "document_revisions": 0,
            "input_sha256": input_hash, "literal_order_sha256": digest(&expected_order),
            "literal_order": expected_order, "adjacency_sha256": digest(&fixture.blockers),
            "graph": graph, "blocker_lookup": lookup, "projection": projection,
            "limits": "Own validated synthetic states. Fixture construction, structural validation, literal/legacy/actual-adjacency checks and counter analysis occur before timing. Lookup and projection samples are separate workloads, not additive stages. Full item output hash must match the retained current-source baseline before any future production change. CPU/allocations are isolated; no RSS, app latency or whole-graph linearity claim."}));
    }
    assert!(executed > 0, "unknown graph case {selected:?}");
}
