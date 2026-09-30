use alondra_core::domain_v2::DomainError;
use alondra_core::views::{BuildError, Client, Lang, Transport, VIEWS_PROTOCOL};
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Extension, Json, Router};
use serde_json::json;
use std::sync::Arc;

use crate::auth::Signed;
use crate::domain_bootstrap::V2Only;
use crate::v2_client_routes;

pub(crate) fn router() -> Router<Arc<V2Only>> {
    Router::new()
        .route("/views/{key}", get(local))
        .route("/actions", post(super::actions::local))
}

async fn local(
    State(state): State<Arc<V2Only>>,
    signed: Option<Extension<Signed>>,
    Path(key): Path<String>,
    uri: axum::http::Uri,
) -> Response {
    if crate::domain_bootstrap::signed_device(state.client().home(), &signed).is_none() {
        return error("forbidden", "signed user device required");
    }
    let response = respond(
        &state.client(),
        &key,
        uri.query().unwrap_or(""),
        Transport::Loopback,
    )
    .await;
    if crate::domain_bootstrap::signed_device(state.client().home(), &signed).is_none() {
        return error("forbidden", "signed user device required");
    }
    response
}

pub(crate) async fn relay(client: &v2_client_routes::Client, path_and_query: &str) -> Response {
    let Some((raw_key, query)) = path_and_query
        .strip_prefix("/views/")
        .and_then(|rest| rest.split_once('?'))
    else {
        return error("invalid_input", "invalid view path");
    };
    let Some(key) = normalize_key(raw_key) else {
        return error("invalid_input", "invalid view key");
    };
    respond(client, &key, query, Transport::Relay).await
}

pub(crate) async fn relay_feed(
    client: &v2_client_routes::Client,
    device: &str,
    path_and_query: &str,
) -> Response {
    let Some(query) = path_and_query.strip_prefix("/changes?") else {
        return error("invalid_input", "invalid changes path");
    };
    super::feed::handle(client, device, query, Transport::Relay).await
}

async fn respond(
    client: &v2_client_routes::Client,
    path_key: &str,
    query: &str,
    transport: Transport,
) -> Response {
    let mut protocol = None;
    let mut kind = None;
    let mut lang = None;
    let mut known = None;
    let mut options = ViewOptions::default();
    for (name, value) in url::form_urlencoded::parse(query.as_bytes()) {
        let slot = match name.as_ref() {
            "protocol" => &mut protocol,
            "client" => &mut kind,
            "lang" => &mut lang,
            "known" => &mut known,
            "focus" => &mut options.focus,
            "section" => &mut options.section,
            "after" => &mut options.after,
            "project" => &mut options.project,
            "q" => &mut options.q,
            "refresh" => &mut options.refresh,
            _ => return error("invalid_input", "unsupported view parameter"),
        };
        if slot.replace(value.into_owned()).is_some() {
            return error("invalid_input", "duplicate view parameter");
        }
    }
    let Some(protocol) = protocol.as_deref().and_then(|s| s.parse::<u32>().ok()) else {
        return error("invalid_input", "protocol is required");
    };
    if protocol != VIEWS_PROTOCOL {
        return error("incompatible_protocol", "Update Alondra");
    }
    let client_kind = match kind.as_deref() {
        Some("ios") => Client::Ios,
        Some("mac") => Client::Mac,
        Some("tui") => Client::Tui,
        _ => return error("invalid_input", "invalid client"),
    };
    let language = match lang.as_deref() {
        Some("en") => Lang::En,
        Some("es") => Lang::Es,
        _ => return error("invalid_input", "invalid language"),
    };
    if known.as_deref().is_some_and(|version| {
        version.len() != 17
            || !version.starts_with('v')
            || !version[1..].bytes().all(|byte| byte.is_ascii_hexdigit())
    }) {
        return error("invalid_input", "invalid known version");
    }
    let Some(base_key) = normalize_key(path_key) else {
        return error("invalid_input", "invalid view key");
    };
    let Some(key) = view_key(&base_key, &options) else {
        return error("invalid_input", "invalid view parameter");
    };
    let state = match client.domain.state() {
        Ok(state) => state,
        Err(_) => return error("storage_unavailable", "domain state unavailable"),
    };
    let live = super::live::gather(client, &state, language, std::slice::from_ref(&key), true, false).await;
    if key == "issue_picker?refresh=1" {
        client.feeds.wake();
    }
    let view = match client
        .views
        .render(&key, &state, &live, client_kind, language, transport)
    {
        Ok(view) => view,
        Err(BuildError::NotFound) => return error("not_found", "view not found"),
        Err(BuildError::InvalidKey | BuildError::UnsupportedClient) => {
            return error("invalid_input", "invalid view key");
        }
    };
    if known.as_deref() == Some(view.version.as_str()) {
        return Json(json!({"protocol":VIEWS_PROTOCOL,"unchanged":true,"cursor":view.cursor,
            "data_epoch":view.data_epoch}))
            .into_response();
    }
    let bytes = match serde_json::to_vec(&view) {
        Ok(bytes) => bytes,
        Err(_) => return error("storage_unavailable", "view serialization failed"),
    };
    if bytes.len() > super::VIEW_BUDGET_BYTES {
        return error("payload_too_large", "view exceeds the response budget");
    }
    (
        StatusCode::OK,
        [("content-type", "application/json")],
        bytes,
    )
        .into_response()
}

