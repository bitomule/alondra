use alondra_core::operation_id::{MAX_OPERATION_COUNTER, OperationId};
use anyhow::{Context, Result};
use rusqlite::{Connection, OptionalExtension, Transaction, params};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};

pub(crate) const MAX_PENDING: usize = 128;
pub(crate) const MAX_LEGACY: usize = 16_384;
pub(crate) const MAX_CACHED_REPLIES: usize = 64;
pub(crate) const MAX_CACHED_BYTES: usize = 8 * 1024 * 1024;

#[derive(Debug)]
pub(crate) struct Capacity;
impl std::fmt::Display for Capacity {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("operation journal capacity exceeded before execution")
    }
}
impl std::error::Error for Capacity {}
#[derive(Debug)]
pub(crate) struct Retired;
impl std::fmt::Display for Retired {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("operation was retired")
    }
}
impl std::error::Error for Retired {}
pub(crate) const MAX_RANGES: usize = MAX_PENDING + 1;
const MARKER: &[u8] = b"alondra-operation-journal\n";

#[derive(Clone)]
pub(crate) struct Scope {
    pub issuer: String,
    pub binding: Vec<u8>,
    pub data_epoch: String,
}
#[derive(Clone, Copy)]
pub(crate) enum Backend {
    Encrypted,
    Loopback,
}
impl Backend {
    fn table(self) -> &'static str {
        match self {
            Self::Encrypted => "service_receipts",
            Self::Loopback => "action_results",
        }
    }
    fn id(self) -> &'static str {
        match self {
            Self::Encrypted => "request_id",
            Self::Loopback => "command_id",
        }
    }
}
#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct Range {
    pub first: u64,
    pub last: u64,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Ack {
    pub data_epoch: String,
    pub generation: u64,
    pub ranges: Vec<Range>,
}
#[derive(Clone)]
pub(crate) struct Descriptor {
    pub data_epoch: String,
    pub generation: u64,
}
impl Descriptor {
    pub(crate) fn value(&self) -> Value {
        json!({"protocol":1,"data_epoch":self.data_epoch,"generation":self.generation,
        "max_pending":MAX_PENDING,"max_ranges":MAX_RANGES,"max_sequence":MAX_OPERATION_COUNTER})
    }
}
pub(crate) fn sql_counter(value: u64) -> Result<i64> {
    anyhow::ensure!(
        value <= MAX_OPERATION_COUNTER,
        "operation counter exceeds protocol limit"
    );
    Ok(i64::try_from(value)?)
}
fn counter_at(row: &rusqlite::Row<'_>, index: usize) -> rusqlite::Result<u64> {
    let value: i64 = row.get(index)?;
    let counter = u64::try_from(value).map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(
            index,
            rusqlite::types::Type::Integer,
            Box::new(error),
        )
    })?;
    if counter > MAX_OPERATION_COUNTER {
        return Err(rusqlite::Error::FromSqlConversionFailure(
            index,
            rusqlite::types::Type::Integer,
            Box::new(std::io::Error::other(
                "operation counter exceeds protocol limit",
            )),
        ));
    }
    Ok(counter)
}
fn marker(path: &Path) -> PathBuf {
    path.with_extension("operation-journal-schema")
}
pub(crate) fn guard(path: &Path) -> Result<bool> {
    use std::io::Read;
    let marker = marker(path);
    if !marker.try_exists()? {
        return Ok(false);
    }
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK);
    }
    let file = options.open(marker)?;
    anyhow::ensure!(
        file.metadata()?.is_file(),
        "operation journal marker must be a regular file"
    );
    let mut bytes = Vec::new();
    file.take(128).read_to_end(&mut bytes)?;
    anyhow::ensure!(bytes == MARKER, "invalid operation journal marker");
    anyhow::ensure!(
        path.try_exists()?
            && std::fs::symlink_metadata(path)?.is_file()
            && std::fs::metadata(path)?.len() > 0,
        "initialized operation journal is missing or empty"
    );
    Ok(true)
}
pub(crate) fn initialize(path: &Path, c: &mut Connection, backend: Backend) -> Result<()> {
    use std::io::Write;
    let initialized = guard(path)?;
    if initialized {
        let generation: u64 = c.query_row(
            "SELECT generation FROM operation_meta WHERE singleton=1",
            [],
            |row| counter_at(row, 0),
        )?;
        anyhow::ensure!(
            generation <= MAX_OPERATION_COUNTER,
            "invalid operation generation"
        );
        let highest: Option<i64> =
            c.query_row("SELECT max(generation) FROM operation_issuers", [], |row| {
                row.get(0)
            })?;
        let highest = highest.map(u64::try_from).transpose()?;
        anyhow::ensure!(
            highest.is_none_or(|highest| highest <= generation),
            "operation generation metadata regressed"
        );
        c.prepare("SELECT issuer,binding,data_epoch,generation,pending_count,retired_digest FROM operation_issuers LIMIT 0")?;
        c.prepare("SELECT issuer,generation,first,last FROM operation_retired LIMIT 0")?;
        c.prepare(&format!(
            "SELECT journal_issuer,journal_generation,journal_sequence FROM {} LIMIT 0",
            backend.table()
        ))?;
        let index:bool=c.query_row("SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='index' AND name='operation_sequence')",[],|row|row.get(0))?;
        anyhow::ensure!(index, "operation sequence uniqueness guard is missing");
        return Ok(());
    }
    let tx = c.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
    tx.execute_batch("CREATE TABLE IF NOT EXISTS operation_meta(singleton INTEGER PRIMARY KEY CHECK(singleton=1),generation INTEGER NOT NULL);
        CREATE TABLE IF NOT EXISTS operation_issuers(issuer TEXT PRIMARY KEY,binding BLOB NOT NULL,data_epoch TEXT NOT NULL,generation INTEGER NOT NULL UNIQUE,pending_count INTEGER NOT NULL,retired_digest BLOB NOT NULL);
        CREATE TABLE IF NOT EXISTS operation_retired(issuer TEXT NOT NULL,generation INTEGER NOT NULL,first INTEGER NOT NULL,last INTEGER NOT NULL,PRIMARY KEY(issuer,generation,first));")?;
    let prior: Option<u64> = tx
        .query_row(
            "SELECT generation FROM operation_meta WHERE singleton=1",
            [],
            |row| counter_at(row, 0),
        )
        .optional()?;
    anyhow::ensure!(
        prior.is_none_or(|generation| generation == 0),
        "operation journal lost its marker"
    );
    let issuers: i64 = tx.query_row("SELECT count(*) FROM operation_issuers", [], |row| {
        row.get(0)
    })?;
    let retired: i64 = tx.query_row("SELECT count(*) FROM operation_retired", [], |row| {
        row.get(0)
    })?;
    anyhow::ensure!(
        issuers == 0 && retired == 0,
        "operation journal lost its initialized marker"
    );
    tx.execute("INSERT OR IGNORE INTO operation_meta VALUES(1,0)", [])?;
    let columns: Vec<String> = {
        let mut statement = tx.prepare(&format!("PRAGMA table_info({})", backend.table()))?;
        statement
            .query_map([], |row| row.get(1))?
            .collect::<rusqlite::Result<_>>()?
    };
    for (name, kind) in [
        ("journal_issuer", "TEXT"),
        ("journal_generation", "INTEGER"),
        ("journal_sequence", "INTEGER"),
    ] {
        if !columns.iter().any(|column| column == name) {
            tx.execute_batch(&format!(
                "ALTER TABLE {} ADD COLUMN {name} {kind}",
                backend.table()
            ))?;
        }
    }
    tx.execute_batch(&format!("CREATE UNIQUE INDEX IF NOT EXISTS operation_sequence ON {}(journal_issuer,journal_generation,journal_sequence) WHERE journal_issuer IS NOT NULL",backend.table()))?;
    tx.commit()?;
    let mut options = std::fs::OpenOptions::new();
    options.create_new(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC);
    }
    let mut file = options.open(marker(path))?;
    file.write_all(MARKER)?;
    file.sync_all()?;
    std::fs::File::open(path.parent().context("operation journal parent missing")?)?.sync_all()?;
    Ok(())
}

