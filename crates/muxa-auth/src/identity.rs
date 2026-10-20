//! What a provider tells us about the person signing in, and how the app turns
//! that into one of its users.

use std::borrow::Cow;
use std::future::Future;

use serde_json::Value;

/// An account at a sign-in provider, as established by a completed sign-in.
///
/// For an OpenID Connect provider every field comes from the verified ID token.
/// For a plain OAuth2 provider they come from its userinfo endpoint, fetched
/// with the access token over TLS; the token itself is used once and dropped.
#[derive(Debug, Clone)]
pub struct ExternalIdentity {
    /// The provider id, as in the URLs and the `[auth.providers.<id>]` key.
    pub provider: String,
    /// The account's stable id at the provider (`sub`, or the configured
    /// `subject_field`). Pair it with `provider` to key a stored identity.
    pub subject: String,
    /// The account's email, when the provider shares one.
    pub email: Option<String>,
    /// Whether the provider vouches for `email`. Always `false` for plain
    /// OAuth2 providers, which make no such claim.
    pub email_verified: bool,
    /// A display name, when the provider shares one.
    pub name: Option<String>,
    /// Everything the provider returned: the ID-token claims or the userinfo body.
    pub profile: Value,
}

/// Context for [`IdentityResolver::resolve`].
#[derive(Debug)]
pub struct ResolveContext<'a, U> {
    /// The user already signed in on this session, if any.
    pub current_user: Option<&'a U>,
    /// The sign-in was started with `link=1`: the caller wants this identity
    /// attached to `current_user` (always `Some` in that case) rather than
    /// signing in as whoever owns it.
    pub link: bool,
}

/// A successful resolution: who to sign in, and what to tell the page.
#[derive(Debug, Clone)]
pub struct Resolved<U> {
    /// The user to sign in on this session.
    pub user: U,
    /// Query parameters appended to the `next` path, e.g. `("linked", "ogs")`.
    pub query: Vec<(String, String)>,
}

impl<U> Resolved<U> {
    /// Sign in as `user` with nothing extra on the redirect.
    pub fn new(user: U) -> Self {
        Self {
            user,
            query: Vec::new(),
        }
    }

    /// Append `key=value` to the redirect.
    #[must_use]
    pub fn with_query(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.query.push((key.into(), value.into()));
        self
    }
}

/// A refused sign-in. The browser is sent back with `code` in the
/// configured error parameter (or [`Refusal::param`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Refusal {
    code: Cow<'static, str>,
    param: Option<Cow<'static, str>>,
}

impl Refusal {
    /// The person cancelled at the provider.
    pub const CANCELLED: &'static str = "cancelled";
    /// The sign-in attempt is unknown, used, or too old (or the cookie was lost).
    pub const EXPIRED: &'static str = "expired";
    /// The provider failed or returned something unusable.
    pub const PROVIDER_FAILED: &'static str = "provider_failed";
    /// No such provider, or it is turned off.
    pub const UNAVAILABLE: &'static str = "unavailable";
    /// A `link=1` sign-in without a signed-in user.
    pub const SIGNED_OUT: &'static str = "signed_out";
    /// Something on our side broke.
    pub const FAILED: &'static str = "failed";

    /// Refuse with `code`.
    pub fn new(code: impl Into<Cow<'static, str>>) -> Self {
        Self {
            code: code.into(),
            param: None,
        }
    }

    /// Put the code in `param` instead of the configured error parameter.
    #[must_use]
    pub fn in_param(mut self, param: impl Into<Cow<'static, str>>) -> Self {
        self.param = Some(param.into());
        self
    }

    /// The error code.
    #[must_use]
    pub fn code(&self) -> &str {
        &self.code
    }

    /// The parameter override, if any.
    #[must_use]
    pub fn param(&self) -> Option<&str> {
        self.param.as_deref()
    }
}

/// Turns a provider identity into one of the app's users.
///
/// This is where the app decides who an identity belongs to: look up a stored
/// `(provider, subject)` link, match a verified email, create a user, attach
/// the identity to `ctx.current_user` for a `link` sign-in, or refuse. muxa-auth
/// then signs the returned user in (rotating the session id) and redirects.
pub trait IdentityResolver<U>: Send + Sync + 'static {
    /// Resolve `identity`.
    fn resolve(
        &self,
        identity: ExternalIdentity,
        ctx: ResolveContext<'_, U>,
    ) -> impl Future<Output = Result<Resolved<U>, Refusal>> + Send;
}