pub(crate) fn approved_read_path(path_and_query: &str) -> bool {
    if super::feed::approved_path(path_and_query) {
        return true;
    }
    let Some((path, query)) = path_and_query
        .strip_prefix("/views/")
        .and_then(|rest| rest.split_once('?'))
    else {
        return false;
    };
    let Some(key) = normalize_key(path) else {
        return false;
    };
    let mut seen = std::collections::HashSet::new();
    let mut options = ViewOptions::default();
    for (name, value) in url::form_urlencoded::parse(query.as_bytes()) {
        if !seen.insert(name.to_string()) {
            return false;
        }
        match name.as_ref() {
            "protocol" if value == "1" => {}
            "client" if matches!(value.as_ref(), "ios" | "mac" | "tui") => {}
            "lang" if matches!(value.as_ref(), "en" | "es") => {}
            "known"
                if value.len() == 17
                    && value.starts_with('v')
                    && value[1..].bytes().all(|byte| byte.is_ascii_hexdigit()) => {}
            "focus" => options.focus = Some(value.into_owned()),
            "section" => options.section = Some(value.into_owned()),
            "after" => options.after = Some(value.into_owned()),
            "project" => options.project = Some(value.into_owned()),
            "q" => options.q = Some(value.into_owned()),
            "refresh" => options.refresh = Some(value.into_owned()),
            _ => return false,
        }
    }
    ["protocol", "client", "lang"]
        .iter()
        .all(|name| seen.contains(*name))
        && view_key(&key, &options).is_some()
}

#[derive(Default)]
struct ViewOptions {
    focus: Option<String>,
    section: Option<String>,
    after: Option<String>,
    project: Option<String>,
    q: Option<String>,
    refresh: Option<String>,
}

fn view_key(base: &str, options: &ViewOptions) -> Option<String> {
    let ViewOptions {
        focus,
        section,
        after,
        project,
        q,
        refresh,
    } = options;
    let clean = |value: &str| value.len() <= 256 && !value.chars().any(char::is_control);
    if base == "home"
        && section.is_none()
        && after.is_none()
        && project.is_none()
        && q.is_none()
        && refresh.is_none()
    {
        return match focus {
            Some(id) if plain_id(id) => Some(format!("home?focus={id}")),
            None => Some(base.into()),
            _ => None,
        };
    }
    if base == "tasks" && focus.is_none() && project.is_none() && q.is_none() && refresh.is_none() {
        return match (section.as_deref(), after.as_deref()) {
            (None, None) => Some(base.into()),
            (Some("finished" | "archived"), Some(n)) if n.parse::<usize>().is_ok() => {
                Some(format!("tasks?section={}&after={n}", section.as_deref()?))
            }
            _ => None,
        };
    }
    if (base == "new_request" || base == "schedule:new")
        && focus.is_none()
        && section.is_none()
        && after.is_none()
        && q.is_none()
        && refresh.is_none()
    {
        return match project {
            Some(value) if clean(value) => Some(format!(
                "{base}?{}",
                url::form_urlencoded::Serializer::new(String::new())
                    .append_pair("project", value)
                    .finish()
            )),
            None => Some(base.into()),
            _ => None,
        };
    }
    if base == "issue_picker"
        && focus.is_none()
        && section.is_none()
        && after.is_none()
        && project.is_none()
    {
        return match (q, refresh.as_deref()) {
            (Some(value), None) if clean(value) => Some(format!(
                "{base}?{}",
                url::form_urlencoded::Serializer::new(String::new())
                    .append_pair("q", value)
                    .finish()
            )),
            (None, Some("1")) => Some("issue_picker?refresh=1".into()),
            (None, None) => Some(base.into()),
            _ => None,
        };
    }
    if base == "search"
        && focus.is_none()
        && section.is_none()
        && after.is_none()
        && project.is_none()
        && refresh.is_none()
    {
        return match q {
            Some(value) if clean(value) => Some(format!(
                "{base}?{}",
                url::form_urlencoded::Serializer::new(String::new())
                    .append_pair("q", value)
                    .finish()
            )),
            None => Some(base.into()),
            _ => None,
        };
    }
    if focus.is_none()
        && section.is_none()
        && after.is_none()
        && project.is_none()
        && q.is_none()
        && refresh.is_none()
    {
        Some(base.into())
    } else {
        None
    }
}

