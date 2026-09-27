//! Borrowed overview projection. The writer rejects overflow before appending;
//! no partial JSON is returned and large source strings are never materialized as Value.
use super::*;
use serde::ser::{Error, Impossible, SerializeMap, SerializeSeq, SerializeStruct};
use serde::{Serialize, Serializer};
use std::io::Write;

pub(super) const MAX_SNAPSHOT: usize = 8 * 1024 * 1024;
// Domain grouping allocates reference/ID indices and can scan quadratically.
// Reject, never truncate, before those helpers. This counts only maps traversed
// by this projection, including retained history used by its grouping rules.
// An oversized history requires pagination even if its final JSON could be small.
// This is a work/scratch cardinality bound, NOT a total RSS guarantee.
const MAX_ENTITIES: usize = 16 * 1024;
fn ensure_cardinality(v: &View) -> anyhow::Result<()> {
    let counts = [
        v.nodes.len(),
        v.questions.len(),
        v.reports.len(),
        v.messages.len(),
        v.delivery_attempts.len(),
        v.prs.len(),
        v.board.requests.len(),
        v.board.tasks.len(),
    ];
    let mut remaining = MAX_ENTITIES;
    for count in counts {
        remaining = remaining.checked_sub(count).ok_or(OverviewTooLarge)?;
    }
    Ok(())
}
fn with_bounded_view<T>(
    v: &View,
    project: impl FnOnce() -> anyhow::Result<T>,
) -> anyhow::Result<T> {
    ensure_cardinality(v)?;
    project()
}
#[derive(Debug)]
pub(crate) struct OverviewTooLarge;
impl std::fmt::Display for OverviewTooLarge {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("overview snapshot exceeds limit")
    }
}
impl std::error::Error for OverviewTooLarge {}
struct Bounded {
    bytes: Vec<u8>,
    limit: usize,
    exceeded: bool,
}
impl Write for Bounded {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        if bytes.len() > self.limit.saturating_sub(self.bytes.len()) {
            self.exceeded = true;
            return Err(std::io::Error::other("overview snapshot exceeds limit"));
        }
        let needed = self.bytes.len() + bytes.len();
        if needed > self.bytes.capacity() {
            let capacity = needed.next_power_of_two().min(self.limit);
            self.bytes.reserve_exact(capacity - self.bytes.len());
        }
        self.bytes.extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}
