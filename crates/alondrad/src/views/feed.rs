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
        Mutex,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};
use tokio::sync::oneshot;

#[derive(Default)]
pub(crate) struct Feeds {
    waiters: Mutex<HashMap<String, (u64, oneshot::Sender<()>)>>,
    next: AtomicU64,
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

pub(crate) async fn handle(
    client: &crate::v2_client_routes::Client,
    device: &str,
    query: &str,
    transport: Transport,
) -> Response {
    let Some((kind, lang, wait, cursor, views)) = parse(query) else {
        return error("invalid_input", "invalid changes query");
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
    loop {
        let current = match current(client, kind, lang, transport, &views).await {
            Ok(current) => current,
            Err(response) => {
                remove_waiter(client, device, generation);
                return response;
            }
        };
        let (new_cursor, status, stale, epoch) = current;
        let old_epoch = cursor.as_deref().and_then(decode_cursor).map(|c| c.0);
        let reset = old_epoch.as_deref().is_some_and(|old| old != epoch);
        let changed = cursor.is_none()
            || reset
            || !stale.is_empty()
            || cursor
                .as_deref()
                .and_then(decode_cursor)
                .is_some_and(|c| c.3 != status.version);
        if changed || started.elapsed() >= Duration::from_secs(wait) {
            remove_waiter(client, device, generation);
            return Json(Reply {
                protocol: VIEWS_PROTOCOL,
                cursor: new_cursor,
                data_epoch: epoch,
                status,
                stale,
                reset,
                superseded: false,
            })
            .into_response();
        }
        let tick = tokio::time::sleep(
            Duration::from_secs(5).min(Duration::from_secs(wait).saturating_sub(started.elapsed())),
        );
        tokio::pin!(tick);
        tokio::select! {
            _ = &mut cancel_rx => return Json(Reply { protocol: VIEWS_PROTOCOL, cursor: new_cursor, data_epoch: epoch, status, stale: vec![], reset: false, superseded: true }).into_response(),
            _ = domain_changes.changed() => {},
            _ = &mut tick => {},
        }
    }
}

pub(crate) fn approved_path(path: &str) -> bool {
    let Some(query) = path.strip_prefix("/changes?") else {
        return false;
    };
    parse(query).is_some()
}

async fn current(
    client: &crate::v2_client_routes::Client,
    kind: Client,
    lang: Lang,
    transport: Transport,
    views: &[(String, String)],
) -> Result<(String, alondra_core::views::Status, Vec<String>, String), Response> {
    let state = client
        .domain
        .state()
        .map_err(|_| error("storage_unavailable", "domain state unavailable"))?;
    let status_key = status_view_key(kind);
    let mut keys = Vec::with_capacity(views.len() + 1);
    keys.push(status_key.to_owned());
    keys.extend(views.iter().map(|(key, _)| key.clone()));
    let live = super::live::gather(client, &state, lang, &keys).await;
    let status_view = client
        .views
        .render(status_key, &state, &live, kind, lang, transport)
        .map_err(|_| error("storage_unavailable", "view status unavailable"))?;
    let mut stale = Vec::new();
    for (key, version) in views {
        let view = client
            .views
            .render(key, &state, &live, kind, lang, transport)
            .map_err(|_| error("invalid_input", "invalid open view key"))?;
        if &view.version != version {
            stale.push(key.clone());
        }
    }
    let tuple = (
        state.data_epoch.0.clone(),
        state.revision,
        live_revision(&live),
        status_view.status.version.clone(),
    );
    let bytes = serde_json::to_vec(&tuple).expect("feed cursor serialization");
    let cursor = format!("c1.{}", URL_SAFE_NO_PAD.encode(bytes));
    Ok((cursor, status_view.status, stale, state.data_epoch.0))
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

fn parse(query: &str) -> Option<(Client, Lang, u64, Option<String>, Vec<(String, String)>)> {
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
            _ => return None,
        };
        if slot.replace(value).is_some() {
            return None;
        }
    }
    if protocol?.parse::<u32>().ok()? != VIEWS_PROTOCOL {
        return None;
    }
    let kind = match client?.as_str() {
        "ios" => Client::Ios,
        "mac" => Client::Mac,
        "tui" => Client::Tui,
        _ => return None,
    };
    let lang = match lang?.as_str() {
        "en" => Lang::En,
        "es" => Lang::Es,
        _ => return None,
    };
    let wait = wait?.parse::<u64>().ok()?;
    if wait > 50 {
        return None;
    }
    if cursor.as_ref().is_some_and(|c| decode_cursor(c).is_none()) {
        return None;
    }
    let mut parsed = Vec::new();
    if let Some(value) = views {
        if !value.is_empty() {
            for item in value.split(',') {
                let (key, version) = item.rsplit_once('@')?;
                if !valid_key(key)
                    || version.len() != 17
                    || !version.starts_with('v')
                    || !version[1..].bytes().all(|b| b.is_ascii_hexdigit())
                {
                    return None;
                }
                parsed.push((key.to_owned(), version.to_owned()));
                if parsed.len() > 16 {
                    return None;
                }
            }
        }
    }
    Some((kind, lang, wait, cursor, parsed))
}

fn error(code: &str, message: &str) -> Response {
    crate::domain_api::domain_error(DomainError::new(code, message))
}

fn valid_key(key: &str) -> bool {
    super::routes::valid_open_key(key)
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
        assert!(parse("protocol=1&client=ios&lang=es&wait=50").is_some());
        assert!(
            parse("protocol=1&client=ios&lang=es&wait=0&views=home%40v0123456789abcdef").is_some()
        );
        assert!(parse("protocol=1&client=ios&lang=es&wait=51").is_none());
        assert!(parse("protocol=1&client=ios&lang=es&wait=1&wait=2").is_none());
        assert!(parse("protocol=2&client=ios&lang=es&wait=1").is_none());
        assert!(
            parse("protocol=1&client=ios&lang=es&wait=1&views=card%3A%2F@v0123456789abcdef")
                .is_none()
        );
    }
}
