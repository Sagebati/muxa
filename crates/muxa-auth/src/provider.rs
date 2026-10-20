//! The two kinds of sign-in provider and their authorization-code flows.

use std::time::SystemTime;

use oauth2::basic::BasicClient;
use oauth2::{
    AuthType, AuthUrl, AuthorizationCode, ClientId, ClientSecret, CsrfToken, EndpointNotSet,
    EndpointSet, PkceCodeChallenge, PkceCodeVerifier, RedirectUrl, Scope, TokenResponse as _,
    TokenUrl,
};
use secrecy::ExposeSecret as _;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use url::Url;

use crate::config::{OAuth2ProviderConfig, ProviderConfig, TokenAuth};
use crate::http_client::HttpClient;
use crate::identity::ExternalIdentity;

/// A sign-in in progress, kept in the session between `start` and `callback`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PendingLogin {
    pub provider: String,
    pub state: String,
    pub pkce_verifier: Option<String>,
    pub nonce: Option<String>,
    pub next: String,
    pub link: bool,
    /// Unix seconds; attempts older than [`PENDING_TTL_SECS`] are refused.
    pub created_at: u64,
}

/// How long a started sign-in stays valid.
pub const PENDING_TTL_SECS: u64 = 10 * 60;

pub fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_secs())
}

/// Why a provider couldn't complete a sign-in. Logged, never shown.
#[derive(Debug, thiserror::Error)]
#[error("{0}")]
pub struct ProviderError(String);

impl ProviderError {
    fn new(context: &str, err: impl std::fmt::Display) -> Self {
        Self(format!("{context}: {err}"))
    }
}

/// A configured, enabled provider.
/// Boxed: the clients are large, and built once.
pub enum Provider {
    OAuth2(Box<OAuth2Provider>),
    #[cfg(feature = "oidc")]
    Oidc(Box<oidc::OidcProvider>),
}

/// The URL to send the browser to, and what to remember until it comes back.
pub struct Authorization {
    pub url: Url,
    pub state: String,
    pub pkce_verifier: Option<String>,
    pub nonce: Option<String>,
}

impl Provider {
    /// Build provider `id` from its configuration.
    ///
    /// `Ok(None)` when it is turned off (no client id); `Err` when it is
    /// configured but unusable.
    pub fn from_config(
        id: &str,
        cfg: &ProviderConfig,
        redirect_url: String,
    ) -> Result<Option<Self>, ProviderError> {
        if cfg.client_id().is_empty() {
            return Ok(None);
        }
        let redirect = RedirectUrl::new(redirect_url)
            .map_err(|err| ProviderError::new("redirect url", err))?;
        match cfg {
            ProviderConfig::OAuth2(cfg) => OAuth2Provider::new(id, cfg, redirect)
                .map(|provider| Self::OAuth2(Box::new(provider)))
                .map(Some),
            #[cfg(feature = "oidc")]
            ProviderConfig::Oidc(cfg) => oidc::OidcProvider::new(id, cfg, redirect)
                .map(|provider| Self::Oidc(Box::new(provider)))
                .map(Some),
            #[cfg(not(feature = "oidc"))]
            ProviderConfig::Oidc(_) => Err(ProviderError(
                "an OpenID Connect provider needs muxa-auth's `oidc` feature".to_owned(),
            )),
        }
    }

    pub async fn authorize(&self, http: &HttpClient) -> Result<Authorization, ProviderError> {
        #[cfg(not(feature = "oidc"))]
        let _ = http;
        match self {
            Self::OAuth2(provider) => Ok(provider.authorize()),
            #[cfg(feature = "oidc")]
            Self::Oidc(provider) => provider.authorize(http).await,
        }
    }

    pub async fn identify(
        &self,
        code: String,
        pending: &PendingLogin,
        http: &HttpClient,
    ) -> Result<ExternalIdentity, ProviderError> {
        match self {
            Self::OAuth2(provider) => provider.identify(code, pending, http).await,
            #[cfg(feature = "oidc")]
            Self::Oidc(provider) => provider.identify(code, pending, http).await,
        }
    }
}

type OAuth2Client =
    BasicClient<EndpointSet, EndpointNotSet, EndpointNotSet, EndpointNotSet, EndpointSet>;

pub struct OAuth2Provider {
    id: String,
    client: OAuth2Client,
    scopes: Vec<String>,
    pkce: bool,
    userinfo_url: Url,
    subject_field: String,
    email_field: Option<String>,
    name_field: Option<String>,
}