pub(crate) fn descriptor(
    tx: &Transaction<'_>,
    scope: &Scope,
    backend: Backend,
) -> Result<Descriptor> {
    let prior: Option<(Vec<u8>, String, u64)> = tx
        .query_row(
            "SELECT binding,data_epoch,generation FROM operation_issuers WHERE issuer=?1",
            params![scope.issuer],
            |row| Ok((row.get(0)?, row.get(1)?, counter_at(row, 2)?)),
        )
        .optional()?;
    if let Some((binding, epoch, generation)) = prior {
        if binding == scope.binding && epoch == scope.data_epoch {
            verify(tx, scope, generation, backend)?;
            return Ok(Descriptor {
                data_epoch: epoch,
                generation,
            });
        }
    }
    let current: u64 = tx.query_row(
        "SELECT generation FROM operation_meta WHERE singleton=1",
        [],
        |row| counter_at(row, 0),
    )?;
    anyhow::ensure!(
        current < MAX_OPERATION_COUNTER,
        "operation generation exhausted"
    );
    let generation = current + 1;
    tx.execute(
        "UPDATE operation_meta SET generation=?1 WHERE singleton=1",
        params![sql_counter(generation)?],
    )?;
    tx.execute("INSERT INTO operation_issuers(issuer,binding,data_epoch,generation,pending_count,retired_digest) VALUES(?1,?2,?3,?4,0,?5)
        ON CONFLICT(issuer) DO UPDATE SET binding=excluded.binding,data_epoch=excluded.data_epoch,generation=excluded.generation,pending_count=0,retired_digest=excluded.retired_digest",
        params![scope.issuer,scope.binding,scope.data_epoch,sql_counter(generation)?,retired_digest(&[])])?;
    tx.execute(
        "DELETE FROM operation_retired WHERE issuer=?1",
        params![scope.issuer],
    )?;
    tx.execute(
        &format!("DELETE FROM {} WHERE journal_issuer=?1", backend.table()),
        params![scope.issuer],
    )?;
    Ok(Descriptor {
        data_epoch: scope.data_epoch.clone(),
        generation,
    })
}
fn ranges(tx: &Transaction<'_>, scope: &Scope, generation: u64) -> Result<Vec<Range>> {
    let mut statement = tx.prepare(
        "SELECT first,last FROM operation_retired WHERE issuer=?1 AND generation=?2 ORDER BY first",
    )?;
    Ok(statement
        .query_map(params![scope.issuer, sql_counter(generation)?], |row| {
            Ok(Range {
                first: counter_at(row, 0)?,
                last: counter_at(row, 1)?,
            })
        })?
        .collect::<rusqlite::Result<_>>()?)
}
fn retired_digest(ranges: &[Range]) -> Vec<u8> {
    Sha256::digest(serde_json::to_vec(ranges).expect("retired ranges encode")).to_vec()
}
fn verify(tx: &Transaction<'_>, scope: &Scope, generation: u64, backend: Backend) -> Result<()> {
    let (expected_pending,expected_retired):(u64,Vec<u8>)=tx.query_row("SELECT pending_count,retired_digest FROM operation_issuers WHERE issuer=?1 AND generation=?2",
        params![scope.issuer,sql_counter(generation)?],|row|Ok((counter_at(row,0)?,row.get(1)?)))?;
    let pending: u64 = tx.query_row(
        &format!(
            "SELECT count(*) FROM {} WHERE journal_issuer=?1 AND journal_generation=?2",
            backend.table()
        ),
        params![scope.issuer, sql_counter(generation)?],
        |row| counter_at(row, 0),
    )?;
    let retired = ranges(tx, scope, generation)?;
    anyhow::ensure!(
        pending == expected_pending
            && retired == merge(retired.clone())?
            && retired_digest(&retired) == expected_retired,
        "operation receipt or retirement metadata is missing"
    );
    Ok(())
}
pub(crate) fn opted_in(tx: &Transaction<'_>, scope: &Scope) -> Result<bool> {
    Ok(tx.query_row("SELECT EXISTS(SELECT 1 FROM operation_issuers WHERE issuer=?1 AND binding=?2 AND data_epoch=?3)",
        params![scope.issuer,scope.binding,scope.data_epoch],|row|row.get(0))?)
}

