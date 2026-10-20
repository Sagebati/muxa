//! [`AuthPlugin`]: wires sessions, login state, the cross-origin guard and the
//! sign-in routes into the app.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use axum_login::tower_sessions::cookie::SameSite as CookieSameSite;
use axum_login::tower_sessions::session_store::{ExpiredDeletion, SessionStore};
use axum_login::tower_sessions::{Expiry, SessionManagerLayer};
use axum_login::{AuthManagerLayerBuilder, AuthnBackend};
use muxa_core::{BoxFuture, BuildCtx, Dupe, Error, Plugin, Result, ShutdownToken, State};

use crate::config::{AuthConfig, SameSite};
use crate::csrf::{self, TrustedOrigins};
use crate::http_client::HttpClient;
use crate::identity::IdentityResolver;
use crate::provider::Provider;
use crate::routes::{self, Flow};

type Sweeper = Box<dyn FnOnce(Duration, ShutdownToken) -> BoxFuture<'static, ()> + Send>;

/// What the app supplies to [`AuthPlugin`]: its users, how provider identities
/// map onto them, and where sessions live.
pub struct AuthParts<B, R, St> {
    backend: B,
    resolver: R,
    store: St,
    sweeper: Option<Sweeper>,
}

impl<B, R, St> AuthParts<B, R, St> {
    /// `backend` loads users for the session (and checks passwords, if the app
    /// uses `AuthSession::authenticate`); `resolver` maps provider identities to
    /// users; `store` keeps the sessions.
    pub fn new(backend: B, resolver: R, store: St) -> Self {
        Self {
            backend,
            resolver,
            store,
            sweeper: None,
        }
    }

    /// Also delete expired sessions from `store` in the background, every
    /// `[auth] cleanup_interval_secs`.
    #[must_use]
    pub fn sweep_expired(mut self) -> Self
    where
        St: ExpiredDeletion + Clone,
    {
        let store = self.store.clone();
        self.sweeper = Some(Box::new(move |interval, shutdown| {
            Box::pin(async move {
                while shutdown
                    .run_until_cancelled(tokio::time::sleep(interval))
                    .await
                    .is_some()
                {
                    if let Err(err) = store.delete_expired().await {
                        tracing::warn!(error = %err, "could not delete expired sessions");
                    }
                }
            })
        }));
        self
    }
}

/// What [`AuthPlugin`] puts in the state: which providers are on.
#[derive(Debug, Clone, Default)]
pub struct AuthHandle {
    providers: Arc<[String]>,
}

impl Dupe for AuthHandle {}

impl AuthHandle {
    /// Ids of the providers that are on, sorted.
    #[must_use]
    pub fn providers(&self) -> &[String] {
        &self.providers
    }

    /// Whether provider `id` is on.
    #[must_use]
    pub fn has_provider(&self, id: &str) -> bool {
        self.providers.iter().any(|provider| provider == id)
    }
}

/// Cookie sessions, login state and OAuth2 / OpenID Connect sign-in.
///
/// Registers, for every route in the app:
/// - a session layer (tower-sessions) with the `[auth.cookie]` cookie,
/// - axum-login's auth layer, so handlers can extract
///   [`AuthSession`](axum_login::AuthSession) or [`SessionUser`](crate::SessionUser),
/// - a guard refusing cross-origin state-changing requests (see the `csrf` docs),
///
/// and mounts, under `[auth] mount`:
/// - `GET {mount}/{provider}/start?next=/path[&link=1]` — redirect to the provider,
/// - `GET {mount}/{provider}/callback` — finish, resolve, sign in, redirect to `next`,
/// - `POST {mount}/sign-out`.
///
/// A provider with an empty `client_id` is off; one that is configured but
/// unusable is logged as an error and left off, so a bad provider setting
/// never stops the app.
///
/// ```ignore
/// .with_plugin(AuthPlugin::new(|state: &_| {
///     let pool: &DieselPool = state.select();
///     AuthParts::new(Users::new(pool.dupe()), Resolver::new(pool.dupe()),
///                    DieselSessionStore::new(pool.dupe()))
///         .sweep_expired()
/// }))
/// .await?
/// ```
pub struct AuthPlugin<F> {
    parts: F,
}

