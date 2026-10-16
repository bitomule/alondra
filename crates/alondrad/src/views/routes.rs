use alondra_core::domain::DomainError;
use alondra_core::views::{BuildError, Client, Lang, Transport, VIEWS_PROTOCOL};
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Extension, Json, Router};
use serde_json::json;
use std::sync::Arc;

use crate::auth::Signed;
use crate::domain_bootstrap::DomainDaemon;
use crate::client_routes;

pub(crate) fn router() -> Router<Arc<DomainDaemon>> {
    Router::new()
        .route("/views/{key}", get(local))
        .route("/actions", post(super::actions::local))
        .route("/operation-results/{id}", get(super::action_results::read))
        .route("/operation-journal", get(super::action_results::descriptor))
        .route("/operation-journal/ack", post(super::action_results::acknowledge))
}

async fn local(
    State(state): State<Arc<DomainDaemon>>,
    signed: Option<Extension<Signed>>,
    Path(key): Path<String>,
    uri: axum::http::Uri,
) -> Response {
    let Some(device) = crate::domain_bootstrap::signed_device(state.client().home(), &signed) else {
        return error("forbidden", "signed user device required");
    };
    let response = respond(
        &state.client(),
        &key,
        uri.query().unwrap_or(""),
        Transport::Loopback,
        Some(&device.device_id),
    )
    .await;
    if crate::domain_bootstrap::signed_device(state.client().home(), &signed).is_none() {
        return error("forbidden", "signed user device required");
    }
    response
}

pub(crate) async fn relay(client: &client_routes::Client, device: &str, path_and_query: &str) -> Response {
    let Some((raw_key, query)) = path_and_query
        .strip_prefix("/views/")
        .and_then(|rest| rest.split_once('?'))
    else {
        return error("invalid_input", "invalid view path");
    };
    let Some(key) = normalize_key(raw_key) else {
        return error_lang(requested_lang(query), "invalid_input", "invalid view key");
    };
    respond(client, &key, query, Transport::Relay, Some(device)).await
}

pub(crate) async fn relay_feed(
    client: &client_routes::Client,
    device: &str,
    path_and_query: &str,
) -> Response {
    let Some(query) = path_and_query.strip_prefix("/changes?") else {
        return error("invalid_input", "invalid changes path");
    };
    super::feed::handle(client, device, query, Transport::Relay).await
}

