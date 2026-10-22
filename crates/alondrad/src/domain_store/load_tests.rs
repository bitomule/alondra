//! Opening the domain replays each task's journal once, in memory linear in its length, however
//! many inputs one task keeps pending.
use super::owner::tests::{running, scope_of};
use super::tests::Fixture;
use super::*;
use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;

struct Allocations;
thread_local! {
    static COUNTING: Cell<bool> = const { Cell::new(false) };
    static BYTES: Cell<u64> = const { Cell::new(0) };
}
fn allocated(size: usize) {
    let _ = COUNTING.try_with(|counting| {
        if counting.get() {
            let _ = BYTES.try_with(|bytes| bytes.set(bytes.get() + size as u64));
        }
    });
}
unsafe impl GlobalAlloc for Allocations {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        allocated(layout.size());
        unsafe { System.alloc(layout) }
    }
    unsafe fn dealloc(&self, pointer: *mut u8, layout: Layout) {
        unsafe { System.dealloc(pointer, layout) }
    }
    unsafe fn realloc(&self, pointer: *mut u8, layout: Layout, size: usize) -> *mut u8 {
        allocated(size);
        unsafe { System.realloc(pointer, layout, size) }
    }
}
#[global_allocator]
static ALLOCATOR: Allocations = Allocations;

/// The bytes this thread allocates while `work` runs.
fn allocated_by<T>(work: impl FnOnce() -> T) -> (T, u64) {
    BYTES.with(|bytes| bytes.set(0));
    COUNTING.with(|counting| counting.set(true));
    let value = work();
    COUNTING.with(|counting| counting.set(false));
    (value, BYTES.with(Cell::get))
}

/// A task running through the Agent SDK whose node was told `notices` times and read none of
/// them, written as the domain writes what it tells a node.
fn told(notices: usize) -> (Fixture, DomainStore) {
    let (fixture, mut store, _) = running();
    let state = store.load().unwrap();
    let task: TaskId = "task-1".into();
    let mut records = state.lifecycle_journal[&task].clone();
    let known = records.len();
    records.extend((0..notices).map(|notice| LifecycleRecord::Told {
        input: format!("input:task-1:notice-{notice}"),
        text: Some("Alondra could not close this session yet: no host could take over its session".into()),
    }));
    store
        .own(scope_of(&state), |owner| Ok(repository::save_journal(owner.journal(), &task, &records, known)?))
        .unwrap();
    (fixture, store)
}

/// What opening the domain over `notices` pending inputs costs: the domain it loads, the events
/// it applied and the bytes it allocated.
fn open(notices: usize) -> (DomainState, u64, u64) {
    let (_fixture, store) = told(notices);
    let applied = alondra_domain::measure::applied();
    let (state, bytes) = allocated_by(|| repository::load(&store.conn).unwrap());
    (state, alondra_domain::measure::applied() - applied, bytes)
}

#[test]
fn opening_the_domain_replays_a_long_inbox_once_in_linear_memory() {
    const NOTICES: usize = 1000;
    let (state, applied, bytes) = open(NOTICES);
    let task: TaskId = "task-1".into();
    let events = u64::try_from(state.lifecycle_journal[&task].len() - 1).unwrap();
    let (_, _, doubled) = open(2 * NOTICES);
    let growth = doubled as f64 / bytes as f64;
    eprintln!("{NOTICES} notices: {applied} events applied for {events} journaled, {bytes} bytes; twice as many notices: {doubled} bytes ({growth:.2}×)");
    assert_eq!(applied, events, "opening the domain applied {applied} events for the {events} in its journal");
    assert!(
        growth < 2.6,
        "twice as many pending inputs made opening the domain allocate {growth:.2} times as much ({bytes} → {doubled} bytes)"
    );
}

/// Opens a copy of a real domain database, at `ALONDRA_DOMAIN_DB`, and prints the median time of
/// 20 loads of its domain.
#[test]
#[ignore = "measures a copy of a real HOME: ALONDRA_DOMAIN_DB=… bazelisk test … --test_arg=--ignored"]
fn measure_domain_open() {
    let path = std::env::var("ALONDRA_DOMAIN_DB").expect("ALONDRA_DOMAIN_DB");
    let store = DomainStore::open(std::path::Path::new(&path)).unwrap();
    let mut samples: Vec<f64> = (0..20)
        .map(|_| {
            let started = std::time::Instant::now();
            repository::load(&store.conn).unwrap();
            started.elapsed().as_secs_f64() * 1000.0
        })
        .collect();
    samples.sort_by(f64::total_cmp);
    eprintln!("loading the domain: median {:.1} ms (min {:.1}, max {:.1})", samples[samples.len() / 2], samples[0], samples[samples.len() - 1]);
}

#[test]
fn a_journal_whose_told_texts_the_store_let_go_still_opens() {
    let (_fixture, store) = told(3);
    let before = repository::load(&store.conn).unwrap();
    let state: String = store.conn.query_row("SELECT state FROM tasks WHERE id='task-1'", [], |row| row.get(0)).unwrap();
    store.conn.execute("UPDATE tasks SET state='done' WHERE id='task-1'", []).unwrap();
    store.conn.execute("UPDATE lifecycle_inputs SET body=NULL WHERE task_id='task-1' AND input_id LIKE '%notice%'", []).unwrap();
    store.conn.execute("UPDATE tasks SET state=?1 WHERE id='task-1'", [state]).unwrap();

    let after = repository::load(&store.conn).unwrap();

    let task: TaskId = "task-1".into();
    let told: Vec<_> = after.lifecycle_journal[&task].iter().filter_map(|record| match record {
        LifecycleRecord::Told { text, .. } => Some(text.clone()),
        _ => None,
    }).collect();
    assert_eq!(told, [None, None, None]);
    assert_eq!(after.lifecycle_journal[&task].len(), before.lifecycle_journal[&task].len());
}
