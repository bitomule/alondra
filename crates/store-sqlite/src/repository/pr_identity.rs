//! A pull request's row keeps which pull request it is apart from where it lives: the provider's
//! opaque key and locator, the current address with every earlier one kept as an alias, and
//! whether the record is still tracked, joined another or was removed. The table refuses what the
//! domain never leaves: a key that changes, an address that moves without its alias, a joined
//! record that is not verified, two tracked records of one key or of one address.
use super::*;
use std::collections::BTreeSet;

const SCHEMA: &str = include_str!("../../../domain-store-schema/schema.sql");

const ALIASES: &str = "CREATE TABLE IF NOT EXISTS pull_request_aliases(pr_id TEXT NOT NULL REFERENCES pull_requests(id),repository TEXT NOT NULL CHECK(length(repository)>0),PRIMARY KEY(pr_id,repository));";

const INDEXES: [(&str, &str); 2] = [
    ("pull_requests_tracked_key", "CREATE UNIQUE INDEX pull_requests_tracked_key ON pull_requests(host,provider_key) WHERE tracking='tracked' AND provider_key IS NOT NULL"),
    ("pull_requests_tracked_address", "CREATE UNIQUE INDEX pull_requests_tracked_address ON pull_requests(host,repository,number) WHERE tracking='tracked' AND identity<>'unresolvable'"),
];

