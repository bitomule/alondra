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
        answers: None,
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

fn pages_read(connection: &Connection) -> i32 {
    let (mut current, mut peak) = (0, 0);
    let result = unsafe {
        rusqlite::ffi::sqlite3_db_status(
            connection.handle(), rusqlite::ffi::SQLITE_DBSTATUS_CACHE_MISS,
            &mut current, &mut peak, 0,
        )
    };
    assert_eq!(result, rusqlite::ffi::SQLITE_OK);
    current
}

#[test]
fn opening_the_domain_does_not_read_historical_observation_pages() {
    use sha2::Digest as _;
    let (fixture, store, execution) = running();
    let expected = repository::load(&store.conn).unwrap();
    drop(store);
    let mut small = DomainStore::open(&fixture.path()).unwrap();
    assert_eq!(*small.load().unwrap(), expected);
    let small_reads = pages_read(&small.conn);
    let digest: [u8; 32] = sha2::Sha256::digest([1u8]).into();
    small.conn.execute(
        "WITH RECURSIVE cursors(cursor) AS (VALUES(1) UNION ALL SELECT cursor+1 FROM cursors WHERE cursor<500000)
         INSERT INTO provider_observations(task_id,generation,binding_id,cursor,digest,bytes)
         SELECT task_id,generation,binding_id,cursor,?1,CASE WHEN cursor=500000 THEN X'01' ELSE X'00' END
         FROM lifecycle_execution_bindings CROSS JOIN cursors;",
        [digest.as_slice()],
    ).unwrap();
    small.conn.execute_batch("INSERT INTO provider_observation_heads(task_id,generation,cursor)
        SELECT task_id,generation,500000 FROM lifecycle_execution_bindings;").unwrap();
    small.own(scope_of(&expected), |owner| {
        owner.journal().put_normalization_checkpoint(&execution, 500_000, None, b"normalized".to_vec())?;
        Ok(())
    }).unwrap();
    let rows: i64 = small.conn.query_row("SELECT count(*) FROM provider_observations", [], |r| r.get(0)).unwrap();
    assert_eq!(rows, 500_000);
    drop(small);

    let mut large = DomainStore::open(&fixture.path()).unwrap();
    assert_eq!(*large.load().unwrap(), expected);
    let large_reads = pages_read(&large.conn);
    eprintln!("opening the same domain: {small_reads} pages without observation history, {large_reads} with {rows} observations");
    assert!(large_reads <= small_reads + 64,
        "opening the domain read historical evidence: {small_reads} → {large_reads} pages");
    large.integrity_check().unwrap();
    assert!(pages_read(&large.conn) > large_reads + 1_000, "the explicit audit must still examine the historical pages");
    large.conn.execute_batch("DROP VIEW events; DROP TABLE branch_update; DROP TABLE orchestrator_batch_decisions;").unwrap();
    drop(large);
    let mut extended = DomainStore::open(&fixture.path()).unwrap();
    assert_eq!(*extended.load().unwrap(), expected);
    let extended_reads = pages_read(&extended.conn);
    assert!(extended_reads <= small_reads + 64,
        "adding tables and a view read historical evidence: {small_reads} → {extended_reads} pages");
    extended.conn.execute_batch("ALTER TABLE model_decisions DROP COLUMN provider").unwrap();
    drop(extended);
    let migrated = DomainStore::open(&fixture.path()).unwrap();
    assert!(pages_read(&migrated.conn) > extended_reads + 1_000,
        "changing an existing table must retain its exhaustive validation");
    migrated.conn.execute_batch("CREATE TABLE node_costs(obsolete TEXT)").unwrap();
    drop(migrated);
    let cleaned = DomainStore::open(&fixture.path()).unwrap();
    assert!(pages_read(&cleaned.conn) > extended_reads + 1_000,
        "removing an existing table must retain its exhaustive validation");
}
