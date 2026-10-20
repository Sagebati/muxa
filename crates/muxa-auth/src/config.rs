//! The `[auth]` configuration section.

use std::collections::BTreeMap;

use secrecy::SecretString;
use serde::Deserialize;

/// Configuration for [`AuthPlugin`](crate::AuthPlugin), read from `[auth]`.
///
/// ```toml
/// [auth]
/// base_url = "https://example.com"   # the public origin the browser sees
/// mount = "/api/v1/auth"             # where the sign-in routes live
///
/// [auth.providers.google]
/// kind = "oidc"
/// issuer = "https://accounts.google.com"
/// client_id = "…"
/// # client_secret from <APP>_AUTH__PROVIDERS__GOOGLE__CLIENT_SECRET
/// ```
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct AuthConfig {
    /// The public origin the browser uses, e.g. `https://example.com`.
    ///
    /// Redirect URLs are built from it (`{base_url}{mount}/{provider}/callback`)
    /// and it is always a trusted origin for the cross-origin check. Required
    /// once any provider is configured.
    pub base_url: String,
    /// Path prefix of the sign-in routes (`{mount}/{provider}/start`, …).
    pub mount: String,
    /// Where a failed sign-in lands, with the error code in [`error_param`](Self::error_param)
    /// and the original `next` kept.
    pub sign_in_path: String,
    /// Where a sign-in lands when the caller gave no (or an unsafe) `next`.
    pub default_next: String,
    /// Query parameter that carries a sign-in error code.
    pub error_param: String,
    /// Origins besides `base_url`'s that may send state-changing requests,
    /// e.g. a dev server on another port.
    pub trusted_origins: Vec<String>,
    /// The session cookie.
    pub cookie: CookieConfig,
    /// How often expired sessions are swept from the store, in seconds.
    pub cleanup_interval_secs: u64,
    /// Sign-in providers, keyed by the id used in their URLs.
    pub providers: BTreeMap<String, ProviderConfig>,
}

impl Default for AuthConfig {
    fn default() -> Self {
        Self {
            base_url: String::new(),
            mount: "/auth".to_owned(),
            sign_in_path: "/".to_owned(),
            default_next: "/".to_owned(),
            error_param: "auth_error".to_owned(),
            trusted_origins: Vec::new(),
            cookie: CookieConfig::default(),
            cleanup_interval_secs: 3600,
            providers: BTreeMap::new(),
        }
    }
}

/// The session cookie's attributes. It is always `HttpOnly`.
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct CookieConfig {
    /// Cookie name.
    pub name: String,
    /// Send the cookie over HTTPS only. Turn off for plain-HTTP local development.
    pub secure: bool,
    /// `SameSite` attribute. `lax` keeps the cookie on the provider's redirect
    /// back, which `strict` would drop.
    pub same_site: SameSite,
    /// Sign-out after this many seconds without a request.
    pub ttl_secs: i64,
}

impl Default for CookieConfig {
    fn default() -> Self {
        Self {
            name: "session".to_owned(),
            secure: true,
            same_site: SameSite::Lax,
            ttl_secs: 30 * 24 * 60 * 60,
        }
    }
}

/// The cookie `SameSite` attribute.
#[derive(Debug, Clone, Copy, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum SameSite {
    /// `SameSite=Strict`.
    Strict,
    /// `SameSite=Lax`.
    #[default]
    Lax,
    /// `SameSite=None` (requires `secure`).
    None,
}

/// One sign-in provider.
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "kind")]
pub enum ProviderConfig {
    /// A plain OAuth2 provider: identity comes from a userinfo endpoint.
    #[serde(rename = "oauth2")]
    OAuth2(OAuth2ProviderConfig),
    /// An OpenID Connect provider: identity comes from a verified ID token.
    #[serde(rename = "oidc")]
    Oidc(OidcProviderConfig),
}