const TRIGGERS: [(&str, &str); 7] = [
    ("immutable_pr_ownership", "CREATE TRIGGER immutable_pr_ownership BEFORE UPDATE ON pull_requests
WHEN NEW.id<>OLD.id OR NEW.host<>OLD.host OR NEW.number<>OLD.number
OR (NEW.task_id<>OLD.task_id AND (SELECT state FROM tasks WHERE id=OLD.task_id) NOT IN('done','cancelled'))
BEGIN SELECT RAISE(ABORT,'PR identity and live ownership are immutable'); END"),
    ("pr_address_moves_with_alias", "CREATE TRIGGER pr_address_moves_with_alias BEFORE UPDATE OF repository ON pull_requests
WHEN NEW.repository<>OLD.repository AND NOT EXISTS(SELECT 1 FROM pull_request_aliases WHERE pr_id=OLD.id AND repository=OLD.repository)
BEGIN SELECT RAISE(ABORT,'a PR moves only from an address kept as its alias'); END"),
    ("immutable_pr_provider_key", "CREATE TRIGGER immutable_pr_provider_key BEFORE UPDATE OF provider_key ON pull_requests
WHEN OLD.provider_key IS NOT NULL AND NEW.provider_key IS NOT OLD.provider_key
BEGIN SELECT RAISE(ABORT,'a PR provider key never changes once known'); END"),
    ("final_pr_tracking", "CREATE TRIGGER final_pr_tracking BEFORE UPDATE OF tracking,superseded_by,detached_at ON pull_requests
WHEN OLD.tracking<>'tracked' AND (NEW.tracking IS NOT OLD.tracking OR NEW.superseded_by IS NOT OLD.superseded_by OR NEW.detached_at IS NOT OLD.detached_at)
BEGIN SELECT RAISE(ABORT,'a joined or removed PR stays so'); END"),
    ("retained_pr", "CREATE TRIGGER retained_pr BEFORE DELETE ON pull_requests BEGIN SELECT RAISE(ABORT,'PR ownership is retained for Task lifetime'); END"),
    ("immutable_pr_alias", "CREATE TRIGGER immutable_pr_alias BEFORE UPDATE ON pull_request_aliases BEGIN SELECT RAISE(ABORT,'a PR alias never changes'); END"),
    ("retained_pr_alias", "CREATE TRIGGER retained_pr_alias BEFORE DELETE ON pull_request_aliases BEGIN SELECT RAISE(ABORT,'a PR alias is retained'); END"),
];

/// The table as the schema declares it, read from a database the schema made.
fn table() -> Result<String> {
    let declared = Connection::open_in_memory()?;
    declared.execute_batch(SCHEMA)?;
    Ok(declared.query_row("SELECT sql FROM sqlite_schema WHERE type='table' AND name='pull_requests'", [], |row| row.get(0))?)
}

/// A table from before identities were kept is made again with its rows, which read as tracked
/// records not checked yet at the address they were registered at; the other tables' references
/// to it wait for the rows to be back, at the end of this transaction. Its indexes and triggers
/// are then made as declared here, whatever shape it had.
pub(crate) fn upgrade(tx: &rusqlite::Transaction<'_>) -> Result<()> {
    let identified = tx.query_row("SELECT count(*) FROM pragma_table_info('pull_requests') WHERE name='provider_key'", [], |row| row.get::<_, i64>(0))? == 1;
    if !identified {
        const KEPT: &str = "id,task_id,host,repository,number,head,base,state,merged_head,allowed_methods";
        tx.execute_batch("PRAGMA defer_foreign_keys=ON; CREATE TABLE pull_requests_copy AS SELECT * FROM pull_requests; DROP TABLE pull_requests;")?;
        tx.execute_batch(&table()?)?;
        tx.execute_batch(&format!("INSERT INTO pull_requests({KEPT}) SELECT {KEPT} FROM pull_requests_copy; DROP TABLE pull_requests_copy;"))?;
    }
    tx.execute_batch(ALIASES)?;
    for (name, definition) in INDEXES {
        crate::refresh_schema_object(tx, "index", name, definition)?;
    }
    for (name, definition) in TRIGGERS {
        crate::refresh_schema_object(tx, "trigger", name, definition)?;
    }
    Ok(())
}

/// The row of `p`: its identity and tracking in their own columns, its aliases kept apart.
pub(super) fn row(p: &PullRequest) -> Result<Map<String, Value>> {
    let mut row = object(p)?;
    strip(&mut row, &["review", "aliases", "identity", "tracking", "missing", "relocation_asked_at"]);
    let (identity, key, locator, reason, since) = match &p.identity {
        PrIdentity::Unverified => ("unverified", None, None, None, None),
        PrIdentity::Verified { key, locator } => ("verified", Some(&key.0), Some(&locator.0), None, None),
        PrIdentity::Unresolvable { key, reason, since } => ("unresolvable", key.as_ref().map(|key| &key.0), None, Some(value(reason)?), Some(*since)),
    };
    let (tracking, by, at) = match &p.tracking {
        PrTracking::Tracked => ("tracked", None, None),
        PrTracking::Superseded { by } => ("superseded", Some(&by.0), None),
        PrTracking::Detached { at } => ("detached", None, Some(*at)),
    };
    row.extend(object(&json!({
        "address_read_at": p.address_read_at,
        "identity": identity, "provider_key": key, "provider_locator": locator,
        "unresolvable_reason": reason, "unresolvable_since": since,
        "tracking": tracking, "superseded_by": by, "detached_at": at,
        "missing_strikes": p.missing.map(|m| m.strikes), "missing_last": p.missing.map(|m| m.last),
        "relocation_asked_at": p.relocation_asked_at,
    }))?);
    Ok(row)
}

/// The aliases `p` gained, which a move of its address needs written first.
pub(super) fn save_aliases(c: &Connection, p: &PullRequest, before: Option<&PullRequest>) -> Result<()> {
    for alias in p.aliases.iter().filter(|alias| before.is_none_or(|b| !b.aliases.contains(*alias))) {
        exec(c, "INSERT OR IGNORE INTO pull_request_aliases(pr_id,repository) VALUES(?1,?2)", params![p.id.0, alias])?;
    }
    Ok(())
}

/// Whether the record stands in the tables' unique indexes after the change: one that leaves them
/// is written before one that takes the key or the address it held.
pub(super) fn holds_an_index(p: &PullRequest) -> bool {
    p.is_tracked() && !p.identity.is_unresolvable()
}

/// Every alias row, by pull request. A record keeps every address it was confirmed at, its
/// current one included when it came back to an earlier address, so its aliases are those rows
/// but its address.
pub(super) fn aliases(c: &Connection) -> Result<BTreeMap<String, BTreeSet<String>>> {
    let mut by_pr: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    let mut statement = c.prepare("SELECT pr_id,repository FROM pull_request_aliases")?;
    for pair in statement.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))? {
        let (pr, repository) = pair?;
        by_pr.entry(pr).or_default().insert(repository);
    }
    Ok(by_pr)
}

/// The stored row as the domain's pull request reads it.
pub(super) fn decode_row(row: &mut Value, aliases: &mut BTreeMap<String, BTreeSet<String>>) -> Result<()> {
    let o = row.as_object_mut().context("a pull request row is an object")?;
    let mut take = |name: &str| o.remove(name).unwrap_or(Value::Null);
    let (key, locator, reason, since) = (take("provider_key"), take("provider_locator"), take("unresolvable_reason"), take("unresolvable_since"));
    let identity = match take("identity").as_str() {
        Some("unverified") => json!({"kind": "unverified"}),
        Some("verified") => json!({"kind": "verified", "key": key, "locator": locator}),
        Some("unresolvable") if key.is_null() => json!({"kind": "unresolvable", "reason": reason, "since": since}),
        Some("unresolvable") => json!({"kind": "unresolvable", "key": key, "reason": reason, "since": since}),
        other => anyhow::bail!("unknown pull request identity {other:?}"),
    };
    let (by, at) = (take("superseded_by"), take("detached_at"));
    let tracking = match take("tracking").as_str() {
        Some("tracked") => json!({"kind": "tracked"}),
        Some("superseded") => json!({"kind": "superseded", "by": by}),
        Some("detached") => json!({"kind": "detached", "at": at}),
        other => anyhow::bail!("unknown pull request tracking {other:?}"),
    };
    let (strikes, last) = (take("missing_strikes"), take("missing_last"));
    let missing = if strikes.is_null() { Value::Null } else { json!({"strikes": strikes, "last": last}) };
    let id = s(&o["id"])?.to_owned();
    let repository = s(&o["repository"])?.to_owned();
    let mut known = aliases.remove(&id).unwrap_or_default();
    known.remove(&repository);
    o.insert("identity".into(), identity);
    o.insert("tracking".into(), tracking);
    o.insert("missing".into(), missing);
    o.insert("aliases".into(), json!(known));
    Ok(())
}

/// The decisions a record may lose: a copy that joined a record of its own task hands them all to
/// the one that stays.
pub(super) fn handed_over(after: &DomainState, id: &PrId, lost: &[PrDecision]) -> bool {
    let PrTracking::Superseded { by } = &after.pull_requests[id].tracking else { return false };
    let Some(kept) = after.pull_requests.get(by).and_then(|pr| pr.review.as_ref()) else { return false };
    lost.iter().all(|d| kept.decisions.contains(d))
}

/// The tasks whose metrics name a project by a pull request that moved or joined another: its own
/// task and those of every record that joined it.
pub(super) fn refresh_projects(c: &Connection, after: &DomainState, changed: &[(&PullRequest, Option<&PullRequest>)]) -> Result<()> {
    let moved: BTreeSet<&PrId> = changed.iter()
        .filter(|(p, prior)| prior.is_some_and(|b| b.repository != p.repository || b.tracking != p.tracking))
        .map(|(p, _)| after.surviving_pr(&p.id))
        .collect();
    let tasks: BTreeSet<&str> = after.pull_requests.values()
        .filter(|pr| moved.contains(after.surviving_pr(&pr.id)))
        .map(|pr| pr.task_id.0.as_str())
        .collect();
    for task in tasks {
        crate::metrics_fold::refresh_project(c, task)?;
    }
    Ok(())
}