pub(super) fn valid_open_key(key: &str) -> bool {
    let (path, query) = key.split_once('?').unwrap_or((key, ""));
    let Some(base) = normalize_key(path) else {
        return false;
    };
    let mut options = ViewOptions::default();
    for (name, value) in url::form_urlencoded::parse(query.as_bytes()) {
        let slot = match name.as_ref() {
            "focus" => &mut options.focus,
            "section" => &mut options.section,
            "after" => &mut options.after,
            "project" => &mut options.project,
            "q" => &mut options.q,
            "refresh" => &mut options.refresh,
            _ => return false,
        };
        if slot.replace(value.into_owned()).is_some() {
            return false;
        }
    }
    view_key(&base, &options).as_deref() == Some(key)
}

fn normalize_key(raw: &str) -> Option<String> {
    if matches!(
        raw,
        "home"
            | "sidebar"
            | "tasks"
            | "fleet"
            | "schedules"
            | "new_request"
            | "issue_picker"
            | "search"
            | "machine"
    ) {
        return Some(raw.into());
    }
    for prefix in [
        "card",
        "document",
        "pull_request",
        "task",
        "request",
        "node",
        "schedule",
    ] {
        let id = raw
            .strip_prefix(&format!("{prefix}:"))
            .or_else(|| raw.strip_prefix(&format!("{prefix}%3A")))
            .or_else(|| raw.strip_prefix(&format!("{prefix}%3a")));
        if let Some(id) = id {
            return (plain_id(id) || prefix == "schedule" && id == "new")
                .then(|| format!("{prefix}:{id}"));
        }
    }
    None
}

fn plain_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 256
        && id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_')
}

fn error(code: &str, message: &str) -> Response {
    crate::domain_api::domain_error(DomainError::new(code, message))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> (std::path::PathBuf, Arc<V2Only>) {
        let home = std::env::temp_dir().join(format!(
            "alondra-views-route-{}",
            crate::auth::random_hex(8)
        ));
        std::fs::create_dir_all(&home).unwrap();
        drop(crate::domain_runtime::DomainRuntime::open(&home).unwrap());
        let state = V2Only::open(&home).unwrap();
        (home, state)
    }

    #[test]
    fn approved_views_are_explicit_and_bounded() {
        assert!(approved_read_path(
            "/views/home?protocol=1&client=ios&lang=es"
        ));
        assert!(approved_read_path(
            "/views/card:i-42?protocol=1&client=tui&lang=en"
        ));
        assert!(approved_read_path(
            "/views/card%3Ai-42?protocol=1&client=ios&lang=en"
        ));
        assert!(!approved_read_path(
            "/views/home?protocol=2&client=ios&lang=en"
        ));
        assert!(!approved_read_path(
            "/views/home?protocol=1&client=ios&lang=en&lang=es"
        ));
        assert!(!approved_read_path(
            "/views/home/../devices?protocol=1&client=ios&lang=en"
        ));
        assert!(!approved_read_path(
            "/views/card:%2F?protocol=1&client=ios&lang=en"
        ));
        assert!(!approved_read_path(
            "/views/home?protocol=1&client=ios&lang=en&future=1"
        ));
    }

    #[tokio::test]
    async fn views_route_requires_a_signed_device() {
        let (home, state) = fixture();
        let response = local(
            State(state.clone()),
            None,
            Path("home".into()),
            "/views/home?protocol=1&client=ios&lang=en".parse().unwrap(),
        )
        .await;
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
        drop(state);
        std::fs::remove_dir_all(home).unwrap();
    }

    #[tokio::test]
    async fn known_version_answers_unchanged() {
        let (home, state) = fixture();
        let client = state.client();
        let full = respond(
            &client,
            "home",
            "protocol=1&client=ios&lang=en",
            Transport::Loopback,
        )
        .await;
        assert_eq!(full.status(), StatusCode::OK);
        let body = axum::body::to_bytes(full.into_body(), 1024 * 1024)
            .await
            .unwrap();
        let view: serde_json::Value = serde_json::from_slice(&body).unwrap();
        let known = view["version"].as_str().unwrap();
        let conditional = respond(
            &client,
            "home",
            &format!("protocol=1&client=ios&lang=en&known={known}"),
            Transport::Loopback,
        )
        .await;
        assert_eq!(conditional.status(), StatusCode::OK);
        let body = axum::body::to_bytes(conditional.into_body(), 1024 * 1024)
            .await
            .unwrap();
        let value: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(value["unchanged"], true);
        assert_eq!(value["cursor"], view["cursor"]);
        assert_eq!(value["data_epoch"], view["data_epoch"]);
        drop(client);
        drop(state);
        std::fs::remove_dir_all(home).unwrap();
    }
}