impl ProviderConfig {
    /// The client id, which also decides whether the provider is on.
    #[must_use]
    pub fn client_id(&self) -> &str {
        match self {
            Self::OAuth2(cfg) => &cfg.client_id,
            Self::Oidc(cfg) => &cfg.client_id,
        }
    }
}

/// How the client authenticates at the token endpoint.
#[derive(Debug, Clone, Copy, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum TokenAuth {
    /// HTTP Basic (`client_secret_basic`).
    #[default]
    Basic,
    /// Form body (`client_secret_post`).
    Post,
}

/// A plain OAuth2 provider (authorization-code flow with PKCE).
///
/// Off while `client_id` is empty.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct OAuth2ProviderConfig {
    /// OAuth client id. Empty turns the provider off.
    pub client_id: String,
    /// OAuth client secret. Empty for a public client.
    pub client_secret: SecretString,
    /// Authorization endpoint.
    pub auth_url: String,
    /// Token endpoint.
    pub token_url: String,
    /// Endpoint returning the signed-in account as JSON, called with the access token.
    pub userinfo_url: String,
    /// Scopes to request.
    pub scopes: Vec<String>,
    /// Use PKCE (S256). On unless the provider rejects it.
    #[serde(default = "yes")]
    pub pkce: bool,
    /// Token endpoint client authentication.
    pub token_auth: TokenAuth,
    /// Userinfo field holding the stable account id (string or number).
    #[serde(default = "default_subject_field")]
    pub subject_field: String,
    /// Userinfo field holding an email, if the provider has one.
    pub email_field: Option<String>,
    /// Userinfo field holding a display name.
    pub name_field: Option<String>,
    /// Override the redirect URL derived from `base_url` and `mount`.
    pub redirect_url: Option<String>,
}

/// An OpenID Connect provider (discovery, authorization-code flow with PKCE,
/// ID token checked for signature, issuer, audience, expiry and nonce).
///
/// Off while `client_id` is empty.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct OidcProviderConfig {
    /// Issuer URL; discovery reads `{issuer}/.well-known/openid-configuration`.
    pub issuer: String,
    /// OAuth client id. Empty turns the provider off.
    pub client_id: String,
    /// OAuth client secret.
    pub client_secret: SecretString,
    /// Scopes besides `openid`.
    #[serde(default = "default_oidc_scopes")]
    pub scopes: Vec<String>,
    /// Re-read the discovery document (and so the signing keys) after this many seconds.
    #[serde(default = "default_metadata_ttl")]
    pub metadata_ttl_secs: u64,
    /// Override the redirect URL derived from `base_url` and `mount`.
    pub redirect_url: Option<String>,
}

fn yes() -> bool {
    true
}

fn default_subject_field() -> String {
    "id".to_owned()
}

fn default_oidc_scopes() -> Vec<String> {
    vec!["email".to_owned(), "profile".to_owned()]
}

fn default_metadata_ttl() -> u64 {
    3600
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn providers_deserialize_by_kind() {
        let cfg: AuthConfig = toml_from(
            r#"
            base_url = "https://example.com"
            [providers.ogs]
            kind = "oauth2"
            client_id = "abc"
            auth_url = "https://ogs/authorize"
            token_url = "https://ogs/token"
            userinfo_url = "https://ogs/me"
            token_auth = "post"
            [providers.google]
            kind = "oidc"
            issuer = "https://accounts.google.com"
            client_id = "def"
            "#,
        );
        let Some(ProviderConfig::OAuth2(ogs)) = cfg.providers.get("ogs") else {
            panic!("ogs should be oauth2");
        };
        assert!(ogs.pkce);
        assert_eq!(ogs.token_auth, TokenAuth::Post);
        assert_eq!(ogs.subject_field, "id");
        let Some(ProviderConfig::Oidc(google)) = cfg.providers.get("google") else {
            panic!("google should be oidc");
        };
        assert_eq!(google.scopes, ["email", "profile"]);
        assert_eq!(cfg.mount, "/auth");
    }

    fn toml_from(src: &str) -> AuthConfig {
        toml::from_str(src).unwrap()
    }
}
