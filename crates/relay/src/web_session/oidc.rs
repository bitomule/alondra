//! OIDC code flow. All provider requests are bounded, redirect-free, and restricted
//! to the configured issuer's origin, including discovery-supplied JWKS URLs.
use super::store::{FlowSecrets, SessionSecrets};
use anyhow::{Context, Result, ensure};
use openidconnect::{
    AccessTokenHash, AuthorizationCode, ClientId, ClientSecret, CsrfToken, EndpointMaybeSet,
    EndpointNotSet, EndpointSet, IssuerUrl, Nonce, OAuth2TokenResponse, PkceCodeChallenge,
    PkceCodeVerifier, RedirectUrl, RefreshToken, TokenResponse,
    core::{CoreAuthenticationFlow, CoreClient, CoreProviderMetadata, CoreTokenResponse},
};
use reqwest::Url;
use std::time::Duration;

pub(super) type OidcClient = CoreClient<
    EndpointSet,
    EndpointNotSet,
    EndpointNotSet,
    EndpointNotSet,
    EndpointMaybeSet,
    EndpointMaybeSet,
>;

pub struct Provider {
    issuer: IssuerUrl,
    origin: String,
    client_id: String,
    client_secret: String,
    redirect: RedirectUrl,
    http: reqwest::Client,
}
pub struct Login {
    pub url: String,
    pub state: String,
    pub secrets: FlowSecrets,
}
pub struct Tokens {
    pub subject: String,
    pub access: String,
    pub refresh: String,
    pub expires_ms: i64,
    pub valid_id: bool,
}

pub fn allowed_origin(value: &Url) -> Result<String> {
    ensure!(
        value.username().is_empty() && value.password().is_none() && value.fragment().is_none(),
        "invalid OIDC URL"
    );
    ensure!(
        value.scheme() == "https"
            || value.scheme() == "http"
                && matches!(value.host_str(), Some("127.0.0.1" | "localhost" | "[::1]")),
        "OIDC needs HTTPS outside loopback"
    );
    Ok(value.origin().ascii_serialization())
}

