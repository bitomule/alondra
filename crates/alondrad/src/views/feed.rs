use alondra_core::domain::DomainError;
use alondra_core::views::{Client, Lang, Transport, VIEWS_PROTOCOL};
use axum::{
    Json,
    response::{IntoResponse, Response},
};
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use serde::Serialize;
use sha2::Digest;
use std::{
    collections::HashMap,
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};
use tokio::sync::{oneshot, watch};

pub(crate) struct Feeds {
    waiters: Mutex<HashMap<String, (u64, oneshot::Sender<()>)>>,
    cache: Mutex<HashMap<String, Current>>,
    open: Mutex<HashMap<String, (String, Instant)>>,
    next: AtomicU64,
    clock: watch::Sender<u64>,
}

impl Default for Feeds {
    fn default() -> Self {
        let (clock, _) = watch::channel(0);
        Self { waiters: Mutex::default(), cache: Mutex::default(), open: Mutex::default(), next: AtomicU64::new(0), clock }
    }
}

impl Feeds {
    pub(crate) fn wake(&self) {
        let next = self.next.fetch_add(1, Ordering::Relaxed) + 1;
        self.clock.send_replace(next);
    }

    /// Changes when the live sampler, or a route, sees displayed live data change.
    pub(crate) fn subscribe(&self) -> watch::Receiver<u64> {
        self.clock.subscribe()
    }

    fn live_keys(&self) -> Vec<String> {
        self.live_keys_at(Instant::now())
    }

    fn remember_open(&self, device: &str, key: String) {
        self.open.lock().expect("feed open views poisoned")
            .insert(device.to_owned(), (key, Instant::now()));
    }

    fn live_keys_at(&self, now: Instant) -> Vec<String> {
        let mut open = self.open.lock().expect("feed open views poisoned");
        open.retain(|_, (_, seen)| now.saturating_duration_since(*seen) < Duration::from_secs(60));
        let mut cache = self.cache.lock().expect("feed cache poisoned");
        cache.retain(|key, _| open.values().any(|(active, _)| active == key));
        let mut keys = cache.values().flat_map(|current| current.live_keys.iter().cloned())
            .collect::<Vec<_>>();
        keys.sort();
        keys.dedup();
        keys
    }

    fn cached_current(&self, key: &str, epoch: &str, revision: u64, wake: u64) -> Option<Current> {
        self.cache.lock().expect("feed cache poisoned").get(key)
            .filter(|current| same_input_revision(
                &current.epoch, current.clock.1, current.wake, epoch, revision, wake,
            ))
            .cloned()
    }
}

/// One live sampler belongs to the daemon client, independently of how many devices poll.
/// It reads in-memory live state and publishes only when its visible projection changes.
pub(crate) fn start_clock(client: &Arc<crate::client_routes::Client>) {
    let Ok(runtime) = tokio::runtime::Handle::try_current() else { return };
    let weak = Arc::downgrade(client);
    runtime.spawn(async move {
        let mut interval = tokio::time::interval_at(
            tokio::time::Instant::now() + Duration::from_secs(5),
            Duration::from_secs(5),
        );
        let Some(initial_client) = weak.upgrade() else { return };
        let mut domain_changes = initial_client.domain.subscribe();
        let mut state = initial_client.domain.state().ok();
        let mut hook_changes = initial_client.domain.live.changed.subscribe();
        let mut report_changes = initial_client.domain.live.mods.changed.subscribe();
        drop(initial_client);
        let mut last = None;
        loop {
            let Some(domain_advanced) = next_sample(
                async { interval.tick().await; },
                &mut domain_changes,
                &mut hook_changes,
                &mut report_changes,
            ).await else { break };
            let Some(client) = weak.upgrade() else { break };
            if domain_advanced || state.is_none() {
                state = client.domain.state().ok();
            }
            let Some(state) = state.as_ref() else { continue };
            sample_live(&client, state, &mut last).await;
        }
    });
}

async fn next_sample(
    periodic: impl std::future::Future<Output = ()>,
    domain_changes: &mut watch::Receiver<alondra_core::domain::Revision>,
    hook_changes: &mut watch::Receiver<()>,
    report_changes: &mut watch::Receiver<()>,
) -> Option<bool> {
    tokio::select! {
        _ = periodic => Some(false),
        changed = domain_changes.changed() => changed.ok().map(|()| true),
        changed = hook_changes.changed() => changed.ok().map(|()| false),
        changed = report_changes.changed() => changed.ok().map(|()| false),
    }
}

async fn sample_live(
    client: &crate::client_routes::Client,
    state: &Arc<alondra_core::domain::DomainState>,
    last: &mut Option<String>,
) -> bool {
    let keys = client.feeds.live_keys();
    let live = super::live::gather(client, state, Lang::En, &keys, false, true).await;
    let digest = live_revision(&live);
    if last.as_ref() == Some(&digest) {
        return false;
    }
    *last = Some(digest);
    client.feeds.wake();
    true
}

#[derive(Serialize)]
struct Reply {
    protocol: u32,
    cursor: String,
    data_epoch: String,
    status: alondra_core::views::Status,
    stale: Vec<String>,
    reset: bool,
    superseded: bool,
    service: alondra_core::views::Service,
}

#[derive(Clone)]
struct Current {
    cursor: String,
    status: alondra_core::views::Status,
    stale: Vec<String>,
    epoch: String,
    // Rendering is needed only when domain or displayed live data changed.
    clock: (String, u64, String),
    wake: u64,
    live_keys: Vec<String>,
}