pub(super) async fn respond(
    client: &client_routes::Client,
    path_key: &str,
    query: &str,
    transport: Transport,
    device: Option<&str>,
) -> Response {
    let requested_language = requested_lang(query);
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
            _ => {
                return error_lang(
                    requested_language,
                    "invalid_input",
                    "unsupported view parameter",
                );
            }
        };
        if slot.replace(value.into_owned()).is_some() {
            return error_lang(
                requested_language,
                "invalid_input",
                "duplicate view parameter",
            );
        }
    }
    let Some(protocol) = protocol.as_deref().and_then(|s| s.parse::<u32>().ok()) else {
        return error_lang(requested_language, "invalid_input", "protocol is required");
    };
    if !super::serves(protocol) {
        return super::incompatible(if lang.as_deref() == Some("es") {
            "Actualiza Alondra"
        } else {
            "Update Alondra"
        });
    }
    let client_kind = match kind.as_deref() {
        Some("ios") => Client::Ios,
        Some("mac") => Client::Mac,
        Some("tui") => Client::Tui,
        _ => return error_lang(requested_language, "invalid_input", "invalid client"),
    };
    let language = match lang.as_deref() {
        Some("en") => Lang::En,
        Some("es") => Lang::Es,
        _ => return error_lang(requested_language, "invalid_input", "invalid language"),
    };
    if let Some(device) = device {
        client.remember_language(device, language);
    }
    if known.as_deref().is_some_and(|version| {
        version.len() != 17
            || !version.starts_with('v')
            || !version[1..].bytes().all(|byte| byte.is_ascii_hexdigit())
    }) {
        return error_lang(language, "invalid_input", "invalid known version");
    }
    let Some(base_key) = normalize_key(path_key) else {
        return error_lang(language, "invalid_input", "invalid view key");
    };
    let Some(key) = view_key(&base_key, &options) else {
        return error_lang(language, "invalid_input", "invalid view parameter");
    };
    let read = client.domain.read();
    let live = super::live::gather(client, read.state(), language, std::slice::from_ref(&key), true, false).await;
    if key == "issue_picker?refresh=1" {
        client.feeds.wake();
    }
    let service = crate::views::this_service();
    let input = super::cache::Input::of(read.state(), &live);
    if let Some(version) = known.as_deref()
        .and_then(|known| client.views.version(&key, &input, client_kind, language, transport).filter(|v| v.version == known))
    {
        return unchanged(&version.cursor, &version.data_epoch, service);
    }
    let rendering = client.shared();
    let render_key = key.clone();
    let rendered = client.read_work.run(move ||
        rendering.views.render_from(&render_key, &read, &live, &input, client_kind, language, transport)
    ).await.expect("view render worker failed");
    let mut view = match rendered {
        Ok(view) => view,
        Err(BuildError::NotFound) => return error_lang(language, "not_found", "view not found"),
        Err(BuildError::InvalidActionId) => {
            log!("view {key} could not be rendered because an action identifier was invalid");
            return error_lang(language, "internal_error", "could not render an action");
        }
        Err(BuildError::InvalidKey | BuildError::UnsupportedClient) => {
            return error_lang(language, "invalid_input", "invalid view key");
        }
    };
    if known.as_deref() == Some(view.version.as_str()) {
        return unchanged(&view.cursor, &view.data_epoch, service);
    }
    view.service = Some(service);
    let bytes = match serde_json::to_vec(&view) {
        Ok(bytes) => bytes,
        Err(_) => return error_lang(language, "storage_unavailable", "view serialization failed"),
    };
    if bytes.len() > super::VIEW_BUDGET_BYTES {
        return error_lang(
            language,
            "payload_too_large",
            "view exceeds the response budget",
        );
    }
    (
        StatusCode::OK,
        [("content-type", "application/json")],
        bytes,
    )
        .into_response()
}

