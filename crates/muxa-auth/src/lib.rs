//! Cookie sessions and OAuth2 / OpenID Connect sign-in for muxa.
//!
//! Built from established crates rather than its own protocol code:
//! [tower-sessions] keeps the session, [axum-login] keeps who is signed in on
//! it (and rotates the session id on sign-in and sign-out), and the
//! [oauth2] / [openidconnect] crates run the authorization-code flows (PKCE,
//! state, nonce, ID-token verification).
//!
//! What muxa-auth adds is the wiring: [`AuthPlugin`] reads `[auth]`, builds
//! the providers, mounts `start` / `callback` / `sign-out` routes, applies the
//! layers to the whole app, and guards cookie-authenticated requests against
//! cross-origin use. The app keeps the parts that are its own business: an
//! [`AuthnBackend`] that loads its users, and an [`IdentityResolver`] that
//! decides which user a provider identity is.
//!
//! Features:
//! - `oidc` — OpenID Connect providers (plain OAuth2 needs no feature);
//! - `password` — `password` hashing helpers (argon2, bcrypt accepted);
//! - `diesel-store` — `DieselSessionStore` over muxa-diesel's pool;
//! - `openapi` — aide docs for the extractors (`extract::openapi`).
//!
//! [tower-sessions]: https://docs.rs/tower-sessions
//! [axum-login]: https://docs.rs/axum-login
//! [oauth2]: https://docs.rs/oauth2
//! [openidconnect]: https://docs.rs/openidconnect

#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod config;
mod csrf;
pub mod extract;
mod http_client;
mod identity;
#[cfg(feature = "password")]
pub mod password;
mod plugin;
mod provider;
pub mod redirect;
mod routes;
#[cfg(feature = "diesel-store")]
pub mod store;

pub use axum_login::{self, AuthSession, AuthUser, AuthnBackend, tower_sessions};
pub use config::{AuthConfig, CookieConfig, ProviderConfig};
pub use extract::{MaybeSessionUser, SessionUser};
pub use identity::{ExternalIdentity, IdentityResolver, Refusal, ResolveContext, Resolved};
pub use plugin::{AuthHandle, AuthParts, AuthPlugin};
pub use redirect::safe_next;
#[cfg(feature = "diesel-store")]
pub use store::DieselSessionStore;