fn encode(value: &impl Serialize, limit: usize) -> anyhow::Result<Vec<u8>> {
    let mut out = Bounded {
        bytes: Vec::new(),
        limit: limit.min(MAX_SNAPSHOT),
        exceeded: false,
    };
    let result = serde_json::to_writer(&mut out, value);
    if out.exceeded {
        return Err(OverviewTooLarge.into());
    }
    result?;
    Ok(out.bytes)
}
pub(super) fn capture(hub: &Hub, limit: usize) -> anyhow::Result<Vec<u8>> {
    // Check before auxiliary cache reads, then recheck the pinned View below:
    // another writer may have added entities while those reads ran.
    hub.read(ensure_cardinality)?;
    let now = now_millis();
    let machine = hub.machine();
    let usage = bounded_usage(&crate::watcher::claude_home())?;
    hub.read(|v| {
        with_bounded_view(v, || {
            encode(
                &Snapshot {
                    v,
                    hub,
                    now,
                    machine: &machine,
                    usage: &usage,
                },
                limit,
            )
        })
    })
}
// Usage is an auxiliary local cache. Retain its existing malformed/missing-file
// behavior, but reject oversized input instead of allocating an unbounded Value.
fn bounded_usage(home: &std::path::Path) -> anyhow::Result<Option<alondra_core::usage::Usage>> {
    use std::io::Read;
    use std::os::unix::fs::OpenOptionsExt;
    let read = |name: &str| -> anyhow::Result<Option<Value>> {
        // Keep legacy symlinks to regular caches working. O_NONBLOCK prevents
        // open() waiting for a FIFO writer; validate the opened FD, not a racy
        // path metadata check. Unsupported cache types remain optional/absent.
        let Ok(file) = std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NONBLOCK)
            .open(home.join(name))
        else {
            return Ok(None);
        };
        if !file.metadata().is_ok_and(|meta| meta.is_file()) {
            return Ok(None);
        }
        let mut bytes = Vec::new();
        if file
            .take(MAX_SNAPSHOT as u64 + 1)
            .read_to_end(&mut bytes)
            .is_err()
        {
            return Ok(None);
        }
        if bytes.len() > MAX_SNAPSHOT {
            return Err(OverviewTooLarge.into());
        }
        Ok(serde_json::from_slice(&bytes).ok())
    };
    let Some(state) = read(alondra_core::usage::STATE_FILE)? else {
        return Ok(None);
    };
    let daily = read(alondra_core::usage::DAILY_FILE)?.unwrap_or(Value::Null);
    let now: libc::time_t = unsafe { libc::time(std::ptr::null_mut()) };
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    unsafe { libc::localtime_r(&now, &mut tm) };
    let today = format!(
        "{:04}-{:02}-{:02}",
        tm.tm_year + 1900,
        tm.tm_mon + 1,
        tm.tm_mday
    );
    Ok(alondra_core::usage::compute(
        &state, &daily, now as i64, &today,
    ))
}
struct FreshSequence<F>(F);
impl<F, I> Serialize for FreshSequence<F>
where
    F: Fn() -> I,
    I: Iterator,
    I::Item: Serialize,
{
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        let mut seq = s.serialize_seq(None)?;
        for item in (self.0)() {
            seq.serialize_element(&item)?;
        }
        seq.end()
    }
}
struct Seq<I>(I);
impl<I: Iterator + Clone> Serialize for Seq<I>
where
    I::Item: Serialize,
{
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        let mut seq = s.serialize_seq(None)?;
        for item in self.0.clone() {
            seq.serialize_element(&item)?;
        }
        seq.end()
    }
}
// Serialize the *fields* of a derived struct directly into its augmented map.
// Filtering happens before serializing a field (not after cloning it to Value).
struct Fields<'a, M> {
    map: &'a mut M,
    skip: &'a [&'static str],
}
impl<M: SerializeMap> SerializeStruct for Fields<'_, M> {
    type Ok = ();
    type Error = M::Error;
    fn serialize_field<T: Serialize + ?Sized>(
        &mut self,
        key: &'static str,
        value: &T,
    ) -> Result<(), Self::Error> {
        if !self.skip.contains(&key) {
            self.map.serialize_entry(key, value)?;
        }
        Ok(())
    }
    fn end(self) -> Result<(), Self::Error> {
        Ok(())
    }
}
impl<'a, M: SerializeMap> Serializer for Fields<'a, M> {
    type Ok = ();
    type Error = M::Error;
    type SerializeStruct = Self;
    type SerializeSeq = Impossible<(), M::Error>;
    type SerializeTuple = Impossible<(), M::Error>;
    type SerializeTupleStruct = Impossible<(), M::Error>;
    type SerializeTupleVariant = Impossible<(), M::Error>;
    type SerializeMap = Impossible<(), M::Error>;
    type SerializeStructVariant = Impossible<(), M::Error>;
    fn serialize_struct(self, _: &'static str, _: usize) -> Result<Self, Self::Error> {
        Ok(self)
    }
    fn serialize_bool(self, _: bool) -> Result<(), Self::Error> {
        Err(Error::custom("expected overview struct"))
    }
    fn serialize_i8(self, _: i8) -> Result<(), Self::Error> {
        Err(Error::custom("expected overview struct"))
    }
    fn serialize_i16(self, _: i16) -> Result<(), Self::Error> {
        Err(Error::custom("expected overview struct"))
    }
    fn serialize_i32(self, _: i32) -> Result<(), Self::Error> {
        Err(Error::custom("expected overview struct"))
    }
    fn serialize_i64(self, _: i64) -> Result<(), Self::Error> {
        Err(Error::custom("expected overview struct"))
    }
    fn serialize_u8(self, _: u8) -> Result<(), Self::Error> {
        Err(Error::custom("expected overview struct"))
    }
    fn serialize_u16(self, _: u16) -> Result<(), Self::Error> {
        Err(Error::custom("expected overview struct"))
    }
    fn serialize_u32(self, _: u32) -> Result<(), Self::Error> {
        Err(Error::custom("expected overview struct"))
    }
    fn serialize_u64(self, _: u64) -> Result<(), Self::Error> {
        Err(Error::custom("expected overview struct"))
    }
    fn serialize_f32(self, _: f32) -> Result<(), Self::Error> {
        Err(Error::custom("expected overview struct"))
    }
    fn serialize_f64(self, _: f64) -> Result<(), Self::Error> {
        Err(Error::custom("expected overview struct"))
    }
    fn serialize_char(self, _: char) -> Result<(), Self::Error> {
        Err(Error::custom("expected overview struct"))
    }
    fn serialize_str(self, _: &str) -> Result<(), Self::Error> {
        Err(Error::custom("expected overview struct"))
    }
    fn serialize_bytes(self, _: &[u8]) -> Result<(), Self::Error> {
        Err(Error::custom("expected overview struct"))
    }
    fn serialize_none(self) -> Result<(), Self::Error> {
        Err(Error::custom("expected overview struct"))
    }
    fn serialize_unit(self) -> Result<(), Self::Error> {
        Err(Error::custom("expected overview struct"))
    }
    fn serialize_unit_struct(self, _: &'static str) -> Result<(), Self::Error> {
        Err(Error::custom("expected overview struct"))
    }
    fn serialize_unit_variant(
        self,
        _: &'static str,
        _: u32,
        _: &'static str,
    ) -> Result<(), Self::Error> {
        Err(Error::custom("expected overview struct"))
    }
    fn serialize_some<T: Serialize + ?Sized>(self, _: &T) -> Result<(), Self::Error> {
        Err(Error::custom("expected overview struct"))
    }
    fn serialize_newtype_struct<T: Serialize + ?Sized>(
        self,
        _: &'static str,
        _: &T,
    ) -> Result<(), Self::Error> {
        Err(Error::custom("expected overview struct"))
    }
    fn serialize_newtype_variant<T: Serialize + ?Sized>(
        self,
        _: &'static str,
        _: u32,
        _: &'static str,
        _: &T,
    ) -> Result<(), Self::Error> {
        Err(Error::custom("expected overview struct"))
    }
    fn serialize_seq(self, _: Option<usize>) -> Result<Self::SerializeSeq, Self::Error> {
        Err(Error::custom("expected overview struct"))
    }
    fn serialize_tuple(self, _: usize) -> Result<Self::SerializeTuple, Self::Error> {
        Err(Error::custom("expected overview struct"))
    }
    fn serialize_tuple_struct(
        self,
        _: &'static str,
        _: usize,
    ) -> Result<Self::SerializeTupleStruct, Self::Error> {
        Err(Error::custom("expected overview struct"))
    }
    fn serialize_tuple_variant(
        self,
        _: &'static str,
        _: u32,
        _: &'static str,
        _: usize,
    ) -> Result<Self::SerializeTupleVariant, Self::Error> {
        Err(Error::custom("expected overview struct"))
    }
    fn serialize_map(self, _: Option<usize>) -> Result<Self::SerializeMap, Self::Error> {
        Err(Error::custom("expected overview struct"))
    }
    fn serialize_struct_variant(
        self,
        _: &'static str,
        _: u32,
        _: &'static str,
        _: usize,
    ) -> Result<Self::SerializeStructVariant, Self::Error> {
        Err(Error::custom("expected overview struct"))
    }
}
fn fields<M: SerializeMap, T: Serialize>(
    map: &mut M,
    value: &T,
    skip: &[&'static str],
) -> Result<(), M::Error> {
    value.serialize(Fields { map, skip })
}
#[derive(Clone, Copy)]
enum Item<'a> {
    Report(&'a View, &'a Report, bool),
    Question(&'a View, &'a Question, bool),
    Request(&'a View, &'a alondra_core::tasks::Request),
    Task(&'a View, &'a alondra_core::tasks::Task, i64),
    Node(&'a View, &'a alondra_core::Node, i64),
    Pr(&'a View, &'a alondra_core::pr::PullRequest),
    Message(&'a View, &'a Message),
    Orchestrator(&'a View),
}
impl Serialize for Item<'_> {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        // Null is distinct from an empty runtime object.
        if let Self::Orchestrator(v) = self {
            if v.orchestrator.is_none() && v.orchestrator_migration_error.is_none() {
                return s.serialize_none();
            }
        }
        let mut m = s.serialize_map(None)?;
        match *self {
            Self::Report(v, r, news) => {
                fields(&mut m, r, &["task"])?;
                m.serialize_entry("task", &v.report_task(r).map(|t| t.id.as_str()))?;
                if news {
                    if let Some(pr) = v.report_pr(r) {
                        m.serialize_entry("pr_id", &pr)?;
                    }
                    let ids = v.report_stack(r);
                    if !ids.is_empty() {
                        m.serialize_entry(
                            "earlier",
                            &Seq(ids
                                .iter()
                                .filter_map(|id| v.reports.get(id))
                                .map(|r| Item::Report(v, r, false))),
                        )?;
                    }
                }
            }
            Self::Question(v, q, open) => {
                fields(&mut m, q, &["task"])?;
                m.serialize_entry("task", &v.question_task(q).map(|t| t.id.as_str()))?;
                if open {
                    if let Some(pr) = v.question_pr(q) {
                        m.serialize_entry("pr_id", &pr)?;
                    }
                    let ids = v.question_reports(q);
                    if let Some(last) = ids.iter().rev().find_map(|id| v.reports.get(id)) {
                        m.serialize_entry("report", &Item::Report(v, last, false))?;
                        m.serialize_entry(
                            "reports",
                            &Seq(ids
                                .iter()
                                .filter_map(|id| v.reports.get(id))
                                .map(|r| Item::Report(v, r, false))),
                        )?;
                    }
                }
            }
            Self::Request(v, r) => {
                fields(&mut m, r, &["title", "archived_at"])?;
                m.serialize_entry("state", &v.board.request_state(&r.id))?;
                m.serialize_entry("archived_at", &v.board.request_archived_at(&r.id))?;
                let tasks = v.board.tasks.values().filter(|t| t.request == r.id);
                m.serialize_entry("tasks", &Seq(tasks.clone().map(|t| t.id.as_str())))?;
                let mut it = tasks;
                let first = it.next();
                let only = if it.next().is_none() {
                    first.map(|t| t.title.as_str())
                } else {
                    None
                };
                m.serialize_entry("title", &r.title.as_deref().or(only))?;
            }
            Self::Task(v, t, now) => {
                fields(&mut m, t, &["brief", "history", "anomalies"])?;
                m.serialize_entry("auto_merge_pending", &v.auto_merge_pending(t, now))?;
                m.serialize_entry("last_change", &t.last_change())?;
            }
            Self::Node(v, n, now) => {
                fields(&mut m, n, &["alive", "ledger", "background"])?;
                if n.is_orchestrator {
                    m.serialize_entry("orchestrator_runtime", &Item::Orchestrator(v))?;
                }
                m.serialize_entry(
                    "background",
                    &Seq(n.background.iter().map(|item| Background(n, item, now))),
                )?;
                let project = if n.is_orchestrator {
                    "orchestrator"
                } else {
                    n.cwd
                        .as_deref()
                        .and_then(|cwd| cwd.split("/Projects/").nth(1))
                        .and_then(|rest| rest.split('/').next())
                        .filter(|s| !s.is_empty())
                        .or_else(|| {
                            n.name
                                .as_deref()
                                .and_then(|name| name.split('/').next())
                                .filter(|s| !s.is_empty())
                        })
                        .unwrap_or("others")
                };
                m.serialize_entry("project", project)?;
                m.serialize_entry(
                    "messages_held_for_turn_end",
                    &v.held_for_turn_end(&n.session_id),
                )?;
            }
            Self::Pr(v, p) => {
                fields(&mut m, p, &["task"])?;
                let place = v.stack_place(p);
                m.serialize_entry("preferred_method", &v.merge_methods.get(&p.repo))?;
                m.serialize_entry("below", &place.below.map(|p| p.id))?;
                m.serialize_entry("below_number", &place.below.map(|p| p.number))?;
                m.serialize_entry(
                    "below_set_aside",
                    &place
                        .below
                        .is_some_and(|p| p.state == alondra_core::pr::PrState::Dismissed),
                )?;
                m.serialize_entry("stack_bottom", &place.bottom.id)?;
                m.serialize_entry("stack_opened_at", &place.bottom.opened_at)?;
                m.serialize_entry("stack_position", &place.position)?;
                m.serialize_entry("task", &v.pr_task_of(p).map(|t| t.id.as_str()))?;
            }
            Self::Message(v, message) => {
                fields(&mut m, message, &[])?;
                if let Some(a) = v.message_attempt(message.id) {
                    m.serialize_entry("delivery_attempt", &a.id)?;
                    m.serialize_entry("delivery_state", &a.state)?;
                    m.serialize_entry("delivery_reason", &a.reason)?;
                    m.serialize_entry("delivery_version", &a.version)?;
                } else {
                    m.serialize_entry(
                        "delivery_state",
                        if message.handed_at.is_some() {
                            "handed"
                        } else {
                            "queued"
                        },
                    )?;
                }
            }
            Self::Orchestrator(v) => {
                if let Some(r) = &v.orchestrator {
                    m.serialize_entry("phase", &r.phase)?;
                    m.serialize_entry("last_error", &r.last_error)?;
                    m.serialize_entry("retry_at", &r.retry_at)?;
                    m.serialize_entry("updated_at", &r.updated_at)?;
                    m.serialize_entry(
                        "queue_depth",
                        &v.pending_messages(alondra_core::orchestrator::NODE_ID)
                            .count(),
                    )?;
                    m.serialize_entry(
                        "can_retry",
                        &(r.phase == alondra_core::orchestrator::Phase::Failed
                            && r.active_run.is_some()),
                    )?;
                    m.serialize_entry("can_cancel", &(r.is_active() && !r.cancel_requested))?;
                    m.serialize_entry("cancel_requested", &r.cancel_requested)?;
                    m.serialize_entry("active_run_id", &r.active_run.as_ref().map(|r| &r.id))?;
                } else {
                    m.serialize_entry("phase", "failed")?;
                    m.serialize_entry("last_error", &v.orchestrator_migration_error)?;
                    m.serialize_entry("retry_at", &Option::<i64>::None)?;
                    m.serialize_entry("updated_at", &0)?;
                    m.serialize_entry(
                        "queue_depth",
                        &v.nodes
                            .values()
                            .find(|n| n.is_orchestrator)
                            .map_or(0, |n| v.pending_messages(&n.session_id).count()),
                    )?;
                    m.serialize_entry("can_retry", &false)?;
                    m.serialize_entry("can_cancel", &false)?;
                    m.serialize_entry("cancel_requested", &false)?;
                    m.serialize_entry("active_run_id", &Option::<&str>::None)?;
                }
            }
        }
        m.end()
    }
}
// Borrow the label rather than cloning background vectors. This is the same
// predicate as states::background; equivalence tests cover both fresh and idle nodes.
struct Background<'a>(
    &'a alondra_core::Node,
    &'a alondra_core::BackgroundItem,
    i64,
);
impl Serialize for Background<'_> {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        let Self(n, item, now) = *self;
        let fresh = |at: Option<i64>, window: i64| at.is_some_and(|at| now - at <= window);
        let after_stop = |at: &i64| n.last_stop_at.is_none_or(|stop| *at > stop);
        let other = n
            .tool
            .as_ref()
            .is_some_and(|t| now - t.since <= states::call_limit(&t.name))
            || fresh(n.last_work_at, states::BACKGROUND_SUSPECT_MS)
            || fresh(
                n.handed_at.filter(after_stop),
                states::BACKGROUND_SUSPECT_MS,
            )
            || fresh(
                n.daemon_seen_at
                    .filter(|_| n.alive && n.daemon_tempo.as_deref() == Some("active"))
                    .filter(after_stop),
                states::DAEMON_FRESH_MS,
            );
        let mut m = s.serialize_map(None)?;
        fields(&mut m, item, &["suspect"])?;
        if !other && now - item.started_at > states::BACKGROUND_SUSPECT_MS {
            m.serialize_entry("suspect", &true)?;
        }
        m.end()
    }
}
struct Snapshot<'a, M, U> {
    v: &'a View,
    hub: &'a Hub,
    now: i64,
    machine: &'a M,
    usage: &'a U,
}
impl<M: Serialize, U: Serialize> Serialize for Snapshot<'_, M, U> {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        let v = self.v;
        let mut m = s.serialize_map(None)?;
        m.serialize_entry("tick", &self.hub.tick())?;
        m.serialize_entry("seq", &v.seq)?;
        let mut nodes: Vec<_> = v.nodes.values().collect();
        nodes.sort_by(|a, b| {
            (!a.is_orchestrator, a.created_at, &a.session_id).cmp(&(
                !b.is_orchestrator,
                b.created_at,
                &b.session_id,
            ))
        });
        m.serialize_entry(
            "nodes",
            &Seq(nodes.iter().map(|n| Item::Node(v, n, self.now))),
        )?;
        m.serialize_entry(
            "open_questions",
            &Seq(v
                .questions
                .values()
                .filter(|q| q.status.is_open() && !q.to_orchestrator)
                .map(|q| Item::Question(v, q, true))),
        )?;
        m.serialize_entry(
            "news",
            &FreshSequence(|| v.inbox_reports().map(|r| Item::Report(v, r, true))),
        )?;
        let orchestrator = v.nodes.values().find(|n| n.is_orchestrator);
        m.serialize_entry(
            "orchestrator_messages",
            &Seq(v
                .messages
                .values()
                .rev()
                .filter(|msg| orchestrator.is_some_and(|n| n.session_id == msg.session_id))
                .take(20)
                .map(|msg| Item::Message(v, msg))),
        )?;
        m.serialize_entry("orchestrator", &Item::Orchestrator(v))?;
        m.serialize_entry("delivery_attempts", &Seq(v.delivery_attempts.values()))?;
        m.serialize_entry(
            "recent_questions",
            &Seq(v
                .questions
                .values()
                .rev()
                .filter(|q| !q.status.is_open() && !v.left_out.contains(&q.session_id))
                .take(20)
                .map(|q| Item::Question(v, q, false))),
        )?;
        m.serialize_entry(
            "reports",
            &Seq(v
                .reports
                .values()
                .rev()
                .filter(|r| !v.left_out.contains(&r.session_id))
                .take(50)
                .map(|r| Item::Report(v, r, false))),
        )?;
        m.serialize_entry("machine", self.machine)?;
        m.serialize_entry("usage", self.usage)?;
        let prs = v.in_train_order(crate::prs::in_inbox(v));
        m.serialize_entry("prs", &Seq(prs.iter().map(|p| Item::Pr(v, p))))?;
        m.serialize_entry(
            "requests",
            &Seq(v.board.requests.values().map(|r| Item::Request(v, r))),
        )?;
        m.serialize_entry(
            "tasks",
            &Seq(v.board.tasks.values().map(|t| Item::Task(v, t, self.now))),
        )?;
        m.serialize_entry("user", &self.hub.config.name)?;
        m.end()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn fixture() -> View {
        let mut v = View::default();
        let mut node = alondra_core::Node::new("node-a", 1);
        node.name = Some("Árbol/日本語".into());
        node.cwd = Some("/tmp/Projects/árbol/work".into());
        node.in_daemon = true;
        node.alive = true;
        node.background.push(alondra_core::BackgroundItem {
            kind: alondra_core::BackgroundKind::Other,
            label: "背景任务 \"quoted\"".into(),
            started_at: 1,
            suspect: false,
        });
        v.nodes.insert(node.session_id.clone(), node);
        let mut orchestrator = alondra_core::Node::new("orchestrator", 0);
        orchestrator.is_orchestrator = true;
        v.nodes
            .insert(orchestrator.session_id.clone(), orchestrator);
        v.orchestrator_migration_error = Some("synthetic migration ☃".into());
        for index in 0..3 {
            let id = format!("t-{index}");
            v.board.tasks.insert(id.clone(),serde_json::from_value(json!({"id":id,"request":"r-1","title":format!("任务 {index}"),"brief":"not in overview","state":"needs_you","state_since":1,"created_at":1,"assignee":{"session_id":"node-a"}})).unwrap());
        }
        v.board.requests.insert("r-1".into(),serde_json::from_value(json!({"id":"r-1","text":"Una petición ☃","created_at":1,"title":"Request title","attachments":[{"name":"note.txt","source_path":"/synthetic/source","stored_path":"/synthetic/note","bytes":3}]})).unwrap());
        for id in 1..=4 {
            v.reports.insert(id,serde_json::from_value(json!({"id":id,"session_id":"node-a","node_name":"Nodo","title":format!("Report {id}"),"body":"汉字🙂\\\"\n\u{0000}".repeat(100),"kind":"document","created_at":id,"task":if id<3 {"t-0"}else{"t-1"}})).unwrap());
        }
        for id in 1..=2 {
            v.questions.insert(id,serde_json::from_value(json!({"id":id,"session_id":"node-a","node_name":"Nodo","title":"Question ☃","detail":"full details","blocking":true,"origin":"ask","status":"open","asked_at":id,"task":"t-0","options":[{"label":"Sí"},{"label":"No"}]})).unwrap());
        }
        v.messages.insert(1,serde_json::from_value(json!({"id":1,"session_id":"orchestrator","from":"user","text":"全文🙂","created_at":1})).unwrap());
        let mut answered = v.questions[&1].clone();
        answered.id = 3;
        answered.status = QuestionStatus::Answered;
        answered.answer = Some(Answer {
            text: "回答".into(),
            option: None,
            by: None,
            device: None,
            at: 4,
        });
        v.questions.insert(3, answered);
        v.delivery_attempts.insert(1,serde_json::from_value(json!({"id":1,"session_id":"orchestrator","channel":"terminal","message_ids":[1],"state":"uncertain","version":2,"reason":"synthetic_failure","created_at":1,"updated_at":2,"resolved_from_version":null})).unwrap());
        v.apply(
            10,
            3,
            &Event::PrOpened {
                url: "https://github.com/example/repo/pull/1".into(),
                session_id: "node-a".into(),
                task: Some("t-2".into()),
            },
        );
        v
    }
    #[test]
    fn bounded_overview_matches_legacy_full_projection_and_exact_limits() {
        let hub = super::super::tests::temp_hub("overview-stream");
        let v = fixture();
        let machine = hub.machine();
        let usage = Some(alondra_core::usage::Usage::default());
        for now in [2, states::BACKGROUND_SUSPECT_MS + 10] {
            let snapshot = Snapshot {
                v: &v,
                hub: &hub,
                now,
                machine: &machine,
                usage: &usage,
            };
            let bytes = encode(&snapshot, MAX_SNAPSHOT).unwrap();
            let actual: Value = serde_json::from_slice(&bytes).unwrap();
            let expected = overview_value(&hub, &v, now, &machine, &usage);
            assert_eq!(
                actual, expected,
                "borrowed representation must preserve every field and item"
            );
            assert_eq!(actual["nodes"].as_array().unwrap().len(), 2);
            assert_eq!(actual["reports"].as_array().unwrap().len(), 4);
            assert_eq!(actual["open_questions"].as_array().unwrap().len(), 2);
            assert_eq!(actual["tasks"].as_array().unwrap().len(), 3);
            assert_eq!(actual["prs"].as_array().unwrap().len(), 1);
            assert_eq!(actual["recent_questions"].as_array().unwrap().len(), 1);
            assert_eq!(
                actual["orchestrator_messages"][0]["delivery_state"],
                "uncertain"
            );
            assert_eq!(actual["delivery_attempts"].as_array().unwrap().len(), 1);
            assert!(
                actual["open_questions"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|q| q["reports"].as_array().is_some_and(|rs| rs.len() == 2))
            );
            assert!(
                actual["news"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|r| r["earlier"].as_array().is_some_and(|rs| rs.len() == 1))
            );
            assert!(actual["tasks"][0].get("brief").is_none());
            assert_eq!(encode(&snapshot, bytes.len()).unwrap(), bytes);
            assert!(
                encode(&snapshot, bytes.len() - 1)
                    .unwrap_err()
                    .is::<OverviewTooLarge>()
            );
            assert!(encode(&snapshot, 0).unwrap_err().is::<OverviewTooLarge>());
        }
    }
    #[test]
    fn bounded_overview_refuses_large_visible_text_but_never_serializes_omitted_task_fields() {
        let hub = super::super::tests::temp_hub("overview-stream-large");
        let mut v = fixture();
        let machine = hub.machine();
        v.board.tasks.get_mut("t-0").unwrap().brief = Some("x".repeat(MAX_SNAPSHOT + 1));
        let bytes = encode(
            &Snapshot {
                v: &v,
                hub: &hub,
                now: 2,
                machine: &machine,
                usage: &None::<u8>,
            },
            MAX_SNAPSHOT,
        )
        .unwrap();
        assert!(
            !serde_json::from_slice::<Value>(&bytes).unwrap()["tasks"][0]
                .as_object()
                .unwrap()
                .contains_key("brief")
        );
        v.reports.get_mut(&4).unwrap().body = Some("🙂".repeat(MAX_SNAPSHOT / 4 + 1));
        assert!(
            encode(
                &Snapshot {
                    v: &v,
                    hub: &hub,
                    now: 2,
                    machine: &machine,
                    usage: &None::<u8>
                },
                MAX_SNAPSHOT
            )
            .unwrap_err()
            .is::<OverviewTooLarge>()
        );
    }
    #[test]
    fn bounded_writer_stops_before_observing_later_items() {
        use std::cell::Cell;
        struct Item<'a>(&'a Cell<usize>);
        impl Serialize for Item<'_> {
            fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
                self.0.set(self.0.get() + 1);
                s.serialize_str("雪雪雪雪")
            }
        }
        let calls = Cell::new(0);
        let sequence = Seq((0..10).map(|_| Item(&calls)));
        assert!(encode(&sequence, 8).unwrap_err().is::<OverviewTooLarge>());
        assert_eq!(
            calls.get(),
            1,
            "overflow must abort iteration, not serialize the rest into temporary Values"
        );
    }
    #[test]
    fn usage_cache_special_files_do_not_wait_and_regular_symlinks_still_work() {
        use std::os::unix::{
            ffi::OsStrExt,
            fs::{OpenOptionsExt, symlink},
        };
        use std::time::Duration;
        struct Directory(std::path::PathBuf);
        impl Drop for Directory {
            fn drop(&mut self) {
                let _ = std::fs::remove_dir_all(&self.0);
            }
        }
        let home = Directory(std::env::temp_dir().join(format!(
            "overview-usage-types-{}",
            crate::auth::random_hex(8)
        )));
        std::fs::create_dir(&home.0).unwrap();
        let state = home.0.join(alondra_core::usage::STATE_FILE);
        let daily = home.0.join(alondra_core::usage::DAILY_FILE);
        let regular = home.0.join("regular.json");
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        std::fs::write(
            &regular,
            serde_json::to_vec(
                &json!({"seven_day":{"used_percentage":12.5,"resets_at":now+86400}}),
            )
            .unwrap(),
        )
        .unwrap();
        symlink(&regular, &state).unwrap();
        assert_eq!(bounded_usage(&home.0).unwrap().unwrap().week_percent, 12.5);
        std::fs::remove_file(&state).unwrap();
        std::fs::create_dir(&state).unwrap();
        assert!(bounded_usage(&home.0).unwrap().is_none());
        std::fs::remove_dir(&state).unwrap();
        for (fifo, expect_usage) in [(&state, false), (&daily, true)] {
            if expect_usage {
                symlink(&regular, &state).unwrap();
            }
            let path = std::ffi::CString::new(fifo.as_os_str().as_bytes()).unwrap();
            assert_eq!(unsafe { libc::mkfifo(path.as_ptr(), 0o600) }, 0);
            let (tx, rx) = std::sync::mpsc::channel();
            let root = home.0.clone();
            let reader = std::thread::spawn(move || {
                let _ = tx.send(bounded_usage(&root));
            });
            let result = rx.recv_timeout(Duration::from_secs(2));
            if result.is_err() {
                // The pre-fix blocking open is released even on the negative
                // control, so a failed assertion leaves no blocked test worker.
                let writer = std::fs::OpenOptions::new()
                    .write(true)
                    .custom_flags(libc::O_NONBLOCK)
                    .open(fifo)
                    .unwrap();
                drop(writer);
            }
            reader.join().unwrap();
            assert_eq!(
                result
                    .expect("usage cache waited for a FIFO writer")
                    .unwrap()
                    .is_some(),
                expect_usage
            );
            std::fs::remove_file(fifo).unwrap();
        }
        std::fs::remove_file(&state).unwrap();
        let oversized = std::fs::File::create(&state).unwrap();
        oversized.set_len(MAX_SNAPSHOT as u64 + 1).unwrap();
        drop(oversized);
        assert!(bounded_usage(&home.0).unwrap_err().is::<OverviewTooLarge>());
    }
    #[test]
    fn overview_entity_budget_is_checked_before_projection_and_accepts_exact_boundary() {
        use std::cell::Cell;
        let mut v = View::default();
        let message: Message = serde_json::from_value(
            json!({"id":1,"session_id":"fixture","from":"user","text":"x","created_at":1}),
        )
        .unwrap();
        for id in 1..=MAX_ENTITIES as u64 {
            let mut m = message.clone();
            m.id = id;
            v.messages.insert(id, m);
        }
        // Retention tombstones and unrelated launch bookkeeping are not traversed.
        for id in 0..MAX_ENTITIES {
            v.removed.insert(format!("removed-{id}"), 0);
        }
        let projected = Cell::new(0);
        assert_eq!(
            with_bounded_view(&v, || {
                projected.set(projected.get() + 1);
                Ok(7)
            })
            .unwrap(),
            7
        );
        assert_eq!(projected.get(), 1);
        v.nodes
            .insert("extra".into(), alondra_core::Node::new("extra", 0));
        let error = with_bounded_view(&v, || {
            projected.set(projected.get() + 1);
            Ok(())
        })
        .unwrap_err();
        assert!(error.is::<OverviewTooLarge>());
        assert_eq!(
            projected.get(),
            1,
            "no snapshot helper or auxiliary work may run after cardinality rejection"
        );
        v.messages.remove(&1);
        assert!(
            with_bounded_view(&v, || Ok(())).is_ok(),
            "budget is the sum across consulted maps, not a per-map cap"
        );
    }
}
