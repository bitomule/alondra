//! Sectioned reads over the cached domain projection (docs/domain-v2-read-endpoints.md).
//! One snapshot per (data_epoch, revision) serves every page, detail and section
//! token; nothing here re-projects state or touches SQL per request.
use crate::domain_api::domain_error;
use crate::domain_runtime::DomainRuntime;
use alondra_core::domain::reads::*;
use alondra_core::domain::*;
use anyhow::Result;
use axum::Json;
use axum::response::{IntoResponse, Response};
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use serde::Serialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, Mutex};

pub(crate) const MAX_PAGE_BYTES: usize = 96 * 1024;
pub(crate) const MAX_ROW_BYTES: usize = 32 * 1024;
pub(crate) const MAX_SUMMARY_BYTES: usize = 16 * 1024;
const CHUNK_TEXT_BYTES: usize = 48 * 1024;
pub(crate) const MAX_CHUNK_ENCODED: usize = 64 * 1024;
const DEFAULT_LIMIT: usize = 100;
const MAX_LIMIT: usize = 200;
const TEXT_KEYS: [&str; 2] = ["body", "text"];

#[derive(Default)]
pub(crate) struct Cache(Mutex<Option<Arc<Snapshot>>>);

pub(crate) struct Snapshot {
    state: Arc<DomainState>,
    projection: DomainProjection,
    sections: BTreeMap<String, SectionInfo>,
    inbox_by_id: HashMap<String, usize>,
    interactions_ordered: Vec<usize>,
    interactions_by_task: HashMap<TaskId, Vec<usize>>,
    interactions_orchestrator: Vec<usize>,
    interactions_by_id: HashMap<InteractionId, usize>,
}

impl Cache {
    pub(crate) fn snapshot(&self, domain: &DomainRuntime) -> Result<Arc<Snapshot>> {
        let state = domain.state()?;
        let mut slot = self.0.lock().expect("domain reads cache poisoned");
        if let Some(current) = slot.as_ref() {
            if current.state.data_epoch == state.data_epoch && current.state.revision == state.revision {
                return Ok(current.clone());
            }
        }
        let snapshot = Arc::new(Snapshot::build(state));
        *slot = Some(snapshot.clone());
        Ok(snapshot)
    }
}

fn token<T: Serialize>(value: &T) -> String {
    let digest = Sha256::digest(serde_json::to_vec(value).expect("section serialization"));
    digest[..8].iter().map(|b| format!("{b:02x}")).collect()
}

fn ordered<'a>(items: impl Iterator<Item = (usize, &'a Interaction)>) -> Vec<usize> {
    let mut rows: Vec<(Revision, &InteractionId, usize)> = items.map(|(i, x)| (x.order, &x.id, i)).collect();
    rows.sort();
    rows.into_iter().map(|(_, _, i)| i).collect()
}

