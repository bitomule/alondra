use alondra_core::domain_v2::DomainError;
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
    time::Duration,
};
use tokio::sync::{oneshot, watch};

pub(crate) struct Feeds {
    waiters: Mutex<HashMap<String, (u64, oneshot::Sender<()>)>>,
    cache: Mutex<HashMap<String, Current>>,
    next: AtomicU64,
    clock: watch::Sender<u64>,
}

impl Default for Feeds {
    fn default() -> Self {
        let (clock, _) = watch::channel(0);
        Self { waiters: Mutex::default(), cache: Mutex::default(), next: AtomicU64::new(0), clock }
    }
}

impl Feeds {
    pub(crate) fn wake(&self) {
        let next = self.next.fetch_add(1, Ordering::Relaxed) + 1;
        self.clock.send_replace(next);
    }
}

/// One live sampler belongs to the daemon client, independently of how many devices poll.
/// It reads in-memory live state and publishes only when its visible projection changes.
pub(crate) fn start_clock(client: &Arc<crate::v2_client_routes::Client>) {
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
        drop(initial_client);
        let mut last = None;
        loop {
            let domain_advanced = tokio::select! {
                _ = interval.tick() => false,
                changed = domain_changes.changed() => {
                    if changed.is_err() { break; }
                    true
                },
            };
            let Some(client) = weak.upgrade() else { break };
            if domain_advanced || state.is_none() {
                state = client.domain.state().ok();
            }
            let Some(state) = state.as_ref() else { continue };
            let live = super::live::gather(&client, &state, Lang::En, &[], false, true).await;
            let digest = live_revision(&live);
            if last.as_ref() != Some(&digest) {
                last = Some(digest);
                client.feeds.wake();
            }
        }
    });
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
}

pub(crate) async fn handle(
    client: &crate::v2_client_routes::Client,
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
                QueryError::IncompatibleProtocol => error("incompatible_protocol", "incompatible views protocol", "protocolo de vistas incompatible", lang),
            };
        }
    };
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
            })
            .into_response();
        }
        previous = Some(current.clone());
        let timeout = tokio::time::sleep(Duration::from_secs(wait).saturating_sub(started.elapsed()));
        tokio::pin!(timeout);
        tokio::select! {
            _ = &mut cancel_rx => {
                remove_waiter(client, device, generation);
                return Json(Reply { protocol: VIEWS_PROTOCOL, cursor: current.cursor, data_epoch: current.epoch, status: current.status, stale: vec![], reset: false, superseded: true }).into_response();
            },
            _ = domain_changes.changed() => {},
            _ = live_changes.changed() => {},
            _ = &mut timeout => {
                remove_waiter(client, device, generation);
                return Json(Reply { protocol: VIEWS_PROTOCOL, cursor: current.cursor, data_epoch: current.epoch, status: current.status, stale: vec![], reset: false, superseded: false }).into_response();
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
    client: &crate::v2_client_routes::Client,
    kind: Client,
    lang: Lang,
    transport: Transport,
    views: &[(String, String)],
    previous: Option<&Current>,
) -> Result<Current, Response> {
    let state = client
        .domain
        .state()
        .map_err(|_| error("storage_unavailable", "domain state unavailable",
            "estado de datos no disponible", lang))?;
    let wake = *client.feeds.clock.borrow();
    let cache_key = format!("{kind:?}/{lang:?}/{transport:?}/{views:?}");
    let cached = client.feeds.cache.lock().expect("feed cache poisoned")
        .get(&cache_key).cloned();
    let previous = previous.or(cached.as_ref());
    if let Some(previous) = previous.filter(|previous| same_input_revision(
        &previous.epoch, previous.clock.1, previous.wake,
        &state.data_epoch.0, state.revision, wake,
    )) {
        return Ok(previous.clone());
    }
    let status_key = status_view_key(kind);
    let mut keys = Vec::with_capacity(views.len() + 1);
    keys.push(status_key.to_owned());
    keys.extend(views.iter().filter(|(key, _)| super::routes::valid_open_key(key)).map(|(key, _)| key.clone()));
    let live = super::live::gather(client, &state, lang, &keys, false, false).await;
    let clock = (state.data_epoch.0.clone(), state.revision, live_revision(&live));
    if let Some(previous) = previous.filter(|previous| previous.clock == clock) {
        let mut current = previous.clone();
        current.wake = wake;
        cache_current(client, cache_key, &current);
        return Ok(current);
    }
    let status_view = client
        .views
        .render(status_key, &state, &live, kind, lang, transport)
        .map_err(|_| error("storage_unavailable", "view status unavailable",
            "estado de la vista no disponible", lang))?;
    let mut stale = Vec::new();
    for (key, version) in views {
        let stale_view = if super::routes::valid_open_key(key) {
            match client.views.render(key, &state, &live, kind, lang, transport) {
                Ok(view) => &view.version != version,
                Err(_) => true,
            }
        } else { true };
        if stale_view {
            stale.push(key.clone());
        }
    }
    let tuple = (
        clock.0.clone(),
        clock.1,
        clock.2.clone(),
        status_view.status.version.clone(),
    );
    let bytes = serde_json::to_vec(&tuple).expect("feed cursor serialization");
    let cursor = format!("c1.{}", URL_SAFE_NO_PAD.encode(bytes));
    let current = Current { cursor, status: status_view.status, stale, epoch: clock.0.clone(), clock, wake };
    cache_current(client, cache_key, &current);
    Ok(current)
}

fn cache_current(client: &crate::v2_client_routes::Client, key: String, current: &Current) {
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
    let incompatible_protocol = protocol != VIEWS_PROTOCOL;
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

fn remove_waiter(client: &crate::v2_client_routes::Client, device: &str, generation: u64) {
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
        assert!(same_input_revision("epoch", 4, 8, "epoch", 4, 8));
        assert!(!same_input_revision("epoch", 4, 8, "epoch", 5, 8));
        assert!(!same_input_revision("epoch", 4, 8, "epoch", 4, 9));
        assert!(!same_input_revision("epoch", 4, 8, "new-epoch", 4, 8));
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
}
