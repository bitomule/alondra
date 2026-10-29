//! What the Mac app on this machine shows as system notifications while it is not in front.
//! The push gate already decided which cards a phone hears about; the Mac app reads those same
//! notices here, over loopback, so both tell the user about exactly the same cards.
use crate::locks::Poisonable as _;
use std::net::SocketAddr;
use std::sync::Arc;

use alondra_core::domain::InteractionId;
use alondra_core::views::{self, Lang};
use axum::extract::{ConnectInfo, Query, State};
use axum::http::HeaderMap;
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Extension, Json, Router};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::auth::Signed;
use crate::domain_bootstrap::DomainDaemon;
use crate::domain_runtime::DomainRuntime;

pub(crate) fn router() -> Router<Arc<DomainDaemon>> {
    Router::new().route("/notices", get(local))
}

#[derive(Deserialize)]
struct Params {
    lang: Option<String>,
}

async fn local(
    State(state): State<Arc<DomainDaemon>>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    signed: Option<Extension<Signed>>,
    Query(params): Query<Params>,
) -> Response {
    if !crate::auth::is_local(&peer, &headers)
        || crate::domain_bootstrap::signed_device(state.client().home(), &signed).is_none()
    {
        return crate::domain_api::domain_error(alondra_core::domain::DomainError::new(
            "forbidden",
            "signed local device required",
        ));
    }
    let lang = if params.lang.as_deref() == Some("es") { Lang::Es } else { Lang::En };
    let domain = state.domain();
    match tokio::task::spawn_blocking(move || notices(&domain, lang)).await {
        Ok(Ok(body)) => Json(body).into_response(),
        _ => crate::domain_api::domain_error(alondra_core::domain::DomainError::new(
            "storage_unavailable",
            "notices could not be read",
        )),
    }
}

/// The cards that wait for the user, each with the notice a phone got for it. A card no phone
/// was told about (the gate dropped it, or it repeats one already told) has no notice.
pub(crate) fn notices(domain: &DomainRuntime, lang: Lang) -> anyhow::Result<Value> {
    let read = domain.read();
    let open = views::waiting_cards(read.index());
    let ids: Vec<InteractionId> = open.iter().map(|id| InteractionId::from(id.as_str())).collect();
    let handed = domain.store.lock_or_exit("the domain store").handed_user_notices(&ids)?;
    let notices: Vec<Value> = handed
        .into_iter()
        .map(|(message, card)| {
            let content = views::push_content(&read, &card.0, lang).unwrap_or_else(|error| {
                log!("could not render notice for card {}: {error:?}", card.0);
                views::PushContent { alert: None, actions: vec![], interruption_level: "active", thread: None }
            });
            let (title, body) = content.alert.map_or(("Alondra".to_owned(), None), |alert| (alert.title, alert.body));
            json!({"id": message, "card": card.0, "title": title, "body": body,
                "thread": content.thread, "level": content.interruption_level})
        })
        .collect();
    Ok(json!({"data_epoch": read.state().data_epoch, "notices": notices, "open": open}))
}

#[cfg(test)]
mod tests;
