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