impl Snapshot {
    fn build(state: Arc<DomainState>) -> Self {
        let projection = state.projection();
        let mut inbox_by_id = HashMap::new();
        for (r, root) in projection.inbox.iter().enumerate() {
            inbox_by_id.insert(root.id.clone(), r);
        }
        let mut by_task: HashMap<TaskId, Vec<(usize, &Interaction)>> = HashMap::new();
        let mut orchestrator = vec![];
        let mut by_id = HashMap::new();
        for (i, x) in projection.interactions.iter().enumerate() {
            by_id.insert(x.id.clone(), i);
            match &x.scope {
                Scope::Task { task_id } => by_task.entry(task_id.clone()).or_default().push((i, x)),
                Scope::Orchestrator { .. } => orchestrator.push((i, x)),
            }
        }
        let documents: Vec<DocumentBundle> = projection.documents.iter().map(|d| bundle(&projection, d)).collect();
        let mut sections = BTreeMap::new();
        let mut section = |name: &str, revision: String, count: usize| {
            sections.insert(name.to_owned(), SectionInfo { revision, count: count as u64 });
        };
        section("inbox", token(&projection.inbox), projection.inbox.len());
        section("tasks", token(&projection.tasks), projection.tasks.len());
        section("requests", token(&projection.requests), projection.requests.len());
        section("agents", token(&projection.agents), projection.agents.len());
        section("interactions", token(&projection.interactions), projection.interactions.len());
        section("pull_requests", token(&projection.pull_requests), projection.pull_requests.len());
        section("documents", token(&documents), documents.len());
        section("merge_authorizations", token(&projection.merge_authorizations), projection.merge_authorizations.len());
        section("stack_readiness", token(&projection.stack_readiness), projection.stack_readiness.len());
        section("superseded_acceptances", token(&projection.superseded_acceptances), projection.superseded_acceptances.len());
        section("schedules", token(&state.schedule), state.schedule.schedules.len());
        Self {
            interactions_ordered: ordered(projection.interactions.iter().enumerate()),
            interactions_by_task: by_task.into_iter().map(|(t, rows)| (t, ordered(rows.into_iter()))).collect(),
            interactions_orchestrator: ordered(orchestrator.into_iter()),
            interactions_by_id: by_id,
            inbox_by_id,
            sections,
            projection,
            state,
        }
    }
    pub(crate) fn state(&self) -> &DomainState {
        &self.state
    }
    /// Each inbox row with the revision that put it there.
    pub(crate) fn inbox_orders(&self) -> Vec<(String, Revision)> {
        self.inbox_by_id.iter().map(|(id, item)| (id.clone(), self.projection.inbox[*item].order)).collect()
    }
    pub(crate) fn section_tokens(&self) -> BTreeMap<String, String> {
        self.sections.iter().map(|(k, v)| (k.clone(), v.revision.clone())).collect()
    }
    pub(crate) fn envelope_ids(&self) -> (DataEpoch, Revision) {
        (self.state.data_epoch.clone(), self.state.revision)
    }
    fn envelope(&self) -> (u32, DataEpoch, Revision) {
        (PROTOCOL_VERSION, self.state.data_epoch.clone(), self.state.revision)
    }
}

fn bundle(projection: &DomainProjection, document: &Document) -> DocumentBundle {
    DocumentBundle {
        document: document.clone(),
        revisions: projection.document_revisions.iter().filter(|r| r.document_id == document.id).cloned().collect(),
        reviews: projection.document_reviews.iter().filter(|r| r.document_id == document.id).cloned().collect(),
    }
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Key {
    Ordered(Revision, String),
    Id(String),
}
impl Key {
    fn encode(&self) -> String {
        let value = match self {
            Key::Ordered(order, id) => json!({"o": order, "i": id}),
            Key::Id(id) => json!({"i": id}),
        };
        URL_SAFE_NO_PAD.encode(serde_json::to_vec(&value).expect("cursor"))
    }
    fn decode(cursor: &str, ordered: bool) -> Option<Self> {
        let bytes = URL_SAFE_NO_PAD.decode(cursor).ok()?;
        let value: Value = serde_json::from_slice(&bytes).ok()?;
        let object = value.as_object()?;
        let id = object.get("i")?.as_str()?.to_owned();
        match (ordered, object.len(), object.get("o")) {
            (true, 2, Some(order)) => Some(Key::Ordered(order.as_u64()?, id)),
            (false, 1, None) => Some(Key::Id(id)),
            _ => None,
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Query {
    cursor: Option<String>,
    limit: usize,
    pending_only: bool,
    request: Option<String>,
    agent: Option<String>,
    orchestrator: bool,
    include: Vec<String>,
    offset: Option<u64>,
}

fn is_id(id: &str) -> bool {
    !id.is_empty() && id.len() <= 256 && id.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_'))
}
/// A PR id is the registered `host/owner/repo#number`, so it travels percent-encoded.
pub(crate) fn is_pull_request_id(id: &str) -> bool {
    !id.is_empty() && id.len() <= 256
        && id.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'/' | b'#'))
}

pub(crate) fn encoded_pull_request_id(raw: &str) -> bool {
    decoded_pull_request_id(raw).is_some()
}

pub(crate) fn decoded_pull_request_id(raw: &str) -> Option<String> {
    let id = decode_segment(raw)?;
    (alondra_core::views::id::valid(&id) && alondra_core::views::id::path_segment(&id) == raw)
        .then_some(id)
}

fn is_inbox_id(id: &str) -> bool {
    !id.is_empty() && id.len() <= 512 && id.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b':'))
}