impl Provider {
    pub fn new(
        issuer: &str,
        public_origin: &str,
        client_id: String,
        client_secret: String,
    ) -> Result<Self> {
        let issuer_url = Url::parse(issuer)?;
        let origin = allowed_origin(&issuer_url)?;
        ensure!(
            issuer_url.query().is_none(),
            "issuer cannot contain query parameters"
        );
        let public = Url::parse(public_origin)?;
        let web_origin = allowed_origin(&public)?;
        ensure!(
            public.path() == "/"
                && public.query().is_none()
                && !client_id.is_empty()
                && !client_secret.is_empty(),
            "invalid web OIDC configuration"
        );
        let http = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(Duration::from_secs(8))
            .build()?;
        Ok(Self {
            issuer: IssuerUrl::new(issuer.into())?,
            origin,
            client_id,
            client_secret,
            redirect: RedirectUrl::new(format!("{web_origin}/auth/callback"))?,
            http,
        })
    }
    async fn request(
        &self,
        request: openidconnect::HttpRequest,
    ) -> std::result::Result<openidconnect::HttpResponse, std::io::Error> {
        use std::io::{Error, ErrorKind};
        let invalid = || Error::new(ErrorKind::InvalidData, "OIDC provider request failed");
        let url = Url::parse(&request.uri().to_string()).map_err(|_| invalid())?;
        if allowed_origin(&url).map_err(|_| invalid())? != self.origin {
            return Err(invalid());
        }
        let mut response = self
            .http
            .request(request.method().clone(), url)
            .headers(request.headers().clone())
            .body(request.body().clone())
            .send()
            .await
            .map_err(|_| invalid())?;
        if response.content_length().is_some_and(|n| n > 128 * 1024) {
            return Err(invalid());
        }
        let status = response.status();
        let headers = response.headers().clone();
        let mut body = Vec::new();
        while let Some(chunk) = response.chunk().await.map_err(|_| invalid())? {
            if body.len() + chunk.len() > 128 * 1024 {
                return Err(invalid());
            }
            body.extend_from_slice(&chunk);
        }
        let mut result = openidconnect::http::Response::builder()
            .status(status)
            .body(body)
            .map_err(|_| invalid())?;
        *result.headers_mut() = headers;
        Ok(result)
    }
    async fn client(&self) -> Result<OidcClient> {
        let metadata = CoreProviderMetadata::discover_async(self.issuer.clone(), self)
            .await
            .map_err(|_| anyhow::anyhow!("OIDC discovery unavailable"))?;
        // Request-time origin enforcement above also covers JWKS discovery.
        let auth = Url::parse(metadata.authorization_endpoint().as_str())?;
        let token = Url::parse(
            metadata
                .token_endpoint()
                .context("OIDC token endpoint missing")?
                .as_str(),
        )?;
        ensure!(
            allowed_origin(&auth)? == self.origin && allowed_origin(&token)? == self.origin,
            "OIDC endpoint origin mismatch"
        );
        Ok(CoreClient::from_provider_metadata(
            metadata,
            ClientId::new(self.client_id.clone()),
            Some(ClientSecret::new(self.client_secret.clone())),
        )
        .set_redirect_uri(self.redirect.clone()))
    }
    pub async fn login(&self) -> Result<Login> {
        let client = self.client().await?;
        let (challenge, verifier) = PkceCodeChallenge::new_random_sha256();
        let (url, state, nonce) = client
            .authorize_url(
                CoreAuthenticationFlow::AuthorizationCode,
                CsrfToken::new_random,
                Nonce::new_random,
            )
            .set_pkce_challenge(challenge)
            .url();
        Ok(Login {
            url: url.to_string(),
            state: state.secret().clone(),
            secrets: FlowSecrets {
                verifier: verifier.secret().clone(),
                nonce: nonce.secret().clone(),
            },
        })
    }
    pub async fn exchange(&self, code: String, flow: &FlowSecrets, now: i64) -> Result<Tokens> {
        ensure!(
            !code.is_empty() && code.len() <= 8192,
            "invalid authorization code"
        );
        let client = self.client().await?;
        let response = client
            .exchange_code(AuthorizationCode::new(code))?
            .set_pkce_verifier(PkceCodeVerifier::new(flow.verifier.clone()))
            .request_async(self)
            .await
            .map_err(|_| anyhow::anyhow!("OIDC exchange failed"))?;
        let subject =
            verify_id(&client, &response, &flow.nonce, false)?.context("OIDC ID token missing")?;
        tokens(response, subject, None, now)
    }
    pub async fn prepare_refresh(&self) -> Result<OidcClient> {
        self.client().await
    }
    pub async fn refresh(
        &self,
        client: &OidcClient,
        old: &SessionSecrets,
        subject: &str,
        now: i64,
    ) -> Result<Tokens> {
        let refresh = RefreshToken::new(old.refresh_token.clone());
        let response = client
            .exchange_refresh_token(&refresh)?
            .request_async(self)
            .await
            .map_err(|_| anyhow::anyhow!("OIDC refresh failed"))?;
        // Return issued credentials even on a bad ID token so the caller can
        // durably deny and clean up that rotation. Never activate the session.
        let valid_id = match verify_id(client, &response, &old.nonce, true) {
            Ok(Some(returned)) => returned == subject,
            Ok(None) => true,
            Err(_) => false,
        };
        let mut result = tokens(
            response,
            subject.into(),
            Some(old.refresh_token.clone()),
            now,
        )?;
        result.valid_id = valid_id;
        Ok(result)
    }
}
fn verify_id(
    client: &OidcClient,
    response: &CoreTokenResponse,
    nonce: &str,
    refresh: bool,
) -> Result<Option<String>> {
    let Some(id) = response.id_token() else {
        return Ok(None);
    };
    let verifier = client.id_token_verifier();
    let nonce = Nonce::new(nonce.into());
    let claims = id
        .claims(&verifier, |claim: Option<&Nonce>| {
            validate_nonce(&nonce, claim, refresh)
        })
        .map_err(|_| anyhow::anyhow!("OIDC claims invalid"))?;
    if let Some(expected) = claims.access_token_hash() {
        let actual = AccessTokenHash::from_token(
            response.access_token(),
            id.signing_alg()?,
            id.signing_key(&verifier)?,
        )?;
        ensure!(actual == *expected, "OIDC access token hash mismatch");
    }
    Ok(Some(claims.subject().as_str().into()))
}
fn tokens(
    response: CoreTokenResponse,
    subject: String,
    previous_refresh: Option<String>,
    now: i64,
) -> Result<Tokens> {
    let expires = response
        .expires_in()
        .context("OIDC token lifetime missing")?
        .as_millis();
    ensure!(
        expires > 0 && expires <= 24 * 60 * 60 * 1000,
        "invalid token lifetime"
    );
    let refresh = response
        .refresh_token()
        .map(|value| value.secret().clone())
        .or(previous_refresh)
        .context("OIDC refresh token missing")?;
    let access = response.access_token().secret().clone();
    ensure!(
        !subject.is_empty() && access.len() <= 8192 && refresh.len() <= 16384,
        "invalid OIDC tokens"
    );
    Ok(Tokens {
        subject,
        access,
        refresh,
        expires_ms: now + expires as i64,
        valid_id: true,
    })
}

impl<'c> openidconnect::AsyncHttpClient<'c> for Provider {
    type Error = std::io::Error;
    type Future = std::pin::Pin<
        Box<
            dyn std::future::Future<
                    Output = std::result::Result<openidconnect::HttpResponse, Self::Error>,
                > + Send
                + 'c,
        >,
    >;
    fn call(&'c self, request: openidconnect::HttpRequest) -> Self::Future {
        Box::pin(self.request(request))
    }
}

// OIDC Core 12.2: refreshed ID tokens normally omit nonce; if present it must
// match the original authentication. Authorization-code exchange still requires it.
fn validate_nonce(
    expected: &Nonce,
    claim: Option<&Nonce>,
    refresh: bool,
) -> std::result::Result<(), String> {
    use openidconnect::NonceVerifier;
    if refresh && claim.is_none() {
        Ok(())
    } else {
        expected.verify(claim)
    }
}
#[cfg(test)]
mod nonce_tests {
    use super::*;
    #[test]
    fn refresh_only_allows_absent_nonce_never_a_different_nonce() {
        let expected = Nonce::new("synthetic-original".into());
        let different = Nonce::new("synthetic-other".into());
        assert!(validate_nonce(&expected, None, true).is_ok());
        assert!(validate_nonce(&expected, None, false).is_err());
        for refresh in [false, true] {
            assert!(validate_nonce(&expected, Some(&expected), refresh).is_ok());
            assert!(validate_nonce(&expected, Some(&different), refresh).is_err());
        }
    }
}
