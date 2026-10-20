//! The sign-in routes: `{mount}/{provider}/start`, `{mount}/{provider}/callback`
//! and `{mount}/sign-out`.

use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

use axum::Router;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse as _, Redirect, Response};
use axum::routing::{get, post};
use axum_login::tower_sessions::Session;
use axum_login::{AuthSession, AuthnBackend};
use serde::Deserialize;
use subtle::ConstantTimeEq as _;

use crate::http_client::HttpClient;
use crate::identity::{IdentityResolver, Refusal, ResolveContext};
use crate::provider::{PENDING_TTL_SECS, PendingLogin, Provider, now_secs};
use crate::redirect::{safe_next, with_query};

/// Session key holding the sign-ins in progress, by state token.
const PENDING_KEY: &str = "muxa_auth.pending";
/// At most this many sign-ins in progress per session (several tabs); the
/// oldest is dropped beyond it.
const MAX_PENDING: usize = 4;

pub struct Flow<R> {
    pub providers: BTreeMap<String, Provider>,
    pub resolver: R,
    pub http: HttpClient,
    pub sign_in_path: String,
    pub default_next: String,
    pub error_param: String,
}

pub fn router<B, R>(flow: Arc<Flow<R>>, mount: &str) -> Router
where
    B: AuthnBackend + Send + Sync + 'static,
    R: IdentityResolver<B::User>,
{
    let mount = mount.trim_end_matches('/');
    Router::new()
        .route(&format!("{mount}/{{provider}}/start"), get(start::<B, R>))
        .route(
            &format!("{mount}/{{provider}}/callback"),
            get(callback::<B, R>),
        )
        .route(&format!("{mount}/sign-out"), post(sign_out::<B>))
        .with_state(flow)
}

#[derive(Debug, Deserialize)]
struct StartQuery {
    next: Option<String>,
    link: Option<String>,
}

/// Where a sign-in goes back to, and whether it was a link.
struct Return<'a> {
    next: &'a str,
    link: bool,
}

impl<R> Flow<R> {
    /// Send the browser back with `refusal`: to `next` for a link (the user is
    /// signed in and on some page), else to the sign-in page, keeping `next`.
    fn refuse(&self, refusal: &Refusal, back: Option<Return<'_>>) -> Response {
        let param = refusal.param().unwrap_or(&self.error_param);
        let target = match back {
            Some(Return { next, link: true }) => with_query(next, &[(param, refusal.code())]),
            Some(Return { next, link: false }) => with_query(
                &self.sign_in_path,
                &[(param, refusal.code()), ("next", next)],
            ),
            None => with_query(&self.sign_in_path, &[(param, refusal.code())]),
        };
        Redirect::to(&target).into_response()
    }
}

async fn start<B, R>(
    State(flow): State<Arc<Flow<R>>>,
    Path(provider_id): Path<String>,
    Query(query): Query<StartQuery>,
    auth: AuthSession<B>,
) -> Response
where
    B: AuthnBackend + Send + Sync + 'static,
    R: IdentityResolver<B::User>,
{
    let next = safe_next(query.next.as_deref(), &flow.default_next);
    let link = matches!(query.link.as_deref(), Some("1" | "true"));
    let back = || Some(Return { next: &next, link });

    let Some(provider) = flow.providers.get(&provider_id) else {
        return flow.refuse(&Refusal::new(Refusal::UNAVAILABLE), back());
    };
    if link && auth.user.is_none() {
        return flow.refuse(
            &Refusal::new(Refusal::SIGNED_OUT),
            Some(Return {
                next: &next,
                link: false,
            }),
        );
    }

    let authorization = match provider.authorize(&flow.http).await {
        Ok(authorization) => authorization,
        Err(err) => {
            tracing::warn!(provider = %provider_id, error = %err, "could not start sign-in");
            return flow.refuse(&Refusal::new(Refusal::PROVIDER_FAILED), back());
        }
    };

    let pending = PendingLogin {
        provider: provider_id,
        state: authorization.state,
        pkce_verifier: authorization.pkce_verifier,
        nonce: authorization.nonce,
        next: next.clone(),
        link,
        created_at: now_secs(),
    };
    if let Err(err) = remember(&auth.session, pending).await {
        tracing::error!(error = %err, "could not store the sign-in attempt");
        return flow.refuse(&Refusal::new(Refusal::FAILED), back());
    }
    Redirect::to(authorization.url.as_str()).into_response()
}

#[derive(Debug, Deserialize)]
struct CallbackQuery {
    code: Option<String>,
    state: Option<String>,
    error: Option<String>,
}

