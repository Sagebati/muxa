//! Extractors for the signed-in user.
//!
//! Apps with their own error type usually extract [`AuthSession`] themselves
//! and map a missing user to their 401; these are for the rest.

use axum::extract::FromRequestParts;
use axum::http::StatusCode;
use axum::http::request::Parts;
use axum::response::{IntoResponse as _, Response};
use axum_login::{AuthSession, AuthnBackend};

/// The signed-in user. Rejects with 401 when the session has none.
#[derive(Debug, Clone)]
pub struct SessionUser<B: AuthnBackend>(pub B::User);

/// The signed-in user, if any. Never rejects (beyond a missing auth layer).
#[derive(Debug, Clone)]
pub struct MaybeSessionUser<B: AuthnBackend>(pub Option<B::User>);

impl<S, B> FromRequestParts<S> for SessionUser<B>
where
    B: AuthnBackend + Send + Sync + 'static,
    S: Send + Sync,
{
    type Rejection = Response;

    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self, Self::Rejection> {
        let auth = AuthSession::<B>::from_request_parts(parts, state)
            .await
            .map_err(axum::response::IntoResponse::into_response)?;
        auth.user
            .map(Self)
            .ok_or_else(|| StatusCode::UNAUTHORIZED.into_response())
    }
}

impl<S, B> FromRequestParts<S> for MaybeSessionUser<B>
where
    B: AuthnBackend + Send + Sync + 'static,
    S: Send + Sync,
{
    type Rejection = Response;

    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self, Self::Rejection> {
        let auth = AuthSession::<B>::from_request_parts(parts, state)
            .await
            .map_err(axum::response::IntoResponse::into_response)?;
        Ok(Self(auth.user))
    }
}

/// OpenAPI documentation for cookie-session authentication.
#[cfg(feature = "openapi")]
pub mod openapi {
    use aide::OperationInput;
    use aide::generate::GenContext;
    use aide::openapi::{
        ApiKeyLocation, Operation, Response as SpecResponse, SecurityRequirement, SecurityScheme,
        StatusCode as SpecStatusCode,
    };
    use axum_login::AuthnBackend;

    use super::{MaybeSessionUser, SessionUser};

    /// Name of the security scheme the extractors reference. Register it with
    /// [`security_scheme`] under this name.
    pub const SESSION_SCHEME: &str = "session";

    /// The cookie security scheme, for `components.security_schemes`.
    #[must_use]
    pub fn security_scheme(cookie_name: &str) -> SecurityScheme {
        SecurityScheme::ApiKey {
            location: ApiKeyLocation::Cookie,
            name: cookie_name.to_owned(),
            description: Some("Session cookie set by signing in.".to_owned()),
            extensions: Default::default(),
        }
    }

    /// Mark `operation` as needing the session (idempotent).
    pub fn require_session(operation: &mut Operation) {
        let mut requirement = SecurityRequirement::new();
        requirement.insert(SESSION_SCHEME.to_owned(), Vec::new());
        if !operation.security.contains(&requirement) {
            operation.security.push(requirement);
        }
    }

    impl<B: AuthnBackend> OperationInput for SessionUser<B> {
        fn operation_input(_ctx: &mut GenContext, operation: &mut Operation) {
            require_session(operation);
        }

        fn inferred_early_responses(
            _ctx: &mut GenContext,
            _operation: &mut Operation,
        ) -> Vec<(Option<SpecStatusCode>, SpecResponse)> {
            vec![(
                Some(SpecStatusCode::Code(401)),
                SpecResponse {
                    description: "Not signed in.".to_owned(),
                    ..SpecResponse::default()
                },
            )]
        }
    }

    impl<B: AuthnBackend> OperationInput for MaybeSessionUser<B> {}
}