/// Path segments are percent-encoded (RFC 3986 §2.1): `interaction%3Aq` names `interaction:q`.
/// The decoded id still has to pass the id grammar, so `%2F` or `%25` never widen it.
fn decode_segment(raw: &str) -> Option<String> {
    let bytes = raw.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' {
            let pair = bytes.get(index + 1..index + 3)?;
            if !pair.iter().all(u8::is_ascii_hexdigit) {
                return None;
            }
            decoded.push(u8::from_str_radix(std::str::from_utf8(pair).ok()?, 16).ok()?);
            index += 3;
        } else {
            decoded.push(bytes[index]);
            index += 1;
        }
    }
    String::from_utf8(decoded).ok()
}

fn parse_query(raw: Option<&str>, allowed: &[&str]) -> Option<Query> {
    let mut query = Query { limit: DEFAULT_LIMIT, ..Query::default() };
    let Some(raw) = raw else { return Some(query) };
    let mut seen = Vec::new();
    for field in raw.split('&') {
        let (key, value) = field.split_once('=')?;
        if value.is_empty() || !allowed.contains(&key) || seen.contains(&key) {
            return None;
        }
        seen.push(key);
        match key {
            "cursor" if value.len() <= 512 && value.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_')) => {
                query.cursor = Some(value.to_owned());
            }
            "limit" if value.len() <= 3 && value.bytes().all(|b| b.is_ascii_digit()) => {
                let limit: usize = value.parse().ok()?;
                if !(1..=MAX_LIMIT).contains(&limit) {
                    return None;
                }
                query.limit = limit;
            }
            "filter" if value == "pending" => query.pending_only = true,
            "request" if is_id(value) => query.request = Some(value.to_owned()),
            "agent" if is_id(value) => query.agent = Some(value.to_owned()),
            "scope" if value == "orchestrator" => query.orchestrator = true,
            "include" => {
                for name in value.split(',') {
                    if name == "schedules" || !SECTIONS.contains(&name) || query.include.iter().any(|n| n == name) {
                        return None;
                    }
                    query.include.push(name.to_owned());
                }
            }
            "offset" if value.len() <= 19 && value.bytes().all(|b| b.is_ascii_digit()) => {
                query.offset = Some(value.parse().ok()?);
            }
            _ => return None,
        }
    }
    Some(query)
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Route {
    Summary(Query),
    Inbox(Query),
    InboxItem(String),
    Tasks(Query),
    Task(String),
    TaskInteractions(String, Query),
    Interactions(Query),
    Interaction(String),
    InteractionBody(String, u64),
    Requests(Query),
    Request(String),
    RequestBody(String, u64),
    Agents(Query),
    PullRequests(Query),
    PullRequest(String),
    Documents(Query),
    Document(String),
    MergeAuthorizations(Query),
    StackReadiness(Query),
    SupersededAcceptances(Query),
}

const PAGE: &[&str] = &["cursor", "limit"];