async fn callback<B, R>(
    State(flow): State<Arc<Flow<R>>>,
    Path(provider_id): Path<String>,
    Query(query): Query<CallbackQuery>,
    mut auth: AuthSession<B>,
) -> Response
where
    B: AuthnBackend + Send + Sync + 'static,
    R: IdentityResolver<B::User>,
{
    let pending = match &query.state {
        Some(state) => take(&auth.session, state).await,
        None => Ok(None),
    };
    let pending = match pending {
        Ok(Some(pending)) if pending.provider == provider_id => pending,
        Ok(_) => return flow.refuse(&Refusal::new(Refusal::EXPIRED), None),
        Err(err) => {
            tracing::error!(error = %err, "could not read the sign-in attempt");
            return flow.refuse(&Refusal::new(Refusal::FAILED), None);
        }
    };
    let back = || {
        Some(Return {
            next: &pending.next,
            link: pending.link,
        })
    };

    if let Some(error) = &query.error {
        let code = if error == "access_denied" {
            Refusal::CANCELLED
        } else {
            tracing::warn!(provider = %provider_id, %error, "provider returned an error");
            Refusal::PROVIDER_FAILED
        };
        return flow.refuse(&Refusal::new(code), back());
    }
    let (Some(code), Some(provider)) = (query.code, flow.providers.get(&provider_id)) else {
        return flow.refuse(&Refusal::new(Refusal::PROVIDER_FAILED), back());
    };

    let identity = match provider.identify(code, &pending, &flow.http).await {
        Ok(identity) => identity,
        Err(err) => {
            tracing::warn!(provider = %provider_id, error = %err, "sign-in failed at the provider");
            return flow.refuse(&Refusal::new(Refusal::PROVIDER_FAILED), back());
        }
    };
    if pending.link && auth.user.is_none() {
        return flow.refuse(
            &Refusal::new(Refusal::SIGNED_OUT),
            Some(Return {
                next: &pending.next,
                link: false,
            }),
        );
    }

    let ctx = ResolveContext {
        current_user: auth.user.as_ref(),
        link: pending.link,
    };
    let resolved = match flow.resolver.resolve(identity, ctx).await {
        Ok(resolved) => resolved,
        Err(refusal) => return flow.refuse(&refusal, back()),
    };
    // Rotates the session id, so a session fixed before sign-in is useless after it.
    if let Err(err) = auth.login(&resolved.user).await {
        tracing::error!(error = %err, "could not sign in");
        return flow.refuse(&Refusal::new(Refusal::FAILED), back());
    }
    Redirect::to(&with_query(&pending.next, &resolved.query)).into_response()
}

async fn sign_out<B>(mut auth: AuthSession<B>) -> StatusCode
where
    B: AuthnBackend + Send + Sync + 'static,
{
    match auth.logout().await {
        Ok(_) => StatusCode::NO_CONTENT,
        Err(err) => {
            tracing::error!(error = %err, "could not sign out");
            StatusCode::INTERNAL_SERVER_ERROR
        }
    }
}

type PendingMap = HashMap<String, PendingLogin>;

async fn remember(
    session: &Session,
    pending: PendingLogin,
) -> Result<(), axum_login::tower_sessions::session::Error> {
    let mut map: PendingMap = session.get(PENDING_KEY).await?.unwrap_or_default();
    let now = now_secs();
    map.retain(|_, attempt| now.saturating_sub(attempt.created_at) < PENDING_TTL_SECS);
    while map.len() >= MAX_PENDING {
        let Some(oldest) = map
            .iter()
            .min_by_key(|(_, attempt)| attempt.created_at)
            .map(|(state, _)| state.clone())
        else {
            break;
        };
        map.remove(&oldest);
    }
    map.insert(pending.state.clone(), pending);
    session.insert(PENDING_KEY, map).await
}

/// Remove and return the attempt whose state token is `state`, if it is
/// still fresh. Single use: a replayed callback finds nothing.
async fn take(
    session: &Session,
    state: &str,
) -> Result<Option<PendingLogin>, axum_login::tower_sessions::session::Error> {
    let Some(mut map) = session.get::<PendingMap>(PENDING_KEY).await? else {
        return Ok(None);
    };
    let key = map
        .keys()
        .find(|key| bool::from(key.as_bytes().ct_eq(state.as_bytes())))
        .cloned();
    let found = key.and_then(|key| map.remove(&key));
    if map.is_empty() {
        session.remove::<PendingMap>(PENDING_KEY).await?;
    } else {
        session.insert(PENDING_KEY, map).await?;
    }
    Ok(found.filter(|attempt| now_secs().saturating_sub(attempt.created_at) < PENDING_TTL_SECS))
}