pub(crate) async fn handle(
    client: &crate::client_routes::Client,
    device: &str,
    query: &str,
    transport: Transport,
) -> Response {
    let (kind, lang, wait, cursor, views) = match parse(query) {
        Ok(parsed) => parsed,
        Err(parse_error) => {
            let lang = query_language(query);
            return match parse_error {
                QueryError::Invalid => error("invalid_input", "invalid changes query", "consulta de cambios no válida", lang),
                QueryError::IncompatibleProtocol => super::incompatible(if lang == Lang::Es { "protocolo de vistas incompatible" } else { "incompatible views protocol" }),
            };
        }
    };
    client.remember_language(device, lang);
    client.feeds.remember_open(device, feed_key(kind, lang, transport, &views));
    let (cancel_tx, mut cancel_rx) = oneshot::channel();
    let generation = client.feeds.next.fetch_add(1, Ordering::Relaxed);
    if let Some((_, old)) = client
        .feeds
        .waiters
        .lock()
        .expect("feed waiters poisoned")
        .insert(device.to_owned(), (generation, cancel_tx))
    {
        let _ = old.send(());
    }
    let started = tokio::time::Instant::now();
    let mut domain_changes = client.domain.subscribe();
    let mut live_changes = client.feeds.clock.subscribe();
    let mut previous = None;
    loop {
        let current = match current(client, kind, lang, transport, &views, previous.as_ref()).await {
            Ok(current) => current,
            Err(response) => {
                remove_waiter(client, device, generation);
                return response;
            }
        };
        let old_epoch = cursor.as_deref().and_then(decode_cursor).map(|c| c.0);
        let reset = old_epoch.as_deref().is_some_and(|old| old != current.epoch);
        let changed = cursor.is_none()
            || reset
            || !current.stale.is_empty()
            || cursor
                .as_deref()
                .and_then(decode_cursor)
                .is_some_and(|c| c.3 != current.status.version);
        if changed || started.elapsed() >= Duration::from_secs(wait) {
            remove_waiter(client, device, generation);
            return Json(Reply {
                protocol: VIEWS_PROTOCOL,
                cursor: current.cursor,
                data_epoch: current.epoch,
                status: current.status,
                stale: current.stale,
                reset,
                superseded: false,
                service: crate::views::this_service(),
            })
            .into_response();
        }
        previous = Some(current.clone());
        let timeout = tokio::time::sleep(Duration::from_secs(wait).saturating_sub(started.elapsed()));
        tokio::pin!(timeout);
        tokio::select! {
            _ = &mut cancel_rx => {
                remove_waiter(client, device, generation);
                return Json(Reply { protocol: VIEWS_PROTOCOL, cursor: current.cursor, data_epoch: current.epoch, status: current.status, stale: vec![], reset: false, superseded: true, service: crate::views::this_service() }).into_response();
            },
            _ = domain_changes.changed() => {},
            _ = live_changes.changed() => {},
            _ = &mut timeout => {
                remove_waiter(client, device, generation);
                return Json(Reply { protocol: VIEWS_PROTOCOL, cursor: current.cursor, data_epoch: current.epoch, status: current.status, stale: vec![], reset: false, superseded: false, service: crate::views::this_service() }).into_response();
            },
        }
    }
}

pub(crate) fn approved_path(path: &str) -> bool {
    let Some(query) = path.strip_prefix("/changes?") else {
        return false;
    };
    parse(query).is_ok()
}

async fn current(
    client: &crate::client_routes::Client,
    kind: Client,
    lang: Lang,
    transport: Transport,
    views: &[(String, String)],
    previous: Option<&Current>,
) -> Result<Current, Response> {
    let read = client.domain.read();
    let state = read.state();
    let wake = *client.feeds.clock.borrow();
    let cache_key = feed_key(kind, lang, transport, views);
    if let Some(current) = client.feeds.cached_current(&cache_key, &state.data_epoch.0, state.revision, wake) {
        return Ok(current);
    }
    let cached = client.feeds.cache.lock().expect("feed cache poisoned")
        .get(&cache_key).cloned();
    let previous = previous.or(cached.as_ref());
    let status_key = status_view_key(kind);
    let mut keys = Vec::with_capacity(views.len() + 1);
    keys.push(status_key.to_owned());
    keys.extend(views.iter().filter(|(key, _)| super::routes::valid_open_key(key)).map(|(key, _)| key.clone()));
    let live = super::live::gather(client, &state, lang, &keys, false, false).await;
    let input = super::cache::Input::of(state, &live);
    let clock = (state.data_epoch.0.clone(), state.revision, input.live_revision().to_owned());
    if let Some(previous) = previous.filter(|previous| previous.clock == clock) {
        let mut current = previous.clone();
        current.wake = wake;
        cache_current(client, cache_key, &current);
        return Ok(current);
    }
    let mut versions: HashMap<String, Result<super::cache::Version, alondra_core::views::BuildError>> = keys.iter()
        .filter_map(|key| Some((key.clone(), Ok(client.views.version(key, &input, kind, lang, transport)?))))
        .collect();
    if keys.iter().any(|key| !versions.contains_key(key)) {
        let rendering = client.shared();
        let render_read = Arc::clone(&read);
        let render_keys = keys.clone();
        versions = client.read_work.run(move ||
            rendering.views.versions(&render_keys, &render_read, &live, &input, kind, lang, transport)
        ).await.expect("feed render worker failed");
    }
    let status = versions.get(status_key).and_then(|v| v.as_ref().ok())
        .ok_or_else(|| error("storage_unavailable", "view status unavailable",
            "estado de la vista no disponible", lang))?.status.clone();
    let stale = views.iter().filter(|(key, version)| {
        !super::routes::valid_open_key(key)
            || versions.get(key).and_then(|v| v.as_ref().ok()).is_none_or(|v| &v.version != version)
    }).map(|(key, _)| key.clone()).collect();
    let tuple = (
        clock.0.clone(),
        clock.1,
        clock.2.clone(),
        status.version.clone(),
    );
    let bytes = serde_json::to_vec(&tuple).expect("feed cursor serialization");
    let cursor = format!("c1.{}", URL_SAFE_NO_PAD.encode(bytes));
    let current = Current {
        cursor, status, stale, epoch: clock.0.clone(), clock, wake,
        live_keys: keys.into_iter().filter(|key| super::routes::valid_open_key(key)).collect(),
    };
    cache_current(client, cache_key, &current);
    Ok(current)
}

