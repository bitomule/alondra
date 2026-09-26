//! Create-only provisioning. A lost response is reconciled by an immutable internal
//! username plus correlation attribute; a retry NEVER updates existing credentials.
use super::{Acceptance, DomainError};
use anyhow::{Result, ensure};
use reqwest::{Client, StatusCode, Url};
use serde::Deserialize;
use serde_json::{Value, json};
use uuid::Uuid;

#[derive(Clone)]
pub struct Keycloak {
    http: Client,
    admin_realm: Url,
    token_url: Url,
    client_id: String,
    client_secret: String,
}
/// No Debug implementation: contains an administrative access token.
struct AdminToken(String);
#[derive(Debug)]
pub struct Provisioned {
    pub subject: String,
    pub already_existed: bool,
}
impl Keycloak {
    pub fn new(
        admin_realm: Url,
        token_url: Url,
        client_id: String,
        client_secret: String,
    ) -> Result<Self> {
        for url in [&admin_realm, &token_url] {
            ensure!(
                url.scheme() == "https"
                    || url.scheme() == "http"
                        && matches!(url.host_str(), Some("127.0.0.1" | "localhost" | "::1")),
                "Keycloak requires TLS outside loopback"
            );
            ensure!(
                url.username().is_empty()
                    && url.password().is_none()
                    && url.query().is_none()
                    && url.fragment().is_none(),
                "invalid Keycloak endpoint"
            );
        }
        ensure!(
            admin_realm.path().ends_with('/'),
            "admin realm URL must end with slash"
        );
        let http = Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(std::time::Duration::from_secs(10))
            .build()?;
        Ok(Self {
            http,
            admin_realm,
            token_url,
            client_id,
            client_secret,
        })
    }
    async fn token(&self) -> Result<AdminToken> {
        #[derive(Deserialize)]
        struct Token {
            access_token: String,
        }
        let response = self
            .http
            .post(self.token_url.clone())
            .form(&[
                ("grant_type", "client_credentials"),
                ("client_id", &self.client_id),
                ("client_secret", &self.client_secret),
            ])
            .send()
            .await
            .map_err(|_| DomainError::ProviderUnavailable)?;
        ensure!(
            response.status() == StatusCode::OK,
            DomainError::ProviderUnavailable
        );
        let value: Token = serde_json::from_value(bounded_json(response).await?)?;
        Ok(AdminToken(value.access_token))
    }
    /// Deployment precondition: do not persist the credentials representation in
    /// Keycloak admin-event details, or substitute email for the correlation username.
    pub async fn verify_realm(&self) -> Result<()> {
        let token = self.token().await?;
        let response = self
            .http
            .get(self.admin_realm.clone())
            .bearer_auth(&token.0)
            .send()
            .await
            .map_err(|_| DomainError::ProviderUnavailable)?;
        ensure!(
            response.status() == StatusCode::OK,
            DomainError::ProviderUnavailable
        );
        let realm = bounded_json(response).await?;
        ensure!(
            realm.get("loginWithEmailAllowed").and_then(Value::as_bool) == Some(true),
            "Keycloak email login must be enabled for invited users"
        );
        ensure!(
            realm
                .get("adminEventsDetailsEnabled")
                .and_then(Value::as_bool)
                == Some(false),
            "Keycloak administrative event details must be disabled"
        );
        ensure!(
            realm
                .get("registrationEmailAsUsername")
                .and_then(Value::as_bool)
                != Some(true),
            "Keycloak must preserve internal usernames"
        );
        ensure!(
            realm.get("registrationAllowed").and_then(Value::as_bool) != Some(true),
            "public Keycloak registration must be disabled"
        );
        Ok(())
    }
    async fn lookup(&self, token: &AdminToken, id: Uuid) -> Result<Option<String>> {
        let username = format!("alondra-{id}");
        let response = self
            .http
            .get(self.admin_realm.join("users")?)
            .bearer_auth(&token.0)
            .query(&[
                ("username", username.as_str()),
                ("exact", "true"),
                ("briefRepresentation", "false"),
                ("max", "2"),
            ])
            .send()
            .await
            .map_err(|_| DomainError::ProviderUnavailable)?;
        ensure!(
            response.status() == StatusCode::OK,
            DomainError::ProviderUnavailable
        );
        let value = bounded_json(response).await?;
        let users = value.as_array().ok_or(DomainError::ProviderUnavailable)?;
        if users.is_empty() {
            return Ok(None);
        }
        ensure!(users.len() == 1, DomainError::Conflict);
        let user = &users[0];
        ensure!(
            user["username"].as_str() == Some(username.as_str())
                && user["attributes"]["alondra_account_id"][0].as_str()
                    == Some(id.to_string().as_str()),
            DomainError::Conflict
        );
        let subject = user["id"]
            .as_str()
            .filter(|s| !s.is_empty())
            .ok_or(DomainError::ProviderUnavailable)?;
        Ok(Some(subject.into()))
    }
    pub async fn provision(&self, reservation: &Acceptance, password: &str) -> Result<Provisioned> {
        ensure!(
            (12..=1024).contains(&password.len()) && !password.contains('\0'),
            DomainError::InvalidInput
        );
        let token = self.token().await?;
        if let Some(subject) = self.lookup(&token, reservation.account_id).await? {
            return Ok(Provisioned {
                subject,
                already_existed: true,
            });
        }
        let response = self
            .http
            .post(self.admin_realm.join("users")?)
            .bearer_auth(&token.0)
            .json(&json!({
                "username":reservation.username(),"email":reservation.email,"emailVerified":false,
                "enabled":true,"firstName":reservation.display_name,
                "attributes":{"alondra_account_id":[reservation.account_id.to_string()]},
                "credentials":[{"type":"password","value":password,"temporary":false}]
            }))
            .send()
            .await;
        match response {
            Ok(response) if response.status() == StatusCode::CREATED => {
                let subject = self
                    .lookup(&token, reservation.account_id)
                    .await?
                    .ok_or(DomainError::ProviderUnavailable)?;
                Ok(Provisioned {
                    subject,
                    already_existed: false,
                })
            }
            Ok(response) if response.status() == StatusCode::BAD_REQUEST => {
                Err(DomainError::InvalidInput.into())
            }
            // Includes conflict and ambiguous network errors. Lookup only, never PUT/reset.
            _ => match self.lookup(&token, reservation.account_id).await? {
                Some(subject) => Ok(Provisioned {
                    subject,
                    already_existed: true,
                }),
                None => Err(DomainError::ProviderUnavailable.into()),
            },
        }
    }
    pub async fn disable(&self, account_id: Uuid) -> Result<()> {
        let token = self.token().await?;
        let Some(subject) = self.lookup(&token, account_id).await? else {
            return Ok(());
        };
        let mut url = self.admin_realm.join("users/")?;
        url.path_segments_mut()
            .map_err(|_| DomainError::InvalidInput)?
            .pop_if_empty()
            .push(&subject);
        let response = self
            .http
            .put(url.clone())
            .bearer_auth(&token.0)
            .json(&json!({"enabled":false}))
            .send()
            .await
            .map_err(|_| DomainError::ProviderUnavailable)?;
        ensure!(
            matches!(
                response.status(),
                StatusCode::NO_CONTENT | StatusCode::NOT_FOUND
            ),
            DomainError::ProviderUnavailable
        );
        url.path_segments_mut()
            .map_err(|_| DomainError::InvalidInput)?
            .push("logout");
        let response = self
            .http
            .post(url)
            .bearer_auth(&token.0)
            .send()
            .await
            .map_err(|_| DomainError::ProviderUnavailable)?;
        ensure!(
            matches!(
                response.status(),
                StatusCode::NO_CONTENT | StatusCode::NOT_FOUND
            ),
            DomainError::ProviderUnavailable
        );
        Ok(())
    }
    pub async fn delete(&self, account_id: Uuid) -> Result<()> {
        let token = self.token().await?;
        let Some(subject) = self.lookup(&token, account_id).await? else {
            return Ok(());
        };
        let mut url = self.admin_realm.join("users/")?;
        url.path_segments_mut()
            .map_err(|_| DomainError::InvalidInput)?
            .pop_if_empty()
            .push(&subject);
        let response = self
            .http
            .delete(url)
            .bearer_auth(&token.0)
            .send()
            .await
            .map_err(|_| DomainError::ProviderUnavailable)?;
        ensure!(
            matches!(
                response.status(),
                StatusCode::NO_CONTENT | StatusCode::NOT_FOUND
            ),
            DomainError::ProviderUnavailable
        );
        Ok(())
    }
}
async fn bounded_json(mut response: reqwest::Response) -> Result<Value> {
    let mut bytes = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|_| DomainError::ProviderUnavailable)?
    {
        ensure!(
            bytes.len() + chunk.len() <= 65536,
            DomainError::ProviderUnavailable
        );
        bytes.extend_from_slice(&chunk);
    }
    serde_json::from_slice(&bytes).map_err(|_| DomainError::ProviderUnavailable.into())
}