impl OAuth2Provider {
    fn new(
        id: &str,
        cfg: &OAuth2ProviderConfig,
        redirect: RedirectUrl,
    ) -> Result<Self, ProviderError> {
        let auth_url = AuthUrl::new(cfg.auth_url.clone())
            .map_err(|err| ProviderError::new("auth_url", err))?;
        let token_url = TokenUrl::new(cfg.token_url.clone())
            .map_err(|err| ProviderError::new("token_url", err))?;
        let userinfo_url =
            Url::parse(&cfg.userinfo_url).map_err(|err| ProviderError::new("userinfo_url", err))?;

        let mut client = BasicClient::new(ClientId::new(cfg.client_id.clone()))
            .set_auth_uri(auth_url)
            .set_token_uri(token_url)
            .set_redirect_uri(redirect)
            .set_auth_type(match cfg.token_auth {
                TokenAuth::Basic => AuthType::BasicAuth,
                TokenAuth::Post => AuthType::RequestBody,
            });
        let secret = cfg.client_secret.expose_secret();
        if !secret.is_empty() {
            client = client.set_client_secret(ClientSecret::new(secret.to_owned()));
        }

        Ok(Self {
            id: id.to_owned(),
            client,
            scopes: cfg.scopes.clone(),
            pkce: cfg.pkce,
            userinfo_url,
            subject_field: cfg.subject_field.clone(),
            email_field: cfg.email_field.clone(),
            name_field: cfg.name_field.clone(),
        })
    }

    fn authorize(&self) -> Authorization {
        let mut request = self.client.authorize_url(CsrfToken::new_random);
        for scope in &self.scopes {
            request = request.add_scope(Scope::new(scope.clone()));
        }
        let mut pkce_verifier = None;
        if self.pkce {
            let (challenge, verifier) = PkceCodeChallenge::new_random_sha256();
            request = request.set_pkce_challenge(challenge);
            pkce_verifier = Some(verifier.secret().clone());
        }
        let (url, state) = request.url();
        Authorization {
            url,
            state: state.secret().clone(),
            pkce_verifier,
            nonce: None,
        }
    }

    async fn identify(
        &self,
        code: String,
        pending: &PendingLogin,
        http: &HttpClient,
    ) -> Result<ExternalIdentity, ProviderError> {
        let mut request = self.client.exchange_code(AuthorizationCode::new(code));
        if let Some(verifier) = &pending.pkce_verifier {
            request = request.set_pkce_verifier(PkceCodeVerifier::new(verifier.clone()));
        }
        let token = request
            .request_async(http)
            .await
            .map_err(|err| ProviderError::new("token exchange", error_chain(&err)))?;

        let profile: Value = http
            .inner()
            .get(self.userinfo_url.clone())
            .bearer_auth(token.access_token().secret())
            .header(http::header::ACCEPT, "application/json")
            .send()
            .await
            .and_then(reqwest::Response::error_for_status)
            .map_err(|err| ProviderError::new("userinfo", err))?
            .json()
            .await
            .map_err(|err| ProviderError::new("userinfo body", err))?;

        let subject = match profile.get(&self.subject_field) {
            Some(Value::String(id)) if !id.is_empty() => id.clone(),
            Some(Value::Number(id)) => id.to_string(),
            _ => {
                return Err(ProviderError(format!(
                    "userinfo has no `{}` field",
                    self.subject_field
                )));
            }
        };
        let string_field = |field: &Option<String>| {
            field
                .as_deref()
                .and_then(|name| profile.get(name))
                .and_then(Value::as_str)
                .filter(|value| !value.is_empty())
                .map(str::to_owned)
        };

        Ok(ExternalIdentity {
            provider: self.id.clone(),
            subject,
            email: string_field(&self.email_field),
            // A plain OAuth2 provider makes no claim about its emails.
            email_verified: false,
            name: string_field(&self.name_field),
            profile,
        })
    }
}

/// An error and its sources on one line, for the log.
fn error_chain(err: &dyn std::error::Error) -> String {
    let mut out = err.to_string();
    let mut source = err.source();
    while let Some(cause) = source {
        out.push_str(": ");
        out.push_str(&cause.to_string());
        source = cause.source();
    }
    out
}

#[cfg(feature = "oidc")]
mod oidc {
    use std::time::{Duration, Instant};

    use openidconnect::core::{CoreAuthenticationFlow, CoreClient, CoreProviderMetadata};
    use openidconnect::{
        AccessTokenHash, AuthorizationCode, ClientId, ClientSecret, CsrfToken, EndpointMaybeSet,
        EndpointNotSet, EndpointSet, IssuerUrl, Nonce, OAuth2TokenResponse as _, PkceCodeChallenge,
        PkceCodeVerifier, RedirectUrl, Scope, TokenResponse as _,
    };
    use secrecy::{ExposeSecret as _, SecretString};
    use tokio::sync::RwLock;

    use super::{Authorization, PendingLogin, ProviderError, error_chain};
    use crate::config::OidcProviderConfig;
    use crate::http_client::HttpClient;
    use crate::identity::ExternalIdentity;

    type OidcClient = CoreClient<
        EndpointSet,
        EndpointNotSet,
        EndpointNotSet,
        EndpointNotSet,
        EndpointMaybeSet,
        EndpointMaybeSet,
    >;

    pub struct OidcProvider {
        id: String,
        issuer: IssuerUrl,
        client_id: String,
        client_secret: SecretString,
        redirect: RedirectUrl,
        scopes: Vec<String>,
        ttl: Duration,
        /// The client built from the last discovery, and when. Rebuilt after
        /// `ttl`, so rotated signing keys are picked up; discovery is lazy, so
        /// a provider outage at startup doesn't stop the app.
        client: RwLock<Option<(OidcClient, Instant)>>,
    }

