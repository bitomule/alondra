//! Local v2-only HTTP bootstrap. This path never constructs a legacy Hub;
//! the routes are the shared domain handlers with a committed host.
use crate::auth::is_local;
use crate::domain_runtime::DomainRuntime;
use alondra_core::domain_v2::DomainError;
use anyhow::{Context, Result, ensure};
use axum::extract::{ConnectInfo, State};
use axum::http::HeaderMap;
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use serde_json::json;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;

pub(crate) struct V2Only {
    domain: Arc<DomainRuntime>,
    home: PathBuf,
}

impl V2Only {
    pub(crate) fn open(home: &Path, expected_epoch: &str) -> Result<Arc<Self>> {
        let database = home.join("domain-v2.db");
        let metadata = std::fs::symlink_metadata(&database).with_context(|| {
            format!("committed domain database missing: {}", database.display())
        })?;
        ensure!(
            metadata.is_file(),
            "committed domain database must be a regular file"
        );
        let domain = Arc::new(DomainRuntime::open(home)?);
        ensure!(
            domain.state()?.data_epoch.0 == expected_epoch,
            "cutover epoch does not match domain database"
        );
        Ok(Arc::new(Self {
            domain,
            home: home.to_owned(),
        }))
    }

    pub(crate) fn domain(&self) -> Arc<DomainRuntime> {
        self.domain.clone()
    }
}

impl crate::domain_api::DomainHost for V2Only {
    fn domain(&self) -> &DomainRuntime {
        &self.domain
    }
    fn home(&self) -> &Path {
        &self.home
    }
    fn cutover_committed(&self) -> bool {
        true
    }
}

pub(crate) fn router(state: Arc<V2Only>) -> Router {
    Router::new()
        .route("/v2/daemon", get(daemon))
        .merge(crate::domain_api::routes())
        .with_state(state)
}