fn unchanged(cursor: &str, data_epoch: &str, service: alondra_core::views::Service) -> Response {
    Json(json!({"protocol":VIEWS_PROTOCOL,"unchanged":true,"cursor":cursor,
        "data_epoch":data_epoch,"service":service})).into_response()
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
            (Some("active" | "finished" | "archived"), Some(n)) if n.parse::<usize>().is_ok() => {
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
    // Relay requests carry the entire key as one encoded path segment. Axum may
    // already have decoded its Path extractor; decoding twice is never allowed.
    let decoded;
    let raw = if raw.contains('%') {
        if raw.contains('&') || raw.contains('+') {
            return None;
        }
        if raw.as_bytes().iter().enumerate().any(|(i, b)| {
            *b == b'%'
                && (i + 2 >= raw.len()
                    || !raw.as_bytes()[i + 1].is_ascii_hexdigit()
                    || !raw.as_bytes()[i + 2].is_ascii_hexdigit())
        }) {
            return None;
        }
        let encoded = format!("key={raw}");
        let mut fields = url::form_urlencoded::parse(encoded.as_bytes());
        decoded = fields.next()?.1.into_owned();
        if fields.next().is_some() {
            return None;
        }
        if decoded.contains('%') {
            return None;
        }
        decoded.as_str()
    } else {
        raw
    };
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
    alondra_core::views::id::valid(id)
}

fn error(code: &str, message: &str) -> Response {
    crate::domain_api::domain_error(DomainError::new(code, message))
}

fn requested_lang(query: &str) -> Lang {
    let mut values = url::form_urlencoded::parse(query.as_bytes())
        .filter(|(name, _)| name == "lang")
        .map(|(_, value)| value.into_owned());
    if values.next().as_deref() == Some("es") && values.next().is_none() {
        Lang::Es
    } else {
        Lang::En
    }
}

fn error_lang(lang: Lang, code: &str, message: &'static str) -> Response {
    let translated = if lang == Lang::Es {
        match message {
            "invalid known version" => "Versión conocida no válida",
            "unsupported view parameter" => "Parámetro de vista no admitido",
            "duplicate view parameter" => "Parámetro de vista duplicado",
            "protocol is required" => "Falta el protocolo",
            "invalid client" => "Cliente no válido",
            "invalid language" => "Idioma no válido",
            "invalid view key" => "Vista no válida",
            "invalid view parameter" => "Parámetro de vista no válido",
            "domain state unavailable" => "El estado no está disponible",
            "view not found" => "Vista no encontrada",
            "view serialization failed" => "No se pudo preparar la vista",
            "view exceeds the response budget" => "La vista supera el límite de tamaño",
            _ => message,
        }
    } else {
        message
    };
    error(code, translated)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> (crate::test_support::Directory, Arc<DomainDaemon>) {
        let home = crate::test_support::Directory::new();
        std::fs::create_dir_all(&home).unwrap();
        drop(crate::domain_runtime::DomainRuntime::open(&home).unwrap());
        let state = DomainDaemon::open(&home).unwrap();
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
        assert!(approved_read_path(
            "/views/card:beta-0.3.0-42-result?protocol=1&client=ios&lang=es"
        ));
        assert!(!approved_read_path(
            "/views/card:..?protocol=1&client=ios&lang=en"
        ));
        assert!(approved_read_path(
            "/views/pull_request%3Agithub.com%2Fbitomule%2Falondra%23154?protocol=1&client=mac&lang=en"
        ));
        assert!(approved_read_path(
            "/views/node%3Aa98d034b-249e-4487-a89b-57de38c26372?protocol=1&client=tui&lang=en"
        ));
        assert!(approved_read_path(
            "/views/tasks?protocol=1&client=tui&lang=en&section=active&after=20"
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
            "/views/pull_request%3Agithub.com%2F..%2Falondra%23154?protocol=1&client=mac&lang=en"
        ));
        assert!(!approved_read_path(
            "/views/card%3Ai-42&ignore=1?protocol=1&client=ios&lang=en"
        ));
        assert!(!approved_read_path(
            "/views/home?protocol=1&client=ios&lang=en&future=1"
        ));
    }

    #[tokio::test]
    async fn malformed_view_queries_answer_in_the_requested_language() {
        let (home, state) = fixture();
        let client = state.client();
        for (query, expected) in [
            (
                "protocol=1&client=ios&future=1&lang=es",
                "Parámetro de vista no admitido",
            ),
            (
                "protocol=1&client=ios&lang=es&client=mac",
                "Parámetro de vista duplicado",
            ),
            ("client=ios&lang=es", "Falta el protocolo"),
            ("protocol=1&client=unknown&lang=es", "Cliente no válido"),
        ] {
            let response = respond(&client, "home", query, Transport::Loopback, None).await;
            let body = axum::body::to_bytes(response.into_body(), 1024 * 1024)
                .await
                .unwrap();
            let value: serde_json::Value = serde_json::from_slice(&body).unwrap();
            assert_eq!(value["message"], expected, "{query}");
        }
        drop(client);
        drop(state);
        std::fs::remove_dir_all(home).unwrap();
    }

    #[tokio::test]
    async fn protocol_mismatch_is_refused() {
        let (home, state) = fixture();
        let response = respond(
            &state.client(),
            "home",
            "protocol=2&client=ios&lang=es",
            Transport::Loopback,
            None,
        )
        .await;
        assert_eq!(response.status(), StatusCode::CONFLICT);
        let body = axum::body::to_bytes(response.into_body(), 1024 * 1024)
            .await
            .unwrap();
        let value: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(value["code"], "incompatible_protocol");
        assert_eq!(value["message"], "Actualiza Alondra");
        drop(state);
        std::fs::remove_dir_all(home).unwrap();
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
    async fn known_version_answers_unchanged_without_mounting_the_view() {
        let (home, state) = fixture();
        let client = state.client();
        let full = respond(
            &client,
            "home",
            "protocol=1&client=ios&lang=en",
            Transport::Loopback,
            None,
        )
        .await;
        assert_eq!(full.status(), StatusCode::OK);
        let body = axum::body::to_bytes(full.into_body(), 1024 * 1024)
            .await
            .unwrap();
        let view: serde_json::Value = serde_json::from_slice(&body).unwrap();
        let known = view["version"].as_str().unwrap();
        client.views.forget_bodies();
        let built = client.views.build_count();
        let conditional = respond(
            &client,
            "home",
            &format!("protocol=1&client=ios&lang=en&known={known}"),
            Transport::Loopback,
            None,
        )
        .await;
        assert_eq!(conditional.status(), StatusCode::OK);
        let body = axum::body::to_bytes(conditional.into_body(), 1024 * 1024)
            .await
            .unwrap();
        let value: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(value["unchanged"], true);
        assert_eq!(client.views.build_count(), built, "answered from the version, the view was not mounted");
        assert_eq!(value["cursor"], view["cursor"]);
        assert_eq!(value["data_epoch"], view["data_epoch"]);
        drop(client);
        drop(state);
        std::fs::remove_dir_all(home).unwrap();
    }

    #[tokio::test]
    async fn relay_serves_views() {
        let (home, state) = fixture();
        let client = state.client();
        let query = "protocol=1&client=ios&lang=en";
        let local = respond(&client, "home", query, Transport::Loopback, None).await;
        let remote = relay(&client, "phone-a", &format!("/views/home?{query}")).await;
        assert_eq!(local.status(), StatusCode::OK);
        assert_eq!(remote.status(), StatusCode::OK);
        let local = axum::body::to_bytes(local.into_body(), super::super::VIEW_BUDGET_BYTES)
            .await
            .unwrap();
        let remote = axum::body::to_bytes(remote.into_body(), super::super::VIEW_BUDGET_BYTES)
            .await
            .unwrap();
        assert_eq!(local, remote);
        assert_eq!(client.language_for_device("phone-a"), Lang::En);
        drop(client);
        drop(state);
        std::fs::remove_dir_all(home).unwrap();
    }

    #[tokio::test]
    async fn views_remember_the_last_language_per_device_with_english_default() {
        let (home, state) = fixture();
        let client = state.client();
        assert_eq!(client.language_for_device("phone-a"), Lang::En);
        let response = relay(&client, "phone-a", "/views/home?protocol=1&client=ios&lang=es").await;
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(client.language_for_device("phone-a"), Lang::Es);
        assert_eq!(client.language_for_device("phone-b"), Lang::En);
        drop(client);
        drop(state);
        let reopened = DomainDaemon::open(&home).unwrap();
        let reopened_client = reopened.client();
        assert_eq!(reopened_client.language_for_device("phone-a"), Lang::Es,
            "pushes after a daemon restart use the device's persisted language");
        assert_eq!(reopened_client.language_for_device("phone-b"), Lang::En);
        let response = relay(&reopened_client, "phone-a", "/views/home?protocol=1&client=ios&lang=en").await;
        assert_eq!(response.status(), StatusCode::OK);
        drop(reopened_client);
        drop(reopened);
        let reopened = DomainDaemon::open(&home).unwrap();
        assert_eq!(reopened.client().language_for_device("phone-a"), Lang::En,
            "the last language survives a second restart");
        drop(reopened);
        std::fs::remove_dir_all(home).unwrap();
    }
}
