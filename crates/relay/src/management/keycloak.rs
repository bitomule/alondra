//! Create-only provisioning. A lost response is reconciled by an immutable internal
//! username plus correlation attribute; a retry NEVER updates existing credentials.
use super::{Acceptance, DomainError, Reconciliation};
use anyhow::{Context, Result, ensure};
use reqwest::{Client, StatusCode, Url};
use serde::Deserialize;
use serde_json::{Value, json};
use uuid::Uuid;

#[derive(Clone)]
pub struct Keycloak {
    issuer: String,
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
        issuer: String,
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
        // The public issuer and private admin host deliberately differ. Their
        // realm and the private token/admin authority must nevertheless agree.
        let issuer_url = Url::parse(&issuer)?;
        let realm = issuer_url
            .path()
            .strip_prefix("/realms/")
            .filter(|realm| !realm.is_empty() && !realm.contains('/') && !realm.contains('%'))
            .ok_or(DomainError::InvalidInput)?;
        ensure!(
            issuer_url.query().is_none()
                && issuer_url.fragment().is_none()
                && issuer_url.username().is_empty()
                && issuer_url.password().is_none(),
            DomainError::InvalidInput
        );
        ensure!(
            admin_realm.path() == format!("/admin/realms/{realm}/")
                && token_url.path() == format!("/realms/{realm}/protocol/openid-connect/token")
                && admin_realm.origin() == token_url.origin(),
            "Keycloak realm/authority mismatch"
        );
        let http = Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(std::time::Duration::from_secs(10))
            .build()?;
        Ok(Self {
            issuer,
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
        let response = self
            .http
            .get(self.admin_realm.join("users/profile")?)
            .bearer_auth(&token.0)
            .send()
            .await
            .map_err(|_| DomainError::ProviderUnavailable)?;
        ensure!(
            response.status() == StatusCode::OK,
            "Keycloak user profile must be readable during preflight"
        );
        let profile = bounded_json(response).await?;
        let attributes = profile
            .get("attributes")
            .and_then(Value::as_array)
            .context("Keycloak user profile attributes are unavailable")?;
        let correlations = attributes
            .iter()
            .filter(|attribute| attribute["name"] == "alondra_account_id")
            .collect::<Vec<_>>();
        ensure!(
            correlations.len() == 1,
            "Keycloak alondra_account_id profile attribute must exist exactly once"
        );
        let correlation = correlations[0];
        ensure!(
            correlation["permissions"]["edit"] == json!(["admin"])
                && correlation["permissions"]["view"] == json!(["admin"])
                && correlation["multivalued"] == json!(false),
            "Keycloak alondra_account_id must be single-valued and admin-only for view and edit"
        );
        for attribute in attributes {
            let name = attribute
                .get("name")
                .and_then(Value::as_str)
                .context("Keycloak user profile attribute has no name")?;
            ensure!(
                name.len() <= 128
                    && name
                        .bytes()
                        .all(|byte| byte.is_ascii_alphanumeric() || b"_-".contains(&byte)),
                "Keycloak user profile contains an unsupported attribute name"
            );
            // The invitation form deliberately collects one display name, not a
            // fabricated family name. Reject profiles that force a follow-up form
            // for fields the product cannot supply (including conditional scopes).
            let required = attribute
                .get("required")
                .is_some_and(|value| !value.is_null() && value != &Value::Bool(false));
            ensure!(
                !required
                    || matches!(
                        name,
                        "username" | "email" | "firstName" | "alondra_account_id"
                    ),
                "Keycloak user profile must not require unsupported invitation attribute: {name}"
            );
        }
        Ok(())
    }
    async fn lookup(&self, token: &AdminToken, id: Uuid) -> Result<Option<String>> {
        self.lookup_correlated(token, id, false).await
    }
    async fn lookup_correlated(
        &self,
        token: &AdminToken,
        id: Uuid,
        cleanup_only: bool,
    ) -> Result<Option<String>> {
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
        let matches_account = user["username"].as_str() == Some(username.as_str())
            && user["attributes"]["alondra_account_id"] == json!([id.to_string()]);
        if cleanup_only && !matches_account {
            // A different account may now own the old name. It is not a
            // cleanup target and must not prevent deleting the bound subject.
            return Ok(None);
        }
        ensure!(matches_account, DomainError::Conflict);
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
    /// Persisted issuer/subject is the authority for admitted accounts. A
    /// username is only a recovery index for an uncompleted provider create.
    async fn reconciliation_subject(
        &self,
        token: &AdminToken,
        job: &Reconciliation,
    ) -> Result<Option<String>> {
        ensure!(job.issuer == self.issuer, DomainError::Conflict);
        let Some(subject) = job.subject.as_deref() else {
            ensure!(job.allow_provisioning_lookup, DomainError::Conflict);
            return self.lookup(token, job.account_id).await;
        };
        ensure!(
            !subject.is_empty() && subject.len() <= 255 && !subject.chars().any(char::is_control),
            DomainError::Conflict
        );
        let mut url = self.admin_realm.join("users/")?;
        url.path_segments_mut()
            .map_err(|_| DomainError::InvalidInput)?
            .pop_if_empty()
            .push(subject);
        let response = self
            .http
            .get(url)
            .bearer_auth(&token.0)
            .send()
            .await
            .map_err(|_| DomainError::ProviderUnavailable)?;
        if response.status() == StatusCode::NOT_FOUND {
            // Never adopt another user that now owns a former username.
            return Ok(None);
        }
        ensure!(
            response.status() == StatusCode::OK,
            DomainError::ProviderUnavailable
        );
        let user = bounded_json(response).await?;
        ensure!(user["id"].as_str() == Some(subject), DomainError::Conflict);
        if let Some(attributes) = user.get("attributes") {
            let attributes = attributes.as_object().ok_or(DomainError::Conflict)?;
            if let Some(correlation) = attributes.get("alondra_account_id") {
                ensure!(
                    correlation == &json!([job.account_id.to_string()]),
                    DomainError::Conflict
                );
            }
        }
        Ok(Some(subject.into()))
    }
    async fn reconciliation_subjects(
        &self,
        token: &AdminToken,
        job: &Reconciliation,
    ) -> Result<Vec<String>> {
        let mut subjects: Vec<String> = self
            .reconciliation_subject(token, job)
            .await?
            .into_iter()
            .collect();
        if job.subject.is_some() && job.allow_provisioned_cleanup {
            // Separate cleanup capability, never a replacement identity: an
            // older timed-out create may commit after the bound user is deleted.
            // Persisted invitation provenance + inactive account restrict this
            // to users we provisioned; legacy bootstrap users never enter here.
            if let Some(late) = self.lookup_correlated(token, job.account_id, true).await? {
                if !subjects.contains(&late) {
                    subjects.push(late);
                }
            }
        }
        Ok(subjects)
    }
    pub async fn disable(&self, job: &Reconciliation) -> Result<()> {
        ensure!(job.issuer == self.issuer, DomainError::Conflict);
        let token = self.token().await?;
        for subject in self.reconciliation_subjects(&token, job).await? {
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
        }
        Ok(())
    }
    pub async fn delete(&self, job: &Reconciliation) -> Result<()> {
        ensure!(job.issuer == self.issuer, DomainError::Conflict);
        let token = self.token().await?;
        for subject in self.reconciliation_subjects(&token, job).await? {
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
        }
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

    fn compatible_profile() -> Value {
        json!({"attributes":[
            {"name":"username","required":{"roles":["user"]}},
            {"name":"email","required":{"roles":["user"]}},
            {"name":"firstName","required":{"roles":["user"]}},
            {"name":"lastName"},
            {"name":"alondra_account_id","permissions":{"view":["admin"],"edit":["admin"]},"multivalued":false}
        ]})
    }
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
                "/realms/test/protocol/openid-connect/token",
                post(|| async { Json(json!({"access_token":"synthetic"})) }),
            )
            .route(
                "/admin/realms/test/",
                get(|State(state): State<Arc<Mutex<Value>>>| async move {
                    Json(state.lock().unwrap().clone())
                }),
            )
            .route(
                "/admin/realms/test/users/profile",
                get(|State(state): State<Arc<Mutex<Value>>>| async move {
                    Json(
                        state
                            .lock()
                            .unwrap()
                            .get("profile")
                            .cloned()
                            .unwrap_or_else(compatible_profile),
                    )
                }),
            )
            .with_state(realm.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
        let base = format!("http://{}", listener.local_addr()?);
        let task = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
        let kc = Keycloak::new(
            "https://identity.example/realms/test".into(),
            format!("{base}/admin/realms/test/").parse()?,
            format!("{base}/realms/test/protocol/openid-connect/token").parse()?,
            "fixture".into(),
            "synthetic".into(),
        )?;
        assert!(
            kc.verify_realm().await.is_err(),
            "internal username cannot be the only login route for an invited user"
        );
        realm.lock().unwrap()["loginWithEmailAllowed"] = json!(true);
        kc.verify_realm().await?;
        for unsupported in ["lastName", "department"] {
            let mut profile = compatible_profile();
            profile["attributes"]
                .as_array_mut()
                .unwrap()
                .push(json!({"name":unsupported,"required":{"roles":["user"]}}));
            realm.lock().unwrap()["profile"] = profile;
            let error = kc.verify_realm().await.unwrap_err();
            assert!(
                error.to_string().contains(unsupported),
                "preflight must identify the incompatible profile attribute"
            );
        }
        for mutation in ["missing", "user_edit", "user_view", "multivalued"] {
            let mut profile = compatible_profile();
            let attributes = profile["attributes"].as_array_mut().unwrap();
            match mutation {
                "missing" => {
                    attributes.pop();
                }
                "user_edit" => {
                    attributes.last_mut().unwrap()["permissions"]["edit"] = json!(["admin", "user"])
                }
                "user_view" => {
                    attributes.last_mut().unwrap()["permissions"]["view"] = json!(["user"])
                }
                _ => attributes.last_mut().unwrap()["multivalued"] = json!(true),
            }
            realm.lock().unwrap()["profile"] = profile;
            assert!(
                kc.verify_realm()
                    .await
                    .unwrap_err()
                    .to_string()
                    .contains("alondra_account_id")
            );
        }
        realm
            .lock()
            .unwrap()
            .as_object_mut()
            .unwrap()
            .remove("profile");
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
    async fn management_bound_reconciliation_never_falls_back_or_mutates_mismatched_identity()
    -> Result<()> {
        use axum::{
            Json, Router,
            extract::State,
            routing::{get, post},
        };
        use std::sync::{Arc, Mutex};
        #[derive(Default)]
        struct StateData {
            body: Option<Value>,
            writes: usize,
            searches: usize,
        }
        let state = Arc::new(Mutex::new(StateData::default()));
        let router = Router::new()
            .route(
                "/realms/test/protocol/openid-connect/token",
                post(|| async { Json(json!({"access_token":"synthetic"})) }),
            )
            .route(
                "/admin/realms/test/users/bound",
                get(|State(state): State<Arc<Mutex<StateData>>>| async move {
                    let body = state.lock().unwrap().body.clone();
                    (
                        if body.is_some() {
                            StatusCode::OK
                        } else {
                            StatusCode::NOT_FOUND
                        },
                        Json(body.unwrap_or(json!({}))),
                    )
                })
                .put(|State(state): State<Arc<Mutex<StateData>>>| async move {
                    state.lock().unwrap().writes += 1;
                    StatusCode::NO_CONTENT
                })
                .delete(|State(state): State<Arc<Mutex<StateData>>>| async move {
                    state.lock().unwrap().writes += 1;
                    StatusCode::NO_CONTENT
                }),
            )
            .route(
                "/admin/realms/test/users/bound/logout",
                post(|| async { StatusCode::NO_CONTENT }),
            )
            .route(
                "/admin/realms/test/users",
                get(|State(state): State<Arc<Mutex<StateData>>>| async move {
                    state.lock().unwrap().searches += 1;
                    Json(json!([]))
                }),
            )
            .with_state(state.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
        let base = format!("http://{}", listener.local_addr()?);
        let server = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
        let issuer = "https://identity.example/realms/test";
        let kc = Keycloak::new(
            issuer.into(),
            format!("{base}/admin/realms/test/").parse()?,
            format!("{base}/realms/test/protocol/openid-connect/token").parse()?,
            "fixture".into(),
            "synthetic".into(),
        )?;
        let mut job = Reconciliation {
            id: Uuid::new_v4(),
            account_id: Uuid::new_v4(),
            action: "keycloak_disable".into(),
            issuer: issuer.into(),
            subject: Some("bound".into()),
            allow_provisioning_lookup: false,
            allow_provisioned_cleanup: false,
        };
        for body in [
            json!({"id":"bound","username":"legacy-david"}),
            json!({"id":"bound","username":"renamed","attributes":{"alondra_account_id":[job.account_id.to_string()]}}),
        ] {
            state.lock().unwrap().body = Some(body);
            kc.disable(&job).await?;
            kc.delete(&job).await?;
        }
        assert_eq!(state.lock().unwrap().writes, 4);
        for body in [
            json!({"id":"other"}),
            json!({"id":"bound","attributes":{"alondra_account_id":[Uuid::new_v4().to_string()]}}),
            json!({"id":"bound","attributes":{"alondra_account_id":[job.account_id.to_string(),"extra"]}}),
        ] {
            state.lock().unwrap().body = Some(body);
            assert!(kc.disable(&job).await.is_err());
            assert!(kc.delete(&job).await.is_err());
        }
        state.lock().unwrap().body = None;
        kc.disable(&job).await?;
        kc.delete(&job).await?;
        job.issuer = "https://other.example/realms/test".into();
        assert!(kc.delete(&job).await.is_err());
        job.issuer = issuer.into();
        job.subject = None;
        assert!(kc.delete(&job).await.is_err());
        assert_eq!(state.lock().unwrap().writes, 4);
        assert_eq!(
            state.lock().unwrap().searches,
            0,
            "bound missing user or lost binding must not search by username"
        );
        job.allow_provisioning_lookup = true;
        kc.delete(&job).await?;
        assert_eq!(
            state.lock().unwrap().searches,
            1,
            "ambiguous create retains strict provisioning lookup"
        );
        assert!(
            Keycloak::new(
                issuer.into(),
                format!("{base}/admin/realms/other/").parse()?,
                format!("{base}/realms/test/protocol/openid-connect/token").parse()?,
                "fixture".into(),
                "synthetic".into()
            )
            .is_err()
        );
        server.abort();
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
            field("issuer")?,
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
                "keycloak_disable" => kc.disable(&job).await.context("real fixture disable")?,
                "keycloak_delete" => kc.delete(&job).await.context("real fixture delete")?,
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
        db.disable_account(admin.id, new_id, true).await?;
        while let Some(job) = db.next_reconciliation().await? {
            match job.action.as_str() {
                "keycloak_disable" => kc.disable(&job).await?,
                "keycloak_delete" => kc.delete(&job).await?,
                "relay_revoke" => (),
                _ => unreachable!(),
            }
            db.reconciliation_complete(&job).await?;
        }
        // Replay the provider commit ordering of an older timed-out create:
        // S1 was admitted and deleted, then that create commits S2. This is a
        // real provider creation, not a mock that shares the resolver state.
        let token = kc.token().await?;
        for correlated in [true, false] {
            let correlation = if correlated { new_id } else { Uuid::new_v4() };
            let response = kc.http.post(kc.admin_realm.join("users")?).bearer_auth(&token.0)
                .json(&json!({"username":format!("alondra-{new_id}"),"email":email,"firstName":"Late","enabled":true,
                    "attributes":{"alondra_account_id":[correlation.to_string()]}})).send().await?;
            ensure!(
                response.status() == StatusCode::CREATED,
                "late fixture create failed"
            );
            let location = Url::parse(
                response
                    .headers()
                    .get(reqwest::header::LOCATION)
                    .context("missing create location")?
                    .to_str()?,
            )?;
            let late_subject = location
                .path_segments()
                .and_then(|s| s.last())
                .context("missing late id")?;
            assert_ne!(late_subject, provisioned.subject);
            let mut late_url = kc.admin_realm.join("users/")?;
            late_url
                .path_segments_mut()
                .map_err(|_| DomainError::InvalidInput)?
                .pop_if_empty()
                .push(late_subject);
            sqlx::query("UPDATE management_outbox SET completed_at=clock_timestamp()-interval '6 minutes' WHERE account_id=$1")
                .bind(new_id).execute(&db.pool).await?;
            let mut jobs = 0;
            while let Some(job) = db.next_reconciliation().await? {
                assert_eq!(job.account_id, new_id);
                assert_eq!(job.subject.as_deref(), Some(provisioned.subject.as_str()));
                assert!(job.allow_provisioned_cleanup);
                assert!(!job.allow_provisioning_lookup);
                match job.action.as_str() {
                    "keycloak_disable" => kc.disable(&job).await?,
                    "keycloak_delete" => kc.delete(&job).await?,
                    "relay_revoke" => (),
                    _ => unreachable!(),
                }
                db.reconciliation_complete(&job).await?;
                jobs += 1;
            }
            assert_eq!(
                jobs, 3,
                "durable tombstones must retry the deleted admitted account"
            );
            let response = kc
                .http
                .get(late_url.clone())
                .bearer_auth(&token.0)
                .send()
                .await?;
            if correlated {
                assert_eq!(
                    response.status(),
                    StatusCode::NOT_FOUND,
                    "late S2 must be removed even though bound S1 is absent"
                );
            } else {
                assert_eq!(response.status(), StatusCode::OK);
                let user: Value = response.json().await?;
                assert_eq!(
                    user["enabled"], true,
                    "foreign owner of deterministic name remains untouched"
                );
                kc.http
                    .delete(late_url)
                    .bearer_auth(&token.0)
                    .send()
                    .await?
                    .error_for_status()?;
            }
            let retained: String =
                sqlx::query_scalar("SELECT subject FROM management_identities WHERE account_id=$1")
                    .bind(new_id)
                    .fetch_one(&db.pool)
                    .await?;
            assert_eq!(
                retained, provisioned.subject,
                "cleanup must never relink the identity"
            );
        }
        cleanup(db, schema).await
    }
    #[tokio::test]
    #[ignore = "requires disposable local PostgreSQL and Keycloak fixture"]
    async fn management_real_keycloak_legacy_subject_survives_rename_and_username_reuse()
    -> Result<()> {
        let fixture: Value = serde_json::from_slice(&std::fs::read(std::env::var(
            "ALONDRA_MANAGEMENT_TEST_CONNECTIONS",
        )?)?)?;
        ensure!(
            fixture["marker"] == "alondra-disposable-system-v1",
            "disposable fixture required"
        );
        let field = |name: &str| -> Result<String> {
            Ok(fixture[name]
                .as_str()
                .context("missing fixture field")?
                .into())
        };
        let realm: Url = field("admin_realm")?.parse()?;
        ensure!(
            matches!(realm.host_str(), Some("127.0.0.1" | "localhost" | "::1")),
            "fixture must be loopback"
        );
        let issuer = field("issuer")?;
        let kc = Keycloak::new(
            issuer.clone(),
            realm,
            field("token_url")?.parse()?,
            field("client_id")?,
            field("client_secret")?,
        )?;
        kc.verify_realm().await?;
        let token = kc.token().await?;
        let legacy_name = format!("legacy-david-{}", Uuid::new_v4());
        let response = kc.http.post(kc.admin_realm.join("users")?).bearer_auth(&token.0)
            .json(&json!({"username":legacy_name,"email":format!("{legacy_name}@example.invalid"),"firstName":"Legacy","enabled":true})).send().await?;
        ensure!(
            response.status() == StatusCode::CREATED,
            "legacy fixture user create failed"
        );
        let legacy_url = Url::parse(
            response
                .headers()
                .get(reqwest::header::LOCATION)
                .context("missing create location")?
                .to_str()?,
        )?;
        // Keep subsequent requests pinned to the configured authority, not Location.
        let subject = legacy_url
            .path_segments()
            .and_then(|s| s.last())
            .context("missing created id")?
            .to_string();
        let user_url = |id: &str| -> Result<Url> {
            let mut url = kc.admin_realm.join("users/")?;
            url.path_segments_mut()
                .map_err(|_| DomainError::InvalidInput)?
                .pop_if_empty()
                .push(id);
            Ok(url)
        };
        let (db, legacy, schema) =
            super::super::tests::fixture_for_identity(&issuer, &subject).await?;
        let invitation = db
            .create_invitation(
                legacy.id,
                &format!("operator-{}@example.invalid", Uuid::new_v4()),
                300,
            )
            .await?;
        let operator = db
            .reserve_acceptance(&invitation.token, "Operator")
            .await?
            .complete("fixture-operator-sub")
            .await?;
        sqlx::query("UPDATE management_accounts SET is_admin=true WHERE id=$1")
            .bind(operator.id)
            .execute(&db.pool)
            .await?;
        db.disable_account(operator.id, legacy.id, false).await?;
        while let Some(job) = db.next_reconciliation().await? {
            assert_eq!(job.subject.as_deref(), Some(subject.as_str()));
            assert!(!job.allow_provisioning_lookup);
            assert!(
                !job.allow_provisioned_cleanup,
                "legacy bootstrap has no invitation provenance"
            );
            if job.action == "keycloak_disable" {
                kc.disable(&job).await?;
            }
            db.reconciliation_complete(&job).await?;
        }
        let user: Value = kc
            .http
            .get(user_url(&subject)?)
            .bearer_auth(&token.0)
            .send()
            .await?
            .json()
            .await?;
        assert_eq!(
            user["enabled"], false,
            "legacy bootstrap identity must actually be disabled at Keycloak"
        );
        let renamed = format!("renamed-{}", Uuid::new_v4());
        let response = kc
            .http
            .put(user_url(&subject)?)
            .bearer_auth(&token.0)
            .json(&json!({"username":renamed}))
            .send()
            .await?;
        ensure!(
            response.status() == StatusCode::NO_CONTENT,
            "fixture rename failed"
        );
        let other_account = Uuid::new_v4();
        let inherited_name = format!("alondra-{}", legacy.id);
        let response = kc.http.post(kc.admin_realm.join("users")?).bearer_auth(&token.0)
            .json(&json!({"username":inherited_name,"email":format!("{inherited_name}@example.invalid"),"firstName":"Other","enabled":true,
                "attributes":{"alondra_account_id":[other_account.to_string()]}})).send().await?;
        ensure!(
            response.status() == StatusCode::CREATED,
            "fixture name reuse failed"
        );
        let other_location = Url::parse(
            response
                .headers()
                .get(reqwest::header::LOCATION)
                .context("missing create location")?
                .to_str()?,
        )?;
        let other_subject = other_location
            .path_segments()
            .and_then(|s| s.last())
            .context("missing created id")?
            .to_string();
        let mut wrong = Reconciliation {
            issuer: issuer.clone(),
            id: Uuid::new_v4(),
            account_id: legacy.id,
            action: "keycloak_delete".into(),
            subject: Some(other_subject.clone()),
            allow_provisioning_lookup: false,
            allow_provisioned_cleanup: false,
        };
        assert!(
            kc.disable(&wrong).await.is_err(),
            "another account correlation rejects stale/wrong binding"
        );
        assert!(kc.delete(&wrong).await.is_err());
        wrong.subject = None;
        assert!(
            kc.delete(&wrong).await.is_err(),
            "missing authoritative binding cannot adopt reused username"
        );
        wrong.subject = Some(subject.clone());
        wrong.issuer = "https://wrong-issuer.example/realms/test".into();
        assert!(kc.delete(&wrong).await.is_err());
        db.disable_account(operator.id, legacy.id, true).await?;
        while let Some(job) = db.next_reconciliation().await? {
            match job.action.as_str() {
                "keycloak_disable" => kc.disable(&job).await?,
                "keycloak_delete" => {
                    kc.delete(&job).await?;
                    kc.delete(&job).await?;
                }
                "relay_revoke" => (),
                _ => unreachable!(),
            }
            db.reconciliation_complete(&job).await?;
        }
        assert_eq!(
            kc.http
                .get(user_url(&subject)?)
                .bearer_auth(&token.0)
                .send()
                .await?
                .status(),
            StatusCode::NOT_FOUND
        );
        let other: Value = kc
            .http
            .get(user_url(&other_subject)?)
            .bearer_auth(&token.0)
            .send()
            .await?
            .json()
            .await?;
        assert_eq!(
            other["enabled"], true,
            "a user holding the predictable name must remain untouched"
        );
        assert_eq!(
            other["attributes"]["alondra_account_id"],
            json!([other_account.to_string()])
        );
        kc.http
            .delete(user_url(&other_subject)?)
            .bearer_auth(&token.0)
            .send()
            .await?
            .error_for_status()?;
        cleanup(db, schema).await
    }
}