pub(crate) fn admit(
    tx: &Transaction<'_>,
    scope: &Scope,
    id: &str,
    backend: Backend,
) -> Result<Option<OperationId>> {
    let Some(operation) = OperationId::parse(id) else {
        return Ok(None);
    };
    let descriptor = descriptor(tx, scope, backend)?;
    anyhow::ensure!(
        operation.matches_epoch(&scope.data_epoch) && operation.generation == descriptor.generation,
        "operation generation or epoch changed"
    );
    let retired:bool = tx.query_row("SELECT EXISTS(SELECT 1 FROM operation_retired WHERE issuer=?1 AND generation=?2 AND first<=?3 AND last>=?3)",
        params![scope.issuer,sql_counter(operation.generation)?,sql_counter(operation.sequence)?],|row|row.get(0))?;
    if retired {
        return Err(Retired.into());
    }
    let prior:Option<String> = tx.query_row(&format!("SELECT {} FROM {} WHERE journal_issuer=?1 AND journal_generation=?2 AND journal_sequence=?3",backend.id(),backend.table()),
        params![scope.issuer,sql_counter(operation.generation)?,sql_counter(operation.sequence)?],|row|row.get(0)).optional()?;
    if let Some(prior) = prior {
        anyhow::ensure!(
            prior == id,
            "operation sequence reused with another identity"
        );
    } else {
        let pending: i64 = tx.query_row(
            &format!(
                "SELECT count(*) FROM {} WHERE journal_issuer=?1 AND journal_generation=?2",
                backend.table()
            ),
            params![scope.issuer, sql_counter(operation.generation)?],
            |row| row.get(0),
        )?;
        if pending >= MAX_PENDING as i64 {
            return Err(Capacity.into());
        }
        let legacy: bool = tx.query_row(
            &format!(
                "SELECT EXISTS(SELECT 1 FROM {} WHERE {}=?1 AND journal_issuer IS NULL)",
                backend.table(),
                backend.id()
            ),
            params![id],
            |row| row.get(0),
        )?;
        anyhow::ensure!(
            !legacy,
            "structured operation has unclassified prior receipt"
        );
        tx.execute(
            "UPDATE operation_issuers SET pending_count=pending_count+1 WHERE issuer=?1",
            params![scope.issuer],
        )?;
    }
    Ok(Some(operation))
}
fn merge(mut ranges: Vec<Range>) -> Result<Vec<Range>> {
    ranges.sort_unstable_by_key(|range| range.first);
    let mut merged: Vec<Range> = Vec::new();
    for range in ranges {
        anyhow::ensure!(
            range.first > 0 && range.first <= range.last && range.last <= MAX_OPERATION_COUNTER,
            "invalid acknowledgement range"
        );
        if let Some(last) = merged.last_mut() {
            if range.first <= last.last + 1 {
                last.last = last.last.max(range.last);
                continue;
            }
        }
        merged.push(range);
    }
    anyhow::ensure!(
        merged.len() <= MAX_RANGES,
        "operation retirement range bound reached"
    );
    Ok(merged)
}
/// Cuts the given sequences out of sorted, merged ranges.
fn without(ranges: &[Range], kept: &[u64]) -> Vec<Range> {
    let mut result = Vec::new();
    for range in ranges {
        let mut first = range.first;
        for &sequence in kept.iter().filter(|s| (range.first..=range.last).contains(*s)) {
            if first < sequence {
                result.push(Range { first, last: sequence - 1 });
            }
            first = sequence + 1;
        }
        if first <= range.last {
            result.push(Range { first, last: range.last });
        }
    }
    result
}