/// The one grammar shared by the local router and the relay allowlist.
pub(crate) fn parse(path: &str) -> Option<Route> {
    if path.len() > 4096 {
        return None;
    }
    let (path, query) = match path.split_once('?') {
        Some((path, query)) => (path, Some(query)),
        None => (path, None),
    };
    let rest = path.strip_prefix("/")?;
    let mut segments = rest.split('/');
    let (head, first, second, more) = (segments.next()?, segments.next(), segments.next(), segments.next());
    if more.is_some() {
        return None;
    }
    let first = match first {
        Some(raw) => Some(decode_segment(raw)?),
        None => None,
    };
    let first = first.as_deref();
    let body_offset = |query: Option<&str>| -> Option<u64> {
        let q = parse_query(query, &["offset"])?;
        q.offset
    };
    Some(match (head, first, second) {
        ("summary", None, None) => Route::Summary(parse_query(query, &["include"])?),
        ("inbox", None, None) => Route::Inbox(parse_query(query, &["cursor", "limit", "filter"])?),
        ("inbox", Some(id), None) if is_inbox_id(id) && query.is_none() => Route::InboxItem(id.to_owned()),
        ("tasks", None, None) => Route::Tasks(parse_query(query, &["cursor", "limit", "request", "agent"])?),
        ("tasks", Some(id), None) if is_id(id) && query.is_none() => Route::Task(id.to_owned()),
        ("tasks", Some(id), Some("interactions")) if is_id(id) => Route::TaskInteractions(id.to_owned(), parse_query(query, PAGE)?),
        ("interactions", None, None) => {
            let q = parse_query(query, &["scope", "cursor", "limit"])?;
            if !q.orchestrator {
                return None;
            }
            Route::Interactions(q)
        }
        ("interactions", Some(id), None) if is_id(id) && query.is_none() => Route::Interaction(id.to_owned()),
        ("interactions", Some(id), Some("body")) if is_id(id) => Route::InteractionBody(id.to_owned(), body_offset(query)?),
        ("requests", None, None) => Route::Requests(parse_query(query, PAGE)?),
        ("requests", Some(id), None) if is_id(id) && query.is_none() => Route::Request(id.to_owned()),
        ("requests", Some(id), Some("body")) if is_id(id) => Route::RequestBody(id.to_owned(), body_offset(query)?),
        ("agents", None, None) => Route::Agents(parse_query(query, PAGE)?),
        ("pull-requests", None, None) => Route::PullRequests(parse_query(query, PAGE)?),
        ("pull-requests", Some(id), None) if is_pull_request_id(id) && query.is_none() => Route::PullRequest(id.to_owned()),
        ("documents", None, None) => Route::Documents(parse_query(query, PAGE)?),
        ("documents", Some(id), None) if is_id(id) && query.is_none() => Route::Document(id.to_owned()),
        ("merge-authorizations", None, None) => Route::MergeAuthorizations(parse_query(query, PAGE)?),
        ("stack-readiness", None, None) => Route::StackReadiness(parse_query(query, PAGE)?),
        ("superseded-acceptances", None, None) => Route::SupersededAcceptances(parse_query(query, PAGE)?),
        _ => return None,
    })
}

pub(crate) fn approved_read_path(path: &str) -> bool {
    parse(path).is_some()
}

fn encoded_len(value: &Value) -> usize {
    serde_json::to_vec(value).map_or(usize::MAX, |bytes| bytes.len())
}

fn longest_text<'a>(value: &'a mut Value, in_text_key: bool) -> Option<&'a mut String> {
    match value {
        Value::String(text) if in_text_key => Some(text),
        Value::Array(items) => items.iter_mut().filter_map(|item| longest_text(item, false)).max_by_key(|s| s.len()),
        Value::Object(fields) => fields
            .iter_mut()
            .filter_map(|(key, item)| longest_text(item, TEXT_KEYS.contains(&key.as_str())))
            .max_by_key(|s| s.len()),
        _ => None,
    }
}

/// Cuts text fields (largest first) until the row fits. Returns whether anything was cut.
fn fit(value: &mut Value, budget: usize) -> bool {
    let mut cut = false;
    while encoded_len(value) > budget {
        let Some(text) = longest_text(value, false) else { break };
        if text.is_empty() {
            break;
        }
        let mut end = text.len() / 2;
        while end > 0 && !text.is_char_boundary(end) {
            end -= 1;
        }
        text.truncate(end);
        cut = true;
    }
    cut
}

struct Row {
    key: Key,
    id: String,
    value: Value,
}

fn after(cursor: &Option<String>, ordered: bool) -> Result<Option<Key>, Response> {
    match cursor {
        None => Ok(None),
        Some(cursor) => Key::decode(cursor, ordered)
            .map(Some)
            .ok_or_else(|| domain_error(DomainError::new("invalid_input", "cursor is invalid"))),
    }
}