/// The running version and update state, for `alondra update` and `alondra doctor` on this Mac.
async fn daemon(State(state): State<Arc<V2Only>>, ConnectInfo(peer): ConnectInfo<SocketAddr>, headers: HeaderMap) -> Response {
    if !is_local(&peer, &headers) {
        return crate::domain_api::domain_error(DomainError::new("forbidden", "local request required"));
    }
    Json(json!({"version": env!("CARGO_PKG_VERSION"), "update": crate::updates::status(&state.home),
        "preflight_rejected": crate::relaunch::rejected()})).into_response()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::Auth;
    use axum::http::StatusCode;
    use base64::{Engine as _, engine::general_purpose::STANDARD};
    use p256::ecdsa::{Signature, SigningKey, signature::Signer};
    use std::net::SocketAddr;

    #[tokio::test]
    async fn standalone_router_reads_real_domain_and_has_no_legacy_routes() {
        let home = std::env::temp_dir().join(format!(
            "alondra-v2-bootstrap-{}",
            crate::auth::random_hex(8)
        ));
        std::fs::create_dir(&home).unwrap();
        let domain = DomainRuntime::open(&home).unwrap();
        let epoch = domain.state().unwrap().data_epoch.0;
        drop(domain);
        let auth = Auth::open(&home).unwrap();
        let key = SigningKey::from_slice(&[9u8; 32]).unwrap();
        let pairing = auth.new_pairing_code();
        let device = auth
            .pair(
                &pairing,
                "test-device",
                &STANDARD.encode(key.verifying_key().to_sec1_bytes()),
            )
            .unwrap();
        let state = V2Only::open(&home, &epoch).unwrap();
        assert!(V2Only::open(&home, "wrong-epoch").is_err());
        let domain = state.domain();
        let capability = domain.issue(crate::domain_authority::Principal::Agent {
            session_id: "test-agent".into(), task_binding: None, provider_agent_id: None,
        }).unwrap();
        let bearer = capability.expose_for_child_environment().to_owned();
        let auth = Arc::new(Auth::open(&home).unwrap());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let app = router(state).layer(axum::middleware::from_fn_with_state(
            auth,
            crate::auth::guard,
        ));
        let server = tokio::spawn(async move {
            axum::serve(
                listener,
                app.into_make_service_with_connect_info::<SocketAddr>(),
            )
            .await
            .unwrap();
        });
        let client = reqwest::Client::new();
        let daemon: serde_json::Value = client.get(format!("{base}/v2/daemon")).send().await.unwrap().json().await.unwrap();
        assert_eq!(daemon["version"], env!("CARGO_PKG_VERSION"));
        assert_eq!(daemon["update"]["current"], env!("CARGO_PKG_VERSION"));
        assert_eq!(
            client
                .get(format!("{base}/v1/overview"))
                .send()
                .await
                .unwrap()
                .status(),
            StatusCode::NOT_FOUND
        );
        assert_eq!(
            client
                .get(format!("{base}/v2/domain"))
                .send()
                .await
                .unwrap()
                .status(),
            StatusCode::FORBIDDEN
        );
        let path = "/v2/domain";
        let timestamp = alondra_core::now_millis().to_string();
        let message = crate::auth::signed_message("GET", path, &timestamp, b"");
        let signature: Signature = key.sign(message.as_bytes());
        let response = client
            .get(format!("{base}{path}"))
            .header("x-alondra-device", &device.id)
            .header("x-alondra-timestamp", timestamp)
            .header(
                "x-alondra-signature",
                STANDARD.encode(signature.to_der().as_bytes()),
            )
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body: serde_json::Value = response.json().await.unwrap();
        assert_eq!(body["protocol_version"], 2);
        assert_eq!(body["data_epoch"].as_str(), Some(epoch.as_str()));
        let producer = client.get(format!("{base}/v2/domain/capabilities"))
            .bearer_auth(&bearer).send().await.unwrap();
        assert_eq!(producer.status(), StatusCode::OK);
        let cursor: serde_json::Value = producer.json().await.unwrap();
        assert_eq!(cursor["data_epoch"].as_str(), Some(epoch.as_str()));
        assert_eq!(client.get(format!("{base}/v2/domain/capabilities"))
            .bearer_auth("invalid").send().await.unwrap().status(), StatusCode::FORBIDDEN);
        for (path, expected) in [
            ("/v2/domain/schedules", StatusCode::OK),
            ("/v2/domain/schedules/missing", StatusCode::NOT_FOUND),
            ("/v2/domain/delivery-attempts?after=0", StatusCode::OK),
            ("/v2/domain/tasks/missing/actions", StatusCode::NOT_FOUND),
            ("/v2/domain/document-revisions/missing/content?offset=0", StatusCode::NOT_FOUND),
            ("/v2/domain/pull-requests/missing/diff", StatusCode::NOT_FOUND),
        ] {
            assert_eq!(client.get(format!("{base}{path}")).send().await.unwrap().status(),
                StatusCode::FORBIDDEN, "unsigned {path}");
            let timestamp = alondra_core::now_millis().to_string();
            let message = crate::auth::signed_message("GET", path, &timestamp, b"");
            let signature: Signature = key.sign(message.as_bytes());
            let response = client.get(format!("{base}{path}"))
                .header("x-alondra-device", &device.id)
                .header("x-alondra-timestamp", timestamp)
                .header("x-alondra-signature", STANDARD.encode(signature.to_der().as_bytes()))
                .send().await.unwrap();
            assert_eq!(response.status(), expected, "signed {path}");
            if expected == StatusCode::NOT_FOUND {
                let body: serde_json::Value = response.json().await.unwrap();
                assert_eq!(body["code"], "not_found", "handler must answer {path}");
            }
        }
        let merge_path = "/v2/domain/merge-authorizations/missing";
        assert_eq!(client.get(format!("{base}{merge_path}"))
            .header("authorization", format!("Bearer {}", "a".repeat(64)))
            .send().await.unwrap().status(), StatusCode::FORBIDDEN);
        let response = client.get(format!("{base}{merge_path}"))
            .header("authorization", format!("Bearer {bearer}"))
            .send().await.unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
        let body: serde_json::Value = response.json().await.unwrap();
        assert_eq!(body["code"], "not_found", "handler must answer merge authorization read");
        domain.revoke_agent("test-agent").unwrap();
        assert_eq!(client.get(format!("{base}{merge_path}"))
            .header("authorization", format!("Bearer {bearer}"))
            .send().await.unwrap().status(), StatusCode::FORBIDDEN);
        assert_eq!(client.get(format!("{base}/v2/domain/capabilities"))
            .bearer_auth(&bearer).send().await.unwrap().status(), StatusCode::FORBIDDEN);
        server.abort();
        let _ = server.await;
        std::fs::remove_dir_all(home).unwrap();
    }

    #[test]
    fn committed_boot_requires_existing_database() {
        let home = std::env::temp_dir().join(format!(
            "alondra-v2-bootstrap-missing-{}",
            crate::auth::random_hex(8)
        ));
        std::fs::create_dir(&home).unwrap();
        assert!(V2Only::open(&home, "epoch").is_err());
        assert!(
            !home.join("domain-v2.db").exists(),
            "must not create an empty authority on missing stage"
        );
        std::fs::remove_dir_all(home).unwrap();
    }
}

#[cfg(test)]
mod vertical_tests;