impl<F> AuthPlugin<F> {
    /// `parts` runs at build against the state so far.
    pub fn new(parts: F) -> Self {
        Self { parts }
    }
}

impl<S, F, B, R, St> Plugin<S> for AuthPlugin<F>
where
    S: State,
    F: FnOnce(&S) -> AuthParts<B, R, St> + Send + 'static,
    B: AuthnBackend + 'static,
    R: IdentityResolver<B::User>,
    St: SessionStore + Clone,
{
    type Output = AuthHandle;
    type Config = AuthConfig;
    const CONFIG_PREFIX: &'static str = "auth";

    async fn build(self, cfg: AuthConfig, state: &S, ctx: &mut BuildCtx) -> Result<AuthHandle> {
        let parts = (self.parts)(state);
        let http = HttpClient::new().map_err(Error::plugin_build::<Self, _>)?;

        let mount = format!("/{}", cfg.mount.trim_matches('/'));
        let mount = if mount == "/" { String::new() } else { mount };
        let base_url = cfg.base_url.trim_end_matches('/').to_owned();

        let mut providers = BTreeMap::new();
        for (id, provider_cfg) in &cfg.providers {
            if provider_cfg.client_id().is_empty() {
                tracing::debug!(provider = %id, "sign-in provider off (no client_id)");
                continue;
            }
            let redirect = match provider_redirect(provider_cfg) {
                Some(url) => url.to_owned(),
                None if !base_url.is_empty() => format!("{base_url}{mount}/{id}/callback"),
                None => {
                    tracing::error!(provider = %id, "auth.base_url is unset; sign-in provider off");
                    continue;
                }
            };
            match Provider::from_config(id, provider_cfg, redirect) {
                Ok(Some(provider)) => {
                    tracing::info!(provider = %id, "sign-in provider on");
                    providers.insert(id.clone(), provider);
                }
                Ok(None) => {}
                Err(err) => {
                    tracing::error!(provider = %id, error = %err, "sign-in provider misconfigured; off");
                }
            }
        }
        let handle = AuthHandle {
            providers: providers.keys().cloned().collect(),
        };

        let flow = Arc::new(Flow {
            providers,
            resolver: parts.resolver,
            http,
            sign_in_path: cfg.sign_in_path.clone(),
            default_next: cfg.default_next.clone(),
            error_param: cfg.error_param.clone(),
        });
        ctx.router.mount("/", routes::router::<B, R>(flow, &mount));

        let session_layer = SessionManagerLayer::new(parts.store)
            .with_name(cfg.cookie.name.clone())
            .with_path("/")
            .with_http_only(true)
            .with_secure(cfg.cookie.secure)
            .with_same_site(match cfg.cookie.same_site {
                SameSite::Strict => CookieSameSite::Strict,
                SameSite::Lax => CookieSameSite::Lax,
                SameSite::None => CookieSameSite::None,
            })
            .with_expiry(Expiry::OnInactivity(time::Duration::seconds(
                cfg.cookie.ttl_secs,
            )));
        let auth_layer = AuthManagerLayerBuilder::new(parts.backend, session_layer).build();
        ctx.router.layer(move |router| router.layer(auth_layer));

        let trusted = TrustedOrigins::new(
            std::iter::once(base_url.as_str())
                .chain(cfg.trusted_origins.iter().map(String::as_str)),
        );
        ctx.router.layer(move |router| {
            router.layer(axum::middleware::from_fn_with_state(trusted, csrf::guard))
        });

        if let Some(sweeper) = parts.sweeper {
            let interval = Duration::from_secs(cfg.cleanup_interval_secs.max(60));
            ctx.tasks.spawn("muxa-auth.sweep", move |shutdown| {
                sweeper(interval, shutdown)
            });
        }

        Ok(handle)
    }
}

fn provider_redirect(cfg: &crate::config::ProviderConfig) -> Option<&str> {
    match cfg {
        crate::config::ProviderConfig::OAuth2(cfg) => cfg.redirect_url.as_deref(),
        crate::config::ProviderConfig::Oidc(cfg) => cfg.redirect_url.as_deref(),
    }
}
