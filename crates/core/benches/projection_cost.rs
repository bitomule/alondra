use alondra_core::domain::*;
use std::{alloc::{GlobalAlloc, Layout, System}, collections::BTreeSet, sync::atomic::{AtomicBool, AtomicUsize, Ordering}, time::Instant};
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
    let mut state = DomainState::new("synthetic-projection-epoch");
    commit(&mut state, Actor::System, Command::RegisterAgent { agent_id: "agent".into(), name: "Synthetic agent".into() });
    commit(&mut state, user(), Command::CreateRequest { request_id: "request".into(), body: "Synthetic request".into(), auto_merge: false, maintenance: false });
    commit(&mut state, user(), Command::CreateTask { task_id: "task".into(), request_id: "request".into(), title: "Synthetic task".into(), agent_id: Some("agent".into()), review_prs: true, dependencies: BTreeSet::new(), stack: None });
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