#[cfg(test)]
mod integration_tests {
    use super::*;
    use crate::management::tests::{cleanup, fixture_for_issuer};
    use anyhow::Context;

    #[tokio::test]
    async fn management_realm_preflight_rejects_unusable_email_login_and_password_audit()
    -> Result<()> {
        use axum::{
            Json, Router,
            extract::State,
            routing::{get, post},
        };
        use std::sync::{Arc, Mutex};
        let realm = Arc::new(Mutex::new(
            json!({"adminEventsDetailsEnabled":false,"registrationAllowed":false,"registrationEmailAsUsername":false,"loginWithEmailAllowed":false}),
        ));
        let router = Router::new()
            .route(
                "/token",
                post(|| async { Json(json!({"access_token":"synthetic"})) }),
            )
            .route(
                "/realm/",
                get(|State(state): State<Arc<Mutex<Value>>>| async move {
                    Json(state.lock().unwrap().clone())
                }),
            )
            .with_state(realm.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
        let base = format!("http://{}", listener.local_addr()?);
        let task = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
        let kc = Keycloak::new(
            format!("{base}/realm/").parse()?,
            format!("{base}/token").parse()?,
            "fixture".into(),
            "synthetic".into(),
        )?;
        assert!(
            kc.verify_realm().await.is_err(),
            "internal username cannot be the only login route for an invited user"
        );
        realm.lock().unwrap()["loginWithEmailAllowed"] = json!(true);
        kc.verify_realm().await?;
        realm.lock().unwrap()["adminEventsDetailsEnabled"] = json!(true);
        assert!(
            kc.verify_realm().await.is_err(),
            "credentials representation must never enter admin event details"
        );
        task.abort();
        Ok(())
    }

    #[tokio::test]
    #[ignore = "requires disposable local PostgreSQL and Keycloak fixture"]
    async fn management_real_keycloak_reissue_releases_unique_email_without_relinking_identity()
    -> Result<()> {
        let path = std::env::var("ALONDRA_MANAGEMENT_TEST_CONNECTIONS")?;
        let fixture: Value = serde_json::from_slice(&std::fs::read(path)?)?;
        ensure!(
            fixture["marker"] == "alondra-disposable-system-v1",
            "disposable fixture required"
        );
        let field = |name: &str| -> Result<String> {
            Ok(fixture[name]
                .as_str()
                .ok_or_else(|| anyhow::anyhow!("missing fixture field"))?
                .into())
        };
        let realm: Url = field("admin_realm")?.parse()?;
        ensure!(
            matches!(realm.host_str(), Some("127.0.0.1" | "localhost" | "::1")),
            "fixture must be loopback"
        );
        let kc = Keycloak::new(
            realm,
            field("token_url")?.parse()?,
            field("client_id")?,
            field("client_secret")?,
        )?;
        kc.verify_realm()
            .await
            .context("real fixture realm preflight")?;
        let (db, admin, schema) = fixture_for_issuer(&field("issuer")?).await?;
        let email = format!("invited-{}@example.invalid", Uuid::new_v4());
        let invite = db.create_invitation(admin.id, &email, 300).await?;
        let reserved = db.reserve_acceptance(&invite.token, "Test Friend").await?;
        let old_id = reserved.account_id;
        let first = kc
            .provision(&reserved, "Fixture-first-password-123")
            .await
            .context("real fixture first create")?;
        assert!(!first.already_existed);
        let retry = kc
            .provision(&reserved, "Fixture-different-password-456")
            .await
            .context("real fixture retry lookup")?;
        assert!(retry.already_existed);
        assert_eq!(first.subject, retry.subject);
        drop(reserved); // request vanished after provider create, before local activation
        let replacement = db
            .reissue_invitation(admin.id, invite.invitation.id, 300)
            .await?;
        while let Some(job) = db.next_reconciliation().await? {
            match job.action.as_str() {
                "keycloak_disable" => kc
                    .disable(job.account_id)
                    .await
                    .context("real fixture disable")?,
                "keycloak_delete" => kc
                    .delete(job.account_id)
                    .await
                    .context("real fixture delete")?,
                "relay_revoke" => (), // no devices were admitted for an unactivated identity
                _ => unreachable!(),
            }
            db.reconciliation_complete(&job).await?;
        }
        let token = kc.token().await?;
        assert!(
            kc.lookup(&token, old_id).await?.is_none(),
            "disable alone does not release a unique email"
        );
        let next = db
            .reserve_acceptance(&replacement.token, "Test Friend")
            .await?;
        let new_id = next.account_id;
        assert_ne!(new_id, old_id);
        let provisioned = kc
            .provision(&next, "Fixture-new-account-password-789")
            .await
            .context("real fixture replacement create")?;
        assert!(!provisioned.already_existed);
        assert_ne!(provisioned.subject, first.subject);
        next.complete(&provisioned.subject).await?;
        kc.delete(new_id).await?;
        cleanup(db, schema).await
    }
}
