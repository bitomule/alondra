use crate::locks::Poisonable as _;
pub(crate) mod cache;
pub(crate) mod actions;
pub(crate) mod action_results;
pub(crate) mod feed;
pub(crate) mod live;
pub(crate) mod reset_time;
pub(crate) mod routes;

pub(crate) const VIEW_BUDGET_BYTES: usize = {
    let local = crate::service::MAX_LOCAL_RESPONSE - 16 * 1024;
    let encrypted = (crate::service::MAX_RELAY_RESPONSE - 8 * 1024) * 3 / 4;
    if local < encrypted { local } else { encrypted }
};

/// A views protocol this daemon no longer (or not yet) serves: 409 with the range it does serve
/// and its version, so the client says "restart the TUI" instead of a raw refusal.
pub(crate) fn incompatible(message: &str) -> axum::response::Response {
    use axum::response::IntoResponse;
    (
        axum::http::StatusCode::CONFLICT,
        axum::Json(serde_json::json!({"code": "incompatible_protocol", "message": message,
            "service": crate::views::this_service()})),
    )
        .into_response()
}

pub(crate) fn serves(protocol: u32) -> bool {
    (alondra_core::views::VIEWS_PROTOCOL_MIN..=alondra_core::views::VIEWS_PROTOCOL).contains(&protocol)
}

/// Every answer says which daemon gave it, so a one-shot command notices it is older; a
/// terminal's own version is noted once per version, for the log.
pub(crate) async fn stamp_version(
    request: axum::extract::Request,
    next: axum::middleware::Next,
) -> axum::response::Response {
    if let Some(client) = request.headers().get("x-alondra-client-version").and_then(|v| v.to_str().ok()) {
        note_client(client);
    }
    let mut response = next.run(request).await;
    response.headers_mut().insert(
        "x-alondra-version",
        axum::http::HeaderValue::from_static(env!("CARGO_PKG_VERSION")),
    );
    response
}

fn note_client(version: &str) {
    /// A holder that panics leaves it whole, so it is recovered: one push.
    static SEEN: std::sync::Mutex<Vec<String>> = std::sync::Mutex::new(Vec::new());
    let service = env!("CARGO_PKG_VERSION");
    if version == service || version.len() > 32 {
        return;
    }
    let mut seen = SEEN.lock_or_recover("the client versions seen");
    if !seen.iter().any(|v| v == version) {
        seen.push(version.to_owned());
        log!("a terminal on {version} talks to service {service}");
    }
}

/// This daemon as it announces itself in views and refusals.
pub(crate) fn this_service() -> alondra_core::views::Service {
    alondra_core::views::Service::serving(env!("CARGO_PKG_VERSION"))
}
#[cfg(test)]
mod measure_tests;