/// Retires what the client will never send again. Retiring only ever refuses an ID for good, so a
/// sequence this Mac never saw, a finished one and one whose executor died are all retired; only an
/// operation `live` says is still running keeps its row, and the answer then says the
/// acknowledgement is not complete so the client asks again later.
pub(crate) fn acknowledge(
    tx: &Transaction<'_>,
    scope: &Scope,
    ack: &Ack,
    backend: Backend,
    live: &dyn Fn(&str) -> bool,
) -> Result<Value> {
    anyhow::ensure!(
        ack.data_epoch == scope.data_epoch
            && !ack.ranges.is_empty()
            && ack.ranges.len() <= MAX_RANGES,
        "invalid operation acknowledgement"
    );
    let descriptor = descriptor(tx, scope, backend)?;
    anyhow::ensure!(
        ack.generation == descriptor.generation,
        "operation generation changed"
    );
    let retired = ranges(tx, scope, descriptor.generation)?;
    let requested = merge(ack.ranges.clone())?;
    let mut kept = Vec::new();
    for range in &requested {
        let mut statement = tx.prepare(&format!("SELECT {},journal_sequence FROM {} WHERE journal_issuer=?1 AND journal_generation=?2 AND journal_sequence BETWEEN ?3 AND ?4 AND state NOT IN ('completed','interrupted')",backend.id(),backend.table()))?;
        let unfinished = statement
            .query_map(params![scope.issuer,sql_counter(descriptor.generation)?,sql_counter(range.first)?,sql_counter(range.last)?],
                |row| Ok((row.get::<_, String>(0)?, counter_at(row, 1)?)))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        kept.extend(unfinished.into_iter().filter(|(id, _)| live(id)).map(|(_, sequence)| sequence));
    }
    kept.sort_unstable();
    let retiring = without(&requested, &kept);
    let Ok(merged) = merge(retired.iter().copied().chain(retiring.iter().copied()).collect()) else {
        let mut result = descriptor.value();
        result["acknowledged"] = json!(false);
        result["ranges"] = json!(retired);
        return Ok(result);
    };
    let requested = retiring;
    tx.execute(
        "DELETE FROM operation_retired WHERE issuer=?1",
        params![scope.issuer],
    )?;
    for range in &merged {
        tx.execute(
            "INSERT INTO operation_retired VALUES(?1,?2,?3,?4)",
            params![
                scope.issuer,
                sql_counter(descriptor.generation)?,
                sql_counter(range.first)?,
                sql_counter(range.last)?
            ],
        )?;
    }
    for range in requested {
        tx.execute(&format!("DELETE FROM {} WHERE journal_issuer=?1 AND journal_generation=?2 AND journal_sequence BETWEEN ?3 AND ?4",backend.table()),
        params![scope.issuer,sql_counter(descriptor.generation)?,sql_counter(range.first)?,sql_counter(range.last)?])?;
    }
    let pending: u64 = tx.query_row(
        &format!(
            "SELECT count(*) FROM {} WHERE journal_issuer=?1 AND journal_generation=?2",
            backend.table()
        ),
        params![scope.issuer, sql_counter(descriptor.generation)?],
        |row| counter_at(row, 0),
    )?;
    tx.execute(
        "UPDATE operation_issuers SET pending_count=?2,retired_digest=?3 WHERE issuer=?1",
        params![scope.issuer, sql_counter(pending)?, retired_digest(&merged)],
    )?;
    let mut result = descriptor.value();
    result["acknowledged"] = json!(kept.is_empty());
    result["ranges"] = json!(merged);
    Ok(result)
}