    impl OidcProvider {
        pub(super) fn new(
            id: &str,
            cfg: &OidcProviderConfig,
            redirect: RedirectUrl,
        ) -> Result<Self, ProviderError> {
            let issuer = IssuerUrl::new(cfg.issuer.clone())
                .map_err(|err| ProviderError::new("issuer", err))?;
            Ok(Self {
                id: id.to_owned(),
                issuer,
                client_id: cfg.client_id.clone(),
                client_secret: cfg.client_secret.clone(),
                redirect,
                scopes: cfg.scopes.clone(),
                ttl: Duration::from_secs(cfg.metadata_ttl_secs),
                client: RwLock::new(None),
            })
        }

        async fn client(
            &self,
            http: &HttpClient,
            force: bool,
        ) -> Result<OidcClient, ProviderError> {
            if !force
                && let Some((client, at)) = &*self.client.read().await
                && at.elapsed() < self.ttl
            {
                return Ok(client.clone());
            }
            let metadata = CoreProviderMetadata::discover_async(self.issuer.clone(), http)
                .await
                .map_err(|err| ProviderError::new("discovery", error_chain(&err)))?;
            let secret = self.client_secret.expose_secret();
            let client = CoreClient::from_provider_metadata(
                metadata,
                ClientId::new(self.client_id.clone()),
                (!secret.is_empty()).then(|| ClientSecret::new(secret.to_owned())),
            )
            .set_redirect_uri(self.redirect.clone());
            *self.client.write().await = Some((client.clone(), Instant::now()));
            Ok(client)
        }

        pub(super) async fn authorize(
            &self,
            http: &HttpClient,
        ) -> Result<Authorization, ProviderError> {
            let client = self.client(http, false).await?;
            let (challenge, verifier) = PkceCodeChallenge::new_random_sha256();
            let mut request = client
                .authorize_url(
                    CoreAuthenticationFlow::AuthorizationCode,
                    CsrfToken::new_random,
                    Nonce::new_random,
                )
                .set_pkce_challenge(challenge);
            for scope in &self.scopes {
                request = request.add_scope(Scope::new(scope.clone()));
            }
            let (url, state, nonce) = request.url();
            Ok(Authorization {
                url,
                state: state.secret().clone(),
                pkce_verifier: Some(verifier.secret().clone()),
                nonce: Some(nonce.secret().clone()),
            })
        }

        pub(super) async fn identify(
            &self,
            code: String,
            pending: &PendingLogin,
            http: &HttpClient,
        ) -> Result<ExternalIdentity, ProviderError> {
            let nonce = Nonce::new(
                pending
                    .nonce
                    .clone()
                    .ok_or_else(|| ProviderError("sign-in has no nonce".to_owned()))?,
            );
            let mut client = self.client(http, false).await?;
            let mut request = client
                .exchange_code(AuthorizationCode::new(code))
                .map_err(|err| ProviderError::new("token endpoint", err))?;
            if let Some(verifier) = &pending.pkce_verifier {
                request = request.set_pkce_verifier(PkceCodeVerifier::new(verifier.clone()));
            }
            let token = request
                .request_async(http)
                .await
                .map_err(|err| ProviderError::new("token exchange", error_chain(&err)))?;
            let id_token = token
                .id_token()
                .ok_or_else(|| ProviderError("token response has no id_token".to_owned()))?;

            // A failure may be a signing key rotated since discovery: rediscover
            // once and check again before giving up.
            let first_check = {
                let verifier = client.id_token_verifier();
                id_token.claims(&verifier, &nonce).cloned()
            };
            let claims = match first_check {
                Ok(claims) => claims,
                Err(first) => {
                    tracing::debug!(provider = %self.id, error = %first, "ID token check failed; rediscovering");
                    client = self.client(http, true).await?;
                    let verifier = client.id_token_verifier();
                    id_token
                        .claims(&verifier, &nonce)
                        .map_err(|err| ProviderError::new("id token", err))?
                        .clone()
                }
            };

            if let Some(expected) = claims.access_token_hash() {
                let verifier = client.id_token_verifier();
                let actual = AccessTokenHash::from_token(
                    token.access_token(),
                    id_token
                        .signing_alg()
                        .map_err(|err| ProviderError::new("id token alg", err))?,
                    id_token
                        .signing_key(&verifier)
                        .map_err(|err| ProviderError::new("id token key", err))?,
                )
                .map_err(|err| ProviderError::new("at_hash", err))?;
                if actual != *expected {
                    return Err(ProviderError(
                        "at_hash does not match the access token".to_owned(),
                    ));
                }
            }

            Ok(ExternalIdentity {
                provider: self.id.clone(),
                subject: claims.subject().as_str().to_owned(),
                email: claims.email().map(|email| email.as_str().to_owned()),
                email_verified: claims.email_verified().unwrap_or(false),
                name: claims
                    .name()
                    .and_then(|name| name.get(None))
                    .map(|name| name.as_str().to_owned()),
                profile: serde_json::to_value(&claims).unwrap_or_default(),
            })
        }
    }
}