fn feed_key(kind: Client, lang: Lang, transport: Transport, views: &[(String, String)]) -> String {
    format!("{kind:?}/{lang:?}/{transport:?}/{views:?}")
}

fn cache_current(client: &crate::client_routes::Client, key: String, current: &Current) {
    let mut cache = client.feeds.cache.lock().expect("feed cache poisoned");
    if cache.len() >= 64 && !cache.contains_key(&key) {
        cache.clear();
    }
    cache.insert(key, current.clone());
}

fn same_input_revision(
    previous_epoch: &str,
    previous_revision: u64,
    previous_wake: u64,
    epoch: &str,
    revision: u64,
    wake: u64,
) -> bool {
    previous_epoch == epoch && previous_revision == revision && previous_wake == wake
}

fn status_view_key(kind: Client) -> &'static str {
    if kind == Client::Mac { "sidebar" } else { "home" }
}

fn live_revision(live: &alondra_core::views::LiveSnapshot) -> String {
    let bytes = serde_json::to_vec(live).expect("live snapshot serialization");
    let digest = sha2::Sha256::digest(bytes);
    digest[..8]
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn decode_cursor(value: &str) -> Option<(String, u64, String, String)> {
    let bytes = URL_SAFE_NO_PAD.decode(value.strip_prefix("c1.")?).ok()?;
    serde_json::from_slice(&bytes).ok()
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum QueryError {
    Invalid,
    IncompatibleProtocol,
}

fn query_language(query: &str) -> Lang {
    url::form_urlencoded::parse(query.as_bytes())
        .any(|(key, value)| key == "lang" && value == "es")
        .then_some(Lang::Es)
        .unwrap_or(Lang::En)
}

fn parse(query: &str) -> Result<(Client, Lang, u64, Option<String>, Vec<(String, String)>), QueryError> {
    let mut protocol = None;
    let mut client = None;
    let mut lang = None;
    let mut wait = None;
    let mut cursor = None;
    let mut views = None;
    for (key, value) in url::form_urlencoded::parse(query.as_bytes()) {
        let value = value.into_owned();
        let slot = match key.as_ref() {
            "protocol" => &mut protocol,
            "client" => &mut client,
            "lang" => &mut lang,
            "wait" => &mut wait,
            "cursor" => &mut cursor,
            "views" => &mut views,
            _ => return Err(QueryError::Invalid),
        };
        if slot.replace(value).is_some() {
            return Err(QueryError::Invalid);
        }
    }
    let protocol = protocol.ok_or(QueryError::Invalid)?
        .parse::<u32>().map_err(|_| QueryError::Invalid)?;
    let incompatible_protocol = !super::serves(protocol);
    let kind = match client.ok_or(QueryError::Invalid)?.as_str() {
        "ios" => Client::Ios,
        "mac" => Client::Mac,
        "tui" => Client::Tui,
        _ => return Err(QueryError::Invalid),
    };
    let lang = match lang.ok_or(QueryError::Invalid)?.as_str() {
        "en" => Lang::En,
        "es" => Lang::Es,
        _ => return Err(QueryError::Invalid),
    };
    let wait = wait.ok_or(QueryError::Invalid)?.parse::<u64>().map_err(|_| QueryError::Invalid)?;
    if wait > 50 {
        return Err(QueryError::Invalid);
    }
    if cursor.as_ref().is_some_and(|c| decode_cursor(c).is_none()) {
        return Err(QueryError::Invalid);
    }
    let mut parsed = Vec::new();
    if let Some(value) = views {
        if !value.is_empty() {
            for item in value.split(',') {
                let (key, version) = item.rsplit_once('@').ok_or(QueryError::Invalid)?;
                if !valid_key(key)
                    || version.len() != 17
                    || !version.starts_with('v')
                    || !version[1..].bytes().all(|b| b.is_ascii_hexdigit())
                {
                    return Err(QueryError::Invalid);
                }
                parsed.push((key.to_owned(), version.to_owned()));
                if parsed.len() > 16 {
                    return Err(QueryError::Invalid);
                }
            }
        }
    }
    if incompatible_protocol {
        Err(QueryError::IncompatibleProtocol)
    } else {
        Ok((kind, lang, wait, cursor, parsed))
    }
}

fn error(code: &str, english: &str, spanish: &str, lang: Lang) -> Response {
    crate::domain_api::domain_error(DomainError::new(code,
        if lang == Lang::Es { spanish } else { english }))
}

fn valid_key(key: &str) -> bool {
    // Old clients may still report a view removed by a newer daemon. Keep that
    // entry in the stale list without ever resolving or rendering its key.
    !key.is_empty()
        && key.len() <= 512
        && key.bytes().all(|byte| byte.is_ascii_alphanumeric()
            || matches!(byte, b'_' | b'-' | b'.' | b':' | b'/' | b'#' | b'?' | b'=' | b'&' | b'%'))
}

fn remove_waiter(client: &crate::client_routes::Client, device: &str, generation: u64) {
    let mut waiters = client.feeds.waiters.lock().expect("feed waiters poisoned");
    if waiters
        .get(device)
        .is_some_and(|(current, _)| *current == generation)
    {
        waiters.remove(device);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alondra_core::domain::{
        Actor, Command, CommandEnvelope, QuestionOption, Recipient, Scope,
    };
    use std::{fs, sync::Arc};

    struct FeedFixture {
        client: Arc<crate::client_routes::Client>,
        home: crate::test_support::Directory,
    }

    impl FeedFixture {
        fn new() -> Self {
            let home = crate::test_support::Directory::new();
            fs::create_dir(&home).unwrap();
            let domain = Arc::new(crate::domain_runtime::DomainRuntime::open(&home).unwrap());
            let client = crate::client_routes::Client::open(
                domain,
                &home,
                Arc::new(std::sync::Mutex::new(None)),
            );
            Self { home, client }
        }

        fn create_request(&self, command_id: &str, body: &str) {
            let state = self.client.domain.state().unwrap();
            let envelope = CommandEnvelope {
                protocol_version: 2,
                data_epoch: state.data_epoch.clone(),
                command_id: command_id.into(),
                expected_revision: state.revision,
                command: Command::CreateRequest {
                    request_id: format!("request-{command_id}").into(),
                    body: body.into(),
                    auto_merge: false, maintenance: false, message: false,
                },
            };
            self.client.domain.execute(
                &Actor::User { user_id: "feed-test".into() },
                &envelope,
                None,
                || Ok(()),
            ).unwrap();
        }

        fn register_agent(&self, command_id: &str) {
            let state = self.client.domain.state().unwrap();
            let envelope = CommandEnvelope {
                protocol_version: 2,
                data_epoch: state.data_epoch.clone(),
                command_id: command_id.into(),
                expected_revision: state.revision,
                command: Command::RegisterAgent {
                    agent_id: format!("agent-{command_id}").into(),
                    name: "Unseen agent".into(), at: 0
                },
            };
            self.client.domain.execute(
                &Actor::System,
                &envelope,
                None,
                || Ok(()),
            ).unwrap();
        }

        /// A node with an open task: the node's id and the key of its task's view.
        fn node_on_task(&self, name: &str) -> (String, String) {
            self.register_agent(name);
            self.create_request(name, "Work");
            let state = self.client.domain.state().unwrap();
            self.client.domain.execute(
                &Actor::Orchestrator { generation: 0 },
                &CommandEnvelope {
                    protocol_version: 2,
                    data_epoch: state.data_epoch.clone(),
                    command_id: format!("task-{name}").into(),
                    expected_revision: state.revision,
                    command: Command::CreateTask { plan_items: None,
                        task_id: format!("task-{name}").into(),
                        request_id: format!("request-{name}").into(),
                        title: "Work".into(),
                        agent_id: Some(format!("agent-{name}").into()),
                        review_prs: false,
                        dependencies: Default::default(),
                        stack: None,
                        launch: Some(alondra_core::domain::LaunchPlan::new("node", "/tmp")),
                    },
                },
                None,
                || Ok(()),
            ).unwrap();
            (format!("agent-{name}"), format!("task:task-{name}"))
        }

        fn create_visible_question(&self, suffix: &str) {
            self.register_agent(&format!("agent-{suffix}"));
            self.create_request(&format!("request-{suffix}"), "Visible request");
            let request_id = format!("request-request-{suffix}");
            let agent_id = format!("agent-agent-{suffix}");
            let state = self.client.domain.state().unwrap();
            self.client.domain.execute(
                &Actor::Orchestrator { generation: 0 },
                &CommandEnvelope {
                    protocol_version: 2,
                    data_epoch: state.data_epoch.clone(),
                    command_id: format!("task-{suffix}").into(),
                    expected_revision: state.revision,
                    command: Command::CreateTask { plan_items: None,
                        task_id: format!("task-{suffix}").into(),
                        request_id: request_id.into(),
                        title: "Visible task".into(),
                        agent_id: Some(agent_id.clone().into()),
                        review_prs: false,
                        dependencies: Default::default(),
                        stack: None,
                        launch: Some(alondra_core::domain::LaunchPlan::new("node", "/tmp")),
                    },
                },
                None,
                || Ok(()),
            ).unwrap();
            let state = self.client.domain.state().unwrap();
            self.client.domain.execute(
                &Actor::Agent { agent_id: agent_id.into() },
                &CommandEnvelope {
                    protocol_version: 2,
                    data_epoch: state.data_epoch.clone(),
                    command_id: format!("question-{suffix}").into(),
                    expected_revision: state.revision,
                    command: Command::AskQuestion {
                        attachments: vec![],
                        interaction_id: format!("question-{suffix}").into(),
                        scope: Scope::Task { task_id: format!("task-{suffix}").into() },
                        recipient: Recipient::User,
                        body: "Visible question".into(),
                        blocking: true,
                        options: vec![QuestionOption {
                            id: "yes".into(),
                            label: "Yes".into(),
                            description: None,
                        }], card: alondra_core::domain::CardSpec::derived(alondra_core::domain::CardKind::Provide, "Answer the question"),
                    },
                },
                None,
                || Ok(()),
            ).unwrap();
        }

        async fn initial(&self, views: &[(String, String)]) -> Current {
            current(&self.client, Client::Ios, Lang::En, Transport::Loopback, views, None)
                .await.unwrap()
        }
    }

    async fn response_json(response: Response) -> serde_json::Value {
        let bytes = axum::body::to_bytes(response.into_body(), 1024 * 1024).await.unwrap();
        serde_json::from_slice(&bytes).unwrap()
    }

    fn query(cursor: &str, wait: u64, views: &[(String, String)]) -> String {
        let mut query = format!("protocol=1&client=ios&lang=en&wait={wait}&cursor={cursor}");
        if !views.is_empty() {
            query.push_str("&views=");
            query.push_str(&url::form_urlencoded::byte_serialize(
                views.iter().map(|(key, version)| format!("{key}@{version}")).collect::<Vec<_>>().join(",").as_bytes()
            ).collect::<String>());
        }
        query
    }

    fn cursor_for(epoch: &str, revision: u64, live: &str, status: &str) -> String {
        let bytes = serde_json::to_vec(&(epoch, revision, live, status)).unwrap();
        format!("c1.{}", URL_SAFE_NO_PAD.encode(bytes))
    }

    #[test]
    fn mac_feed_uses_a_supported_status_view() {
        assert_eq!(status_view_key(Client::Mac), "sidebar");
        assert_eq!(status_view_key(Client::Ios), "home");
        assert_eq!(status_view_key(Client::Tui), "home");
    }

    #[test]
    fn feed_query_is_bounded_and_rejects_ambiguous_fields() {
        assert!(parse("protocol=1&client=ios&lang=es&wait=50").is_ok());
        assert!(
            parse("protocol=1&client=ios&lang=es&wait=0&views=home%40v0123456789abcdef").is_ok()
        );
        assert!(parse("protocol=1&client=ios&lang=es&wait=51").is_err());
        assert!(parse("protocol=1&client=ios&lang=es&wait=1&wait=2").is_err());
        assert_eq!(parse("protocol=2&client=ios&lang=es&wait=1"), Err(QueryError::IncompatibleProtocol));
        assert!(parse("protocol=1&client=ios&lang=es&wait=1&views=card%3A%2F@v0123456789abcdef").is_ok());
        assert!(parse("protocol=1&client=ios&lang=es&wait=1&views=card%3A%0A@v0123456789abcdef").is_err());
    }

    #[test]
    fn cached_feed_input_is_invalidated_by_domain_or_live_changes() {
        let feeds = Feeds::default();
        let current = Current {
            cursor: "cursor-1".into(),
            status: alondra_core::views::Status {
                version: "v0000000000000000".into(), summary: "Ready".into(), badge: 0,
                counts: vec![], gauges: vec![], orchestrator: alondra_core::views::Label {
                    text: "Idle".into(), tone: alondra_core::views::Tone::Neutral, elapsed: None,
                }, update: None, usage_exhausted: None, session_wait: None,
            },
            stale: vec![], epoch: "epoch".into(), clock: ("epoch".into(), 4, "digest".into()),
            wake: 8, live_keys: vec!["home".into()],
        };
        cache_current_for_test(&feeds, "ios/en/home", &current);
        assert_eq!(feeds.cached_current("ios/en/home", "epoch", 4, 8).unwrap().cursor, "cursor-1");
        assert!(feeds.cached_current("ios/en/home", "epoch", 5, 8).is_none(), "domain revision invalidates the stored view");
        assert!(feeds.cached_current("ios/en/home", "epoch", 4, 9).is_none(), "live wake invalidates the stored view");
        assert!(feeds.cached_current("ios/en/home", "new-epoch", 4, 8).is_none(), "new epoch invalidates the stored view");
    }

    fn cache_current_for_test(feeds: &Feeds, key: &str, current: &Current) {
        feeds.cache.lock().unwrap().insert(key.to_owned(), current.clone());
    }

    #[tokio::test]
    async fn one_live_clock_wakes_every_open_feed() {
        let feeds = Feeds::default();
        let mut first = feeds.clock.subscribe();
        let mut second = feeds.clock.subscribe();
        feeds.wake();
        first.changed().await.unwrap();
        second.changed().await.unwrap();
        assert_eq!(*first.borrow(), *second.borrow());
    }

    #[tokio::test]
    async fn a_domain_change_to_an_open_view_answers_stale() {
        let fixture = FeedFixture::new();
        let state = fixture.client.domain.state().unwrap();
        let live = super::super::live::gather(&fixture.client, &state, Lang::En, &["home".into()], false, false).await;
        let rendered = fixture.client.views.render("home", &alondra_core::domain::ReadSnapshot::build(state.clone()), &live, Client::Ios, Lang::En, Transport::Loopback).unwrap();
        let open = vec![("home".to_owned(), rendered.version)];
        let first = fixture.initial(&open).await;
        fixture.create_visible_question("visible-change");
        let reply = response_json(handle(&fixture.client, "device-stale", &query(&first.cursor, 0, &open), Transport::Loopback).await).await;
        assert_eq!(reply["stale"], serde_json::json!(["home"]));
    }

    #[tokio::test]
    async fn another_device_on_the_same_views_compares_versions_without_mounting_any() {
        let fixture = FeedFixture::new();
        fixture.create_visible_question("shown");
        let open = vec![("home".to_owned(), "v0000000000000000".to_owned())];
        let first = response_json(handle(&fixture.client, "phone", &query_without_cursor(&open), Transport::Loopback).await).await;
        fixture.client.views.forget_bodies();
        let built = fixture.client.views.build_count();
        let other = vec![("home".to_owned(), "v1111111111111111".to_owned())];
        let second = response_json(handle(&fixture.client, "tablet", &query_without_cursor(&other), Transport::Loopback).await).await;
        assert_eq!(fixture.client.views.build_count(), built, "the status and the open view come from their versions");
        assert_eq!(second["stale"], serde_json::json!(["home"]));
        assert_eq!(second["status"], first["status"]);
        assert_eq!(second["cursor"], first["cursor"]);
    }

    fn query_without_cursor(views: &[(String, String)]) -> String {
        let views = views.iter().map(|(key, version)| format!("{key}@{version}")).collect::<Vec<_>>().join(",");
        format!("protocol={VIEWS_PROTOCOL}&client=ios&lang=en&wait=0&views={views}")
    }

    #[tokio::test]
    async fn an_unrelated_domain_change_keeps_waiting() {
        let fixture = FeedFixture::new();
        let first = fixture.initial(&[]).await;
        let mut pending = tokio::spawn({
            let client = fixture.client.clone();
            let query = query(&first.cursor, 10, &[]);
            async move { response_json(handle(&client, "device-wait", &query, Transport::Loopback).await).await }
        });
        tokio::task::yield_now().await;
        // The commit advances domain revision but adds no live node, hook or open view.
        fixture.create_request("unrelated-change", "Unrelated request");
        assert!(tokio::time::timeout(Duration::from_millis(50), &mut pending).await.is_err(),
            "a domain commit alone must not answer an unchanged open feed");
        // Supersede the parked request to leave no waiter behind in the fixture.
        let _ = response_json(handle(&fixture.client, "device-wait", &query(&first.cursor, 0, &[]), Transport::Loopback).await).await;
        assert_eq!(pending.await.unwrap()["superseded"], true);
    }

    fn poll_sample(
        domain_changes: &mut watch::Receiver<alondra_core::domain::Revision>,
        hook_changes: &mut watch::Receiver<()>,
        report_changes: &mut watch::Receiver<()>,
    ) -> std::task::Poll<Option<bool>> {
        use std::future::Future;
        let mut pending = Box::pin(next_sample(
            std::future::pending(), domain_changes,
            hook_changes, report_changes,
        ));
        pending.as_mut().poll(&mut std::task::Context::from_waker(std::task::Waker::noop()))
    }

    #[test]
    fn node_reports_publish_without_domain_or_periodic_clock_and_coalesce_bursts() {
        // Open outside the runtime so this test owns the single sampler's progression.
        let fixture = FeedFixture::new();
        let (agent, key) = fixture.node_on_task("event-driven");
        let agent = agent.as_str();
        let report = |command: &str| crate::node_reports::Envelope {
            session_id: agent.into(),
            report: crate::node_reports::Report::ToolStart {
                tool: "Bash".into(), tool_use_id: Some("tool-call".into()),
                input: serde_json::json!({"command": command}),
            },
        };
        fixture.client.domain.report(crate::node_reports::Envelope {
            session_id: agent.into(),
            report: crate::node_reports::Report::SessionStart {
                version: "test".into(), cwd: None, launch_key: None,
            },
        }, 1_700_000_000_000);
        fixture.client.domain.report(report("echo first"), 1_700_000_000_000);
        let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        runtime.block_on(async {
            let state = fixture.client.domain.state().unwrap();
            let mut domain_changes = fixture.client.domain.subscribe();
            let mut hook_changes = fixture.client.domain.live.changed.subscribe();
            let mut report_changes = fixture.client.domain.live.mods.changed.subscribe();
            assert!(poll_sample(&mut domain_changes, &mut hook_changes, &mut report_changes).is_pending());
            let live = super::super::live::gather(&fixture.client, &state, Lang::En, &[key.clone()], false, false).await;
            let rendered = fixture.client.views.render(&key, &alondra_core::domain::ReadSnapshot::build(state.clone()), &live, Client::Ios, Lang::En, Transport::Loopback).unwrap();
            let open = vec![(key.clone(), rendered.version)];
            let first = fixture.initial(&open).await;
            let _ = response_json(handle(&fixture.client, "device-report", &query(&first.cursor, 0, &open), Transport::Loopback).await).await;
            let mut last = None;
            assert!(sample_live(&fixture.client, &state, &mut last).await);
            let before = *fixture.client.feeds.clock.borrow();
            assert!(poll_sample(&mut domain_changes, &mut hook_changes, &mut report_changes).is_pending());
            for index in 0..64 {
                fixture.client.domain.report(report(&format!("echo burst-{index}")), 1_700_000_000_001 + index);
            }
            assert_eq!(fixture.client.domain.state().unwrap().revision, state.revision,
                "tool reports must not need a domain commit to publish");
            let triggered = poll_sample(&mut domain_changes, &mut hook_changes, &mut report_changes);
            println!("reports=64 trigger={triggered:?} domain_commits=0 periodic_ticks=0");
            assert_eq!(triggered, std::task::Poll::Ready(Some(false)),
                "the normal node report path must trigger sampling with the periodic clock withheld");
            assert!(sample_live(&fixture.client, &state, &mut last).await);
            assert_eq!(*fixture.client.feeds.clock.borrow(), before + 1);
            assert!(poll_sample(&mut domain_changes, &mut hook_changes, &mut report_changes).is_pending(), "64 reports retain one sampling permit");
            let reply = response_json(handle(&fixture.client, "device-report", &query(&first.cursor, 0, &open), Transport::Loopback).await).await;
            assert_eq!(reply["stale"], serde_json::json!([key]), "{reply}");
            let live = super::super::live::gather(&fixture.client, &state, Lang::En, &[key.clone()], false, false).await;
            let rendered = fixture.client.views.render(&key, &alondra_core::domain::ReadSnapshot::build(state.clone()), &live, Client::Ios, Lang::En, Transport::Loopback).unwrap();
            assert!(serde_json::to_string(&rendered).unwrap().contains("echo burst-63"), "the final activity reaches the view");
            fixture.client.domain.report(report("echo burst-63"), 1_700_000_000_064);
            assert_eq!(poll_sample(&mut domain_changes, &mut hook_changes, &mut report_changes), std::task::Poll::Ready(Some(false)));
            assert!(!sample_live(&fixture.client, &state, &mut last).await, "an unchanged visible report does not wake clients");
            assert_eq!(*fixture.client.feeds.clock.borrow(), before + 1);
            println!("reports=64 sample_permits=1 visible_wakes=1 domain_commits=0 latest_activity=echo burst-63 duplicate_wakes=0 periodic_ticks=0");
        });
    }

    #[test]
    fn hooks_and_connection_changes_trigger_sampling_but_stale_disconnects_do_not() {
        let fixture = FeedFixture::new();
        let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        runtime.block_on(async {
            let mut domain_changes = fixture.client.domain.subscribe();
            let mut hook_changes = fixture.client.domain.live.changed.subscribe();
            let mut report_changes = fixture.client.domain.live.mods.changed.subscribe();
            fixture.client.domain.live.observe(&serde_json::json!({"hook_event_name": "Stop"}));
            assert!(poll_sample(&mut domain_changes, &mut hook_changes, &mut report_changes).is_pending());
            fixture.client.domain.live.observe(&serde_json::json!({"session_id": "hook", "hook_event_name": "PreToolUse", "tool_name": "Bash", "tool_input": {"command": "echo hooked"}}));
            assert_eq!(poll_sample(&mut domain_changes, &mut hook_changes, &mut report_changes), std::task::Poll::Ready(Some(false)));
            let hello = || crate::node_reports::Hello { session_id: "connected".into(), version: "test".into(), cwd: None, launch_key: None, working: false };
            let first = fixture.client.domain.connect_node(hello(), 1_700_000_000_000);
            assert_eq!(poll_sample(&mut domain_changes, &mut hook_changes, &mut report_changes), std::task::Poll::Ready(Some(false)));
            let second = fixture.client.domain.connect_node(hello(), 1_700_000_000_001);
            assert_eq!(poll_sample(&mut domain_changes, &mut hook_changes, &mut report_changes), std::task::Poll::Ready(Some(false)));
            assert!(!fixture.client.domain.live.mods.disconnect("connected", first));
            assert!(poll_sample(&mut domain_changes, &mut hook_changes, &mut report_changes).is_pending());
            assert!(fixture.client.domain.live.mods.disconnect("connected", second));
            assert_eq!(poll_sample(&mut domain_changes, &mut hook_changes, &mut report_changes), std::task::Poll::Ready(Some(false)));
        });
    }

    #[test]
    fn a_retired_client_sampler_cannot_consume_the_replacement_clients_report() {
        let fixture = FeedFixture::new();
        let retired = crate::client_routes::Client::open(
            fixture.client.domain.clone(), &fixture.home,
            Arc::new(std::sync::Mutex::new(None)),
        );
        let weak = Arc::downgrade(&retired);
        let mut retired_domain = retired.domain.subscribe();
        let mut retired_hooks = retired.domain.live.changed.subscribe();
        let mut retired_reports = retired.domain.live.mods.changed.subscribe();
        drop(retired);
        assert!(weak.upgrade().is_none());
        let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        runtime.block_on(async {
            let mut domain_changes = fixture.client.domain.subscribe();
            let mut hook_changes = fixture.client.domain.live.changed.subscribe();
            let mut report_changes = fixture.client.domain.live.mods.changed.subscribe();
            fixture.client.domain.report(crate::node_reports::Envelope {
                session_id: "replacement".into(),
                report: crate::node_reports::Report::ToolEnd { tool_use_id: None },
            }, 1_700_000_000_000);
            assert_eq!(poll_sample(&mut retired_domain, &mut retired_hooks, &mut retired_reports), std::task::Poll::Ready(Some(false)));
            assert_eq!(poll_sample(&mut domain_changes, &mut hook_changes, &mut report_changes), std::task::Poll::Ready(Some(false)),
                "the retired sampler must not steal the replacement's event");
            assert!(poll_sample(&mut domain_changes, &mut hook_changes, &mut report_changes).is_pending());
        });
    }

    #[test]
    fn a_node_state_change_wakes_the_feed() {
        let fixture = FeedFixture::new();
        let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        runtime.block_on(async {
            let (agent, key) = fixture.node_on_task("node-wake");
            let agent = agent.as_str();
            fixture.client.domain.live.observe(&serde_json::json!({
                "session_id": agent, "hook_event_name": "PreToolUse", "tool_name": "Bash",
                "tool_input": {"command": "echo first"}, "cwd": "/tmp/feed-project"
            }));
            let current = fixture.initial(&[(key.clone(), "v0000000000000000".into())]).await;
            let open = vec![(key.clone(), "v0000000000000000".into())];
            let _ = response_json(handle(&fixture.client, "device-node", &query(&current.cursor, 0, &open), Transport::Loopback).await).await;
            assert!(!current.live_keys.is_empty(), "the open task view is registered for live sampling");
            let mut last = None;
            assert!(sample_live(&fixture.client, &fixture.client.domain.state().unwrap(), &mut last).await);
            let first_wake = *fixture.client.feeds.clock.borrow();
            assert!(!sample_live(&fixture.client, &fixture.client.domain.state().unwrap(), &mut last).await,
                "an unchanged node projection does not wake the feed");
            fixture.client.domain.live.observe(&serde_json::json!({
                "session_id": agent, "hook_event_name": "PreToolUse", "tool_name": "Bash",
                "tool_input": {"command": "echo second"}, "cwd": "/tmp/feed-project"
            }));
            assert!(sample_live(&fixture.client, &fixture.client.domain.state().unwrap(), &mut last).await,
                "a changed node activity projection wakes the feed");
            assert_eq!(*fixture.client.feeds.clock.borrow(), first_wake + 1);
        });
    }

    #[tokio::test]
    async fn the_activity_updates_through_the_feed() {
        let fixture = FeedFixture::new();
        let (agent, key) = fixture.node_on_task("activity");
        let agent = agent.as_str();
        let tool = |command: &str| serde_json::json!({
            "session_id": agent, "hook_event_name": "PreToolUse", "tool_name": "Bash",
            "tool_input": {"command": command}, "cwd": "/tmp/feed-project"
        });
        fixture.client.domain.live.observe(&tool("echo first"));
        let state = fixture.client.domain.state().unwrap();
        let live = super::super::live::gather(&fixture.client, &state, Lang::En, &[key.clone()], false, false).await;
        let rendered = fixture.client.views.render(&key, &alondra_core::domain::ReadSnapshot::build(state.clone()), &live, Client::Ios, Lang::En, Transport::Loopback).unwrap();
        let open = vec![(key.clone(), rendered.version)];
        let first = fixture.initial(&open).await;
        let unchanged = response_json(handle(&fixture.client, "device-activity", &query(&first.cursor, 0, &open), Transport::Loopback).await).await;
        assert_eq!(unchanged["stale"], serde_json::json!([]), "{unchanged}");
        let mut last = None;
        sample_live(&fixture.client, &fixture.client.domain.state().unwrap(), &mut last).await;
        fixture.client.domain.live.observe(&tool("echo second"));
        assert!(sample_live(&fixture.client, &fixture.client.domain.state().unwrap(), &mut last).await);
        let reply = response_json(handle(&fixture.client, "device-activity", &query(&first.cursor, 0, &open), Transport::Loopback).await).await;
        assert_eq!(reply["stale"], serde_json::json!([key]), "{reply}");
    }

    #[tokio::test]
    async fn an_unchanged_feed_reuses_its_cached_current() {
        let fixture = FeedFixture::new();
        let before = fixture.initial(&[]).await;
        let again = fixture.initial(&[]).await;
        assert_eq!(before.cursor, again.cursor);
        assert_eq!(fixture.client.feeds.cached_current("Ios/En/Loopback/[]", &before.epoch, before.clock.1, before.wake).unwrap().cursor,
            before.cursor);
    }

    #[tokio::test]
    async fn sampler_discards_views_after_device_switch_or_lease_expiry() {
        let fixture = FeedFixture::new();
        let open = vec![("task:missing".to_owned(), "v0000000000000000".to_owned())];
        let first = fixture.initial(&open).await;
        let _ = response_json(handle(&fixture.client, "device-a", &query(&first.cursor, 0, &open), Transport::Loopback).await).await;
        assert_eq!(fixture.client.feeds.live_keys(), vec!["home", "task:missing"]);

        let empty = fixture.initial(&[]).await;
        let _ = response_json(handle(&fixture.client, "device-a", &query(&empty.cursor, 0, &[]), Transport::Loopback).await).await;
        assert_eq!(fixture.client.feeds.live_keys(), vec!["home"], "switching views evicts the previous open key");
        assert!(!fixture.client.feeds.cache.lock().unwrap().contains_key(&feed_key(Client::Ios, Lang::En, Transport::Loopback, &open)));

        let _ = response_json(handle(&fixture.client, "device-a", &query(&first.cursor, 0, &open), Transport::Loopback).await).await;
        assert_eq!(fixture.client.feeds.live_keys(), vec!["home", "task:missing"]);
        assert!(fixture.client.feeds.live_keys_at(Instant::now() + Duration::from_secs(61)).is_empty());
        assert!(fixture.client.feeds.cache.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn epoch_change_answers_reset() {
        let fixture = FeedFixture::new();
        let current = fixture.initial(&[]).await;
        let old = cursor_for("old-epoch", current.clock.1, &current.clock.2, &current.status.version);
        let reply = response_json(handle(&fixture.client, "device-reset", &query(&old, 0, &[]), Transport::Loopback).await).await;
        assert_eq!(reply["reset"], true);
        assert_ne!(reply["data_epoch"], "old-epoch");
    }

    #[tokio::test]
    async fn a_new_feed_supersedes_the_old_one() {
        let fixture = FeedFixture::new();
        let first = fixture.initial(&[]).await;
        let client = fixture.client.clone();
        let old = tokio::spawn(async move {
            response_json(handle(&client, "same-device", &query(&first.cursor, 10, &[]), Transport::Loopback).await).await
        });
        tokio::task::yield_now().await;
        let now = fixture.initial(&[]).await;
        let _ = response_json(handle(&fixture.client, "same-device", &query(&now.cursor, 0, &[]), Transport::Loopback).await).await;
        let old = tokio::time::timeout(Duration::from_secs(1), old).await.unwrap().unwrap();
        assert_eq!(old["superseded"], true);
    }

    #[tokio::test]
    async fn status_is_in_every_reply_and_its_version_moves_with_the_badge() {
        let fixture = FeedFixture::new();
        let first = fixture.initial(&[]).await;
        let initial_reply = response_json(handle(&fixture.client, "status-device", &query(&first.cursor, 0, &[]), Transport::Loopback).await).await;
        assert!(initial_reply.get("status").is_some());
        fixture.create_visible_question("badge-change");
        let next = fixture.initial(&[]).await;
        let changed_reply = response_json(handle(&fixture.client, "status-device", &query(&first.cursor, 0, &[]), Transport::Loopback).await).await;
        assert!(changed_reply.get("status").is_some());
        assert_ne!(first.status.version, next.status.version);
        assert_ne!(first.status.badge, next.status.badge);
        assert_eq!(changed_reply["status"]["version"], next.status.version);
    }
}