pub(crate) fn query_epoch(path: &str) -> Option<String> {
    let query = path.strip_prefix("/operation-journal?")?;
    let mut values = url::form_urlencoded::parse(query.as_bytes());
    let (key, value) = values.next()?;
    (key == "data_epoch"
        && !value.is_empty()
        && value.len() <= 256
        && value.bytes().all(|byte| (33..=126).contains(&byte))
        && values.next().is_none())
    .then(|| value.into_owned())
}
pub(crate) fn control(method: &str, path: &str, has_body: bool) -> bool {
    method == "GET" && !has_body && query_epoch(path).is_some()
        || method == "POST" && has_body && path == "/operation-journal/ack"
}

#[cfg(test)]
mod tests {
    use super::*;
    use alondra_core::operation_id::epoch_tag;
    struct Fixture {
        c: Connection,
        path: PathBuf,
        _directory: crate::test_support::Directory,
    }
    impl Fixture {
        fn new() -> Self {
            let directory = crate::test_support::Directory::new();
            std::fs::create_dir_all(&directory).unwrap();
            let path = directory.join("receipts.sqlite");
            let mut c = Connection::open(&path).unwrap();
            c.execute_batch("PRAGMA journal_mode=WAL;PRAGMA synchronous=FULL;CREATE TABLE service_receipts(sender TEXT,request_id TEXT UNIQUE,digest BLOB,state TEXT,response BLOB);
                CREATE TABLE command_audit(value TEXT);INSERT INTO command_audit VALUES('domain history');").unwrap();
            initialize(&path, &mut c, Backend::Encrypted).unwrap();
            Self {
                c,
                path,
                _directory: directory,
            }
        }
        fn scope() -> Scope {
            Scope {
                issuer: "encrypted:phone".into(),
                binding: vec![1; 32],
                data_epoch: "epoch".into(),
            }
        }
        fn id(generation: u64, sequence: u64) -> String {
            format!(
                "op_{}_{generation}_{sequence}_5f0c2d9e-8d0b-4c71-9b0e-2b6f5d8f1a10",
                epoch_tag("epoch")
            )
        }
    }
    fn insert(tx: &Transaction<'_>, scope: &Scope, id: &str, state: &str) -> Result<()> {
        let operation = admit(tx, scope, id, Backend::Encrypted)?.unwrap();
        tx.execute("INSERT INTO service_receipts(sender,request_id,digest,state,response,journal_issuer,journal_generation,journal_sequence) VALUES('phone',?1,?2,?3,?4,?5,?6,?7)",
            params![id,&[7u8;32],state,b"original reply".as_slice(),scope.issuer,sql_counter(operation.generation)?,sql_counter(operation.sequence)?])?;
        Ok(())
    }
    #[test]
    fn acknowledgement_removes_rows_but_replay_and_changed_identity_never_become_new_after_reopen()
    {
        let mut f = Fixture::new();
        let scope = Fixture::scope();
        let tx = f.c.transaction().unwrap();
        let generation = descriptor(&tx, &scope, Backend::Encrypted)
            .unwrap()
            .generation;
        let id = Fixture::id(generation, 1);
        insert(&tx, &scope, &id, "completed").unwrap();
        assert!(
            admit(
                &tx,
                &scope,
                &id.replace("5f0c2d9e", "af0c2d9e"),
                Backend::Encrypted
            )
            .is_err()
        );
        let ack = Ack {
            data_epoch: "epoch".into(),
            generation,
            ranges: vec![Range { first: 1, last: 1 }],
        };
        let first = acknowledge(&tx, &scope, &ack, Backend::Encrypted, &|_| true).unwrap();
        assert_eq!(first["acknowledged"], true);
        assert_eq!(
            tx.query_row("SELECT count(*) FROM service_receipts", [], |row| row
                .get::<_, i64>(0))
                .unwrap(),
            0
        );
        assert_eq!(
            acknowledge(&tx, &scope, &ack, Backend::Encrypted, &|_| true).unwrap(),
            first
        );
        tx.commit().unwrap();
        drop(f.c);
        let mut reopened = Connection::open(&f.path).unwrap();
        initialize(&f.path, &mut reopened, Backend::Encrypted).unwrap();
        let tx = reopened.transaction().unwrap();
        assert!(admit(&tx, &scope, &id, Backend::Encrypted).is_err());
        assert_eq!(
            tx.query_row("SELECT count(*) FROM command_audit", [], |row| row
                .get::<_, i64>(0))
                .unwrap(),
            1
        );
    }
    #[test]
    fn retired_ranges_merge_behind_an_unknown_gap_without_scanning_each_sequence() {
        let mut f = Fixture::new();
        let scope = Fixture::scope();
        let tx = f.c.transaction().unwrap();
        let generation = descriptor(&tx, &scope, Backend::Encrypted)
            .unwrap()
            .generation;
        insert(&tx, &scope, &Fixture::id(generation, 1), "executing").unwrap();
        tx.execute(
            "INSERT INTO operation_retired VALUES(?1,?2,2,999999)",
            params![scope.issuer, sql_counter(generation).unwrap()],
        )
        .unwrap();
        tx.execute(
            "UPDATE operation_issuers SET retired_digest=?2 WHERE issuer=?1",
            params![
                scope.issuer,
                retired_digest(&[Range {
                    first: 2,
                    last: 999999
                }])
            ],
        )
        .unwrap();
        insert(
            &tx,
            &scope,
            &Fixture::id(generation, 1_000_000),
            "completed",
        )
        .unwrap();
        let ack = Ack {
            data_epoch: "epoch".into(),
            generation,
            ranges: vec![Range {
                first: 2,
                last: 1_000_000,
            }],
        };
        let reply = acknowledge(&tx, &scope, &ack, Backend::Encrypted, &|_| true).unwrap();
        assert_eq!(reply["ranges"], json!([{ "first":2,"last":1_000_000 }]));
        assert_eq!(
            tx.query_row("SELECT count(*) FROM operation_retired", [], |row| row
                .get::<_, i64>(0))
                .unwrap(),
            1
        );
        assert_eq!(
            tx.query_row("SELECT count(*) FROM service_receipts", [], |row| row
                .get::<_, i64>(0))
                .unwrap(),
            1
        );
        let live = acknowledge(
            &tx,
            &scope,
            &Ack {
                ranges: vec![Range { first: 1, last: 1 }],
                ..ack
            },
            Backend::Encrypted,
            &|_| true,
        )
        .unwrap();
        assert_eq!(live["acknowledged"], false);
        assert_eq!(live["ranges"], json!([{ "first":2,"last":1_000_000 }]));
    }
    #[test]
    fn epoch_approval_and_new_issuer_share_one_monotonic_generation_counter() {
        let mut f = Fixture::new();
        let mut scope = Fixture::scope();
        let tx = f.c.transaction().unwrap();
        let first = descriptor(&tx, &scope, Backend::Encrypted).unwrap();
        insert(&tx, &scope, &Fixture::id(first.generation, 1), "completed").unwrap();
        assert_eq!(
            descriptor(&tx, &scope, Backend::Encrypted)
                .unwrap()
                .generation,
            first.generation
        );
        scope.data_epoch = "other".into();
        let second = descriptor(&tx, &scope, Backend::Encrypted).unwrap();
        assert!(second.generation > first.generation);
        assert_eq!(
            tx.query_row("SELECT count(*) FROM service_receipts", [], |row| row
                .get::<_, i64>(0))
                .unwrap(),
            0
        );
        scope.binding = vec![2; 32];
        let third = descriptor(&tx, &scope, Backend::Encrypted).unwrap();
        assert!(third.generation > second.generation);
        scope.issuer = "encrypted:other-phone".into();
        let fourth = descriptor(&tx, &scope, Backend::Encrypted).unwrap();
        assert!(fourth.generation > third.generation);
        assert!(
            admit(
                &tx,
                &scope,
                &Fixture::id(first.generation, 1),
                Backend::Encrypted
            )
            .is_err()
        );
    }
    #[test]
    fn unknown_dead_and_finished_operations_retire_but_a_live_one_keeps_its_row() {
        let mut f = Fixture::new();
        let scope = Fixture::scope();
        let tx = f.c.transaction().unwrap();
        let generation = descriptor(&tx, &scope, Backend::Encrypted)
            .unwrap()
            .generation;
        let live_id = Fixture::id(generation, 2);
        insert(&tx, &scope, &Fixture::id(generation, 1), "completed").unwrap();
        insert(&tx, &scope, &live_id, "executing").unwrap();
        insert(&tx, &scope, &Fixture::id(generation, 4), "executing").unwrap();
        insert(&tx, &scope, &Fixture::id(generation, 5), "interrupted").unwrap();
        let reply = acknowledge(
            &tx,
            &scope,
            &Ack {
                data_epoch: "epoch".into(),
                generation,
                ranges: vec![Range { first: 1, last: 6 }],
            },
            Backend::Encrypted,
            &|id| id == live_id,
        )
        .unwrap();
        assert_eq!(reply["acknowledged"], false);
        assert_eq!(
            reply["ranges"],
            json!([{ "first":1,"last":1 }, { "first":3,"last":6 }])
        );
        let rows: Vec<String> = tx
            .prepare("SELECT request_id FROM service_receipts")
            .unwrap()
            .query_map([], |row| row.get(0))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap();
        assert_eq!(rows, vec![live_id.clone()]);
        descriptor(&tx, &scope, Backend::Encrypted).expect("retirement metadata stays consistent");
        assert!(admit(&tx, &scope, &Fixture::id(generation, 3), Backend::Encrypted)
            .unwrap_err()
            .is::<Retired>());
        tx.execute("UPDATE service_receipts SET state='completed'", []).unwrap();
        let rest = acknowledge(
            &tx,
            &scope,
            &Ack {
                data_epoch: "epoch".into(),
                generation,
                ranges: vec![Range { first: 1, last: 6 }],
            },
            Backend::Encrypted,
            &|id| id == live_id,
        )
        .unwrap();
        assert_eq!(rest["acknowledged"], true);
        assert_eq!(rest["ranges"], json!([{ "first":1,"last":6 }]));
        let fragmented: Vec<_> = (0..=MAX_RANGES)
            .map(|i| Range {
                first: 2 * i as u64 + 1,
                last: 2 * i as u64 + 1,
            })
            .collect();
        assert!(merge(fragmented).is_err());
    }
    #[test]
    fn initialized_generation_metadata_loss_fails_closed() {
        let mut f = Fixture::new();
        let tx = f.c.transaction().unwrap();
        descriptor(&tx, &Fixture::scope(), Backend::Encrypted).unwrap();
        tx.commit().unwrap();
        f.c.execute("DELETE FROM operation_meta", []).unwrap();
        assert!(initialize(&f.path, &mut f.c, Backend::Encrypted).is_err());
        drop(f.c);
        std::fs::remove_file(&f.path).unwrap();
        assert!(guard(&f.path).is_err());
    }
    #[test]
    fn losing_retirement_rows_or_pending_rows_cannot_make_old_operations_new() {
        for retired in [true, false] {
            let mut f = Fixture::new();
            let scope = Fixture::scope();
            let tx = f.c.transaction().unwrap();
            let generation = descriptor(&tx, &scope, Backend::Encrypted)
                .unwrap()
                .generation;
            let id = Fixture::id(generation, 1);
            insert(&tx, &scope, &id, "completed").unwrap();
            if retired {
                acknowledge(
                    &tx,
                    &scope,
                    &Ack {
                        data_epoch: "epoch".into(),
                        generation,
                        ranges: vec![Range { first: 1, last: 1 }],
                    },
                    Backend::Encrypted,
                    &|_| true,
                )
                .unwrap();
                tx.execute("DELETE FROM operation_retired", []).unwrap();
            } else {
                tx.execute("DELETE FROM service_receipts", []).unwrap();
            }
            assert!(admit(&tx, &scope, &id, Backend::Encrypted).is_err());
        }
    }
    #[test]
    fn pending_limit_counts_completed_unacknowledged_records_before_effects() {
        let mut f = Fixture::new();
        let scope = Fixture::scope();
        let tx = f.c.transaction().unwrap();
        let generation = descriptor(&tx, &scope, Backend::Encrypted)
            .unwrap()
            .generation;
        for sequence in 1..=MAX_PENDING as u64 {
            insert(&tx, &scope, &Fixture::id(generation, sequence), "completed").unwrap();
        }
        assert!(
            admit(
                &tx,
                &scope,
                &Fixture::id(generation, MAX_PENDING as u64 + 1),
                Backend::Encrypted
            )
            .unwrap_err()
            .is::<Capacity>()
        );
    }
}

#[cfg(test)]
#[path = "operation_journal/process_fixture.rs"]
mod process_fixture;