fn paginate(snapshot: &Snapshot, section: &str, total: usize, rows: impl Iterator<Item = Row>, limit: usize, budget: usize) -> Page<Value> {
    let (protocol_version, data_epoch, revision) = snapshot.envelope();
    let mut page = Page {
        protocol_version,
        data_epoch,
        revision,
        section: section.to_owned(),
        section_revision: snapshot.sections[section].revision.clone(),
        total: total as u64,
        items: vec![],
        next_cursor: None,
        truncated: vec![],
    };
    let mut rows = rows.peekable();
    let mut used = encoded_len(&serde_json::to_value(&page).expect("page")) + 600;
    while let Some(row) = rows.peek_mut() {
        if page.items.len() >= limit {
            break;
        }
        let cut = fit(&mut row.value, MAX_ROW_BYTES);
        let cost = encoded_len(&row.value) + 1 + if cut { row.id.len() + 4 } else { 0 };
        if !page.items.is_empty() && used + cost > budget {
            break;
        }
        let row = rows.next().expect("peeked row");
        used += cost;
        if cut {
            page.truncated.push(row.id.clone());
        }
        page.items.push(row.value);
        page.next_cursor = Some(row.key.encode());
    }
    if rows.peek().is_none() {
        page.next_cursor = None;
    }
    page
}

fn to_value<T: Serialize>(value: &T) -> Value {
    serde_json::to_value(value).expect("row serialization")
}

fn id_rows<'a, T: Serialize + 'a>(items: impl Iterator<Item = (&'a str, &'a T)> + 'a, start: Option<Key>) -> impl Iterator<Item = Row> + 'a {
    items
        .map(|(id, item)| (Key::Id(id.to_owned()), id.to_owned(), item))
        .skip_while(move |(key, _, _)| start.as_ref().is_some_and(|s| key <= s))
        .map(|(key, id, item)| Row { key, id, value: to_value(item) })
}

fn interaction_rows<'a>(snapshot: &'a Snapshot, indices: &'a [usize], start: Option<Key>) -> impl Iterator<Item = Row> + 'a {
    indices
        .iter()
        .map(move |&i| &snapshot.projection.interactions[i])
        .map(|x| (Key::Ordered(x.order, x.id.0.clone()), x))
        .skip_while(move |(key, _)| start.as_ref().is_some_and(|s| key <= s))
        .map(|(key, x)| Row { key, id: x.id.0.clone(), value: to_value(x) })
}

fn inbox_rows<'a>(snapshot: &'a Snapshot, pending_only: bool, start: Option<Key>) -> impl Iterator<Item = Row> + 'a {
    let mut chronological: Vec<_> = snapshot
        .projection
        .inbox
        .iter()
        .filter(move |root| !pending_only || root.pending)
        .map(|root| (Key::Ordered(root.order, root.id.clone()), root))
        .collect();
    chronological.sort_by(|(a, _), (b, _)| a.cmp(b));
    chronological
        .into_iter()
        .skip_while(move |(key, _)| start.as_ref().is_some_and(|s| key <= s))
        .map(|(key, root)| Row { key, id: root.id.clone(), value: to_value(root) })
}

fn not_found(what: &str) -> Response {
    domain_error(DomainError::new("not_found", format!("{what} does not exist")))
}

fn page_response(page: Page<Value>) -> Response {
    Json(page).into_response()
}

fn inbox_page(snapshot: &Snapshot, q: &Query, budget: usize) -> Result<Page<Value>, Response> {
    let start = after(&q.cursor, true)?;
    let total = snapshot.projection.inbox.iter().filter(|r| !q.pending_only || r.pending).count();
    Ok(paginate(snapshot, "inbox", total, inbox_rows(snapshot, q.pending_only, start), q.limit, budget))
}

fn tasks_page(snapshot: &Snapshot, q: &Query, budget: usize) -> Result<Page<Value>, Response> {
    let start = after(&q.cursor, false)?;
    let matches = |t: &&Task| {
        q.request.as_ref().is_none_or(|r| &t.request_id.0 == r) && q.agent.as_ref().is_none_or(|a| t.agent_id.as_ref().is_some_and(|id| &id.0 == a))
    };
    let total = snapshot.projection.tasks.iter().filter(matches).count();
    let rows = id_rows(snapshot.projection.tasks.iter().filter(matches).map(|t| (t.id.0.as_str(), t)), start);
    Ok(paginate(snapshot, "tasks", total, rows, q.limit, budget))
}

fn section_page<'a, T: Serialize + 'a>(
    snapshot: &'a Snapshot, section: &str, items: &'a [T], id: impl Fn(&'a T) -> &'a str + 'a, q: &Query, budget: usize,
) -> Result<Page<Value>, Response> {
    let start = after(&q.cursor, false)?;
    let rows = id_rows(items.iter().map(move |item| (id(item), item)), start);
    Ok(paginate(snapshot, section, items.len(), rows, q.limit, budget))
}

fn first_page(snapshot: &Snapshot, section: &str, budget: usize) -> Page<Value> {
    let q = Query { limit: DEFAULT_LIMIT, ..Query::default() };
    let p = &snapshot.projection;
    let page = match section {
        "inbox" => inbox_page(snapshot, &q, budget),
        "tasks" => tasks_page(snapshot, &q, budget),
        "requests" => section_page(snapshot, section, &p.requests, |r| r.id.0.as_str(), &q, budget),
        "agents" => section_page(snapshot, section, &p.agents, |a| a.id.0.as_str(), &q, budget),
        "interactions" => Ok(paginate(snapshot, section, p.interactions.len(), interaction_rows(snapshot, &snapshot.interactions_ordered, None), q.limit, budget)),
        "pull_requests" => section_page(snapshot, section, &p.pull_requests, |x| x.id.0.as_str(), &q, budget),
        "documents" => documents_page(snapshot, &q, budget),
        "merge_authorizations" => section_page(snapshot, section, &p.merge_authorizations, |x| x.id.0.as_str(), &q, budget),
        "stack_readiness" => section_page(snapshot, section, &p.stack_readiness, |x| x.task_id.0.as_str(), &q, budget),
        "superseded_acceptances" => section_page(snapshot, section, &p.superseded_acceptances, |x| x.task_id.0.as_str(), &q, budget),
        _ => unreachable!("include names are validated"),
    };
    page.unwrap_or_else(|_| unreachable!("a default query has no cursor"))
}

fn documents_page(snapshot: &Snapshot, q: &Query, budget: usize) -> Result<Page<Value>, Response> {
    let start = after(&q.cursor, false)?;
    let p = &snapshot.projection;
    let rows = p
        .documents
        .iter()
        .map(|d| (Key::Id(d.id.0.clone()), d))
        .skip_while(move |(key, _)| start.as_ref().is_some_and(|s| key <= s))
        .map(|(key, d)| Row { key, id: d.id.0.clone(), value: to_value(&bundle(p, d)) });
    Ok(paginate(snapshot, "documents", p.documents.len(), rows, q.limit, budget))
}

fn item_response<T: Serialize>(snapshot: &Snapshot, item: &T) -> Response {
    let (protocol_version, data_epoch, revision) = snapshot.envelope();
    let mut value = to_value(item);
    let truncated = fit(&mut value, MAX_PAGE_BYTES - 512);
    Json(Item { protocol_version, data_epoch, revision, item: value, truncated }).into_response()
}

fn text_chunk(snapshot: &Snapshot, id: &str, text: &str, offset: u64) -> Response {
    let total = text.len() as u64;
    if offset > total || !text.is_char_boundary(offset as usize) {
        return domain_error(DomainError::new("invalid_input", "offset is not a character boundary of the body"));
    }
    let start = offset as usize;
    let mut end = (start + CHUNK_TEXT_BYTES).min(text.len());
    while end > start && !text.is_char_boundary(end) {
        end -= 1;
    }
    let mut data = &text[start..end];
    while encoded_len(&Value::String(data.to_owned())) > MAX_CHUNK_ENCODED {
        let mut cut = data.len() * 3 / 4;
        while cut > 0 && !data.is_char_boundary(cut) {
            cut -= 1;
        }
        data = &data[..cut];
    }
    let (protocol_version, data_epoch, revision) = snapshot.envelope();
    Json(TextChunk {
        protocol_version,
        data_epoch,
        revision,
        id: id.to_owned(),
        offset,
        total_bytes: total,
        data: data.to_owned(),
        has_more: start + data.len() < text.len(),
    })
    .into_response()
}

fn summary(snapshot: &Snapshot, q: &Query) -> Response {
    let (protocol_version, data_epoch, revision) = snapshot.envelope();
    let p = &snapshot.projection;
    let mut summary = Summary {
        protocol_version,
        data_epoch,
        revision,
        counts: p.counts.clone(),
        open_tasks: p.tasks.iter().filter(|t| !t.state.terminal() && !t.archived).count() as u64,
        connected_agents: p.agents.iter().filter(|a| a.phase.live()).count() as u64,
        sections: snapshot.sections.clone(),
        expansions: BTreeMap::new(),
        pending: vec![],
    };
    let mut used = encoded_len(&to_value(&summary));
    for name in &q.include {
        let remaining = MAX_PAGE_BYTES.saturating_sub(used + name.len() + 8);
        let page = first_page(snapshot, name, remaining);
        let cost = encoded_len(&to_value(&page)) + name.len() + 8;
        if used + cost > MAX_PAGE_BYTES {
            summary.pending.push(name.clone());
            continue;
        }
        used += cost;
        summary.expansions.insert(name.clone(), page);
    }
    Json(summary).into_response()
}

fn task_detail(snapshot: &Snapshot, id: &str) -> Response {
    let p = &snapshot.projection;
    let Some(task) = p.tasks.iter().find(|t| t.id.0 == id) else { return not_found("task") };
    let Some(request) = p.requests.iter().find(|r| r.id == task.request_id) else { return not_found("task") };
    let (protocol_version, data_epoch, revision) = snapshot.envelope();
    let detail = TaskDetail {
        protocol_version,
        data_epoch,
        revision,
        task: task.clone(),
        request: request.clone(),
        agent: task.agent_id.as_ref().and_then(|a| p.agents.iter().find(|x| &x.id == a)).cloned(),
        pull_request: task.pr_id.as_ref().and_then(|pr| p.pull_requests.iter().find(|x| &x.id == pr)).cloned(),
        documents: p
            .documents
            .iter()
            .filter(|d| task.documents.contains_key(&d.id))
            .map(|d| {
                let mut b = bundle(p, d);
                b.reviews.retain(|r| r.task_id == task.id);
                b
            })
            .collect(),
        merge_authorizations: p.merge_authorizations.iter().filter(|a| a.task_id == task.id).cloned().collect(),
        stack_readiness: p.stack_readiness.iter().find(|s| s.task_id == task.id).cloned(),
        superseded_acceptance: p.superseded_acceptances.iter().find(|s| s.task_id == task.id).cloned(),
        actions: snapshot.state.task_actions(&task.id).unwrap_or_default(),
        interactions: snapshot.interactions_by_task.get(&task.id).map_or(0, |rows| rows.len() as u64),
    };
    let mut value = to_value(&detail);
    fit(&mut value, MAX_PAGE_BYTES);
    Json(value).into_response()
}

pub(crate) fn respond(domain: &DomainRuntime, route: &Route) -> Response {
    let snapshot = match domain.reads.snapshot(domain) {
        Ok(snapshot) => snapshot,
        Err(_) => return domain_error(DomainError::new("storage_unavailable", "domain transaction could not be completed")),
    };
    let s = &*snapshot;
    let p = &s.projection;
    let paged = |page: Result<Page<Value>, Response>| page.map_or_else(|error| error, page_response);
    match route {
        Route::Summary(q) => summary(s, q),
        Route::Inbox(q) => paged(inbox_page(s, q, MAX_PAGE_BYTES)),
        Route::InboxItem(id) => match s.inbox_by_id.get(id) {
            Some(&r) => {
                let item = &p.inbox[r];
                let (protocol_version, data_epoch, revision) = s.envelope();
                let mut value = to_value(&InboxLookup { protocol_version, data_epoch, revision, item: item.clone(), root_id: item.id.clone() });
                fit(&mut value, MAX_PAGE_BYTES);
                Json(value).into_response()
            }
            None => not_found("inbox item"),
        },
        Route::Tasks(q) => paged(tasks_page(s, q, MAX_PAGE_BYTES)),
        Route::Task(id) => task_detail(s, id),
        Route::TaskInteractions(id, q) => {
            if !p.tasks.iter().any(|t| t.id.0 == *id) {
                return not_found("task");
            }
            let indices = s.interactions_by_task.get(&TaskId(id.clone())).map_or(&[][..], Vec::as_slice);
            paged(after(&q.cursor, true).map(|start| paginate(s, "interactions", indices.len(), interaction_rows(s, indices, start), q.limit, MAX_PAGE_BYTES)))
        }
        Route::Interactions(q) => {
            let indices = &s.interactions_orchestrator;
            paged(after(&q.cursor, true).map(|start| paginate(s, "interactions", indices.len(), interaction_rows(s, indices, start), q.limit, MAX_PAGE_BYTES)))
        }
        Route::Interaction(id) => match s.interactions_by_id.get(&InteractionId(id.clone())) {
            Some(&i) => item_response(s, &p.interactions[i]),
            None => not_found("interaction"),
        },
        Route::InteractionBody(id, offset) => match s.interactions_by_id.get(&InteractionId(id.clone())) {
            Some(&i) => text_chunk(s, id, &p.interactions[i].body, *offset),
            None => not_found("interaction"),
        },
        Route::Requests(q) => paged(section_page(s, "requests", &p.requests, |r| r.id.0.as_str(), q, MAX_PAGE_BYTES)),
        Route::Request(id) => match p.requests.iter().find(|r| r.id.0 == *id) {
            Some(request) => item_response(s, request),
            None => not_found("request"),
        },
        Route::RequestBody(id, offset) => match p.requests.iter().find(|r| r.id.0 == *id) {
            Some(request) => text_chunk(s, id, &request.body, *offset),
            None => not_found("request"),
        },
        Route::Agents(q) => paged(section_page(s, "agents", &p.agents, |a| a.id.0.as_str(), q, MAX_PAGE_BYTES)),
        Route::PullRequests(q) => paged(section_page(s, "pull_requests", &p.pull_requests, |x| x.id.0.as_str(), q, MAX_PAGE_BYTES)),
        Route::PullRequest(id) => match p.pull_requests.iter().find(|x| x.id.0 == *id) {
            Some(pr) => {
                let (protocol_version, data_epoch, revision) = s.envelope();
                Json(PullRequestDetail {
                    protocol_version,
                    data_epoch,
                    revision,
                    pull_request: pr.clone(),
                    merge_authorizations: p.merge_authorizations.iter().filter(|a| a.pr_id == pr.id).cloned().collect(),
                    stack_readiness: p.stack_readiness.iter().filter(|x| x.pr_id == pr.id).cloned().collect(),
                })
                .into_response()
            }
            None => not_found("pull request"),
        },
        Route::Documents(q) => paged(documents_page(s, q, MAX_PAGE_BYTES)),
        Route::Document(id) => match p.documents.iter().find(|d| d.id.0 == *id) {
            Some(d) => item_response(s, &bundle(p, d)),
            None => not_found("document"),
        },
        Route::MergeAuthorizations(q) => paged(section_page(s, "merge_authorizations", &p.merge_authorizations, |x| x.id.0.as_str(), q, MAX_PAGE_BYTES)),
        Route::StackReadiness(q) => paged(section_page(s, "stack_readiness", &p.stack_readiness, |x| x.task_id.0.as_str(), q, MAX_PAGE_BYTES)),
        Route::SupersededAcceptances(q) => paged(section_page(s, "superseded_acceptances", &p.superseded_acceptances, |x| x.task_id.0.as_str(), q, MAX_PAGE_BYTES)),
    }
}

#[cfg(test)]
mod tests;
