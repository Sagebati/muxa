# muxa-auth

Cookie sessions and sign-in with OAuth2 / OpenID Connect providers for [muxa](../muxa).

It wires established crates together rather than implementing the protocols:

- [tower-sessions](https://docs.rs/tower-sessions) for the session cookie and its store,
- [axum-login](https://docs.rs/axum-login) for who is signed in on a session, including session-id rotation on sign-in and sign-out,
- [oauth2](https://docs.rs/oauth2) and [openidconnect](https://docs.rs/openidconnect) for the authorization-code flow (PKCE, state, nonce, ID-token verification).

Through the facade this is the `auth` feature, plus `auth-oidc`, `auth-password`, `auth-diesel` and `auth-openapi`.

## What the plugin does

`AuthPlugin` reads `[auth]` and, for the whole app:

- adds the session layer and axum-login's auth layer, so any handler can extract `AuthSession<B>` or `SessionUser<B>`;
- refuses cross-origin state-changing requests (`POST`, `PUT`, …), following Go's `CrossOriginProtection` rules: `Sec-Fetch-Site`, else `Origin` against `Host`, plus `base_url` and `trusted_origins`;
- mounts, under `mount`:
  - `GET {mount}/{provider}/start?next=/path[&link=1]`, which redirects to the provider;
  - `GET {mount}/{provider}/callback`, which finishes the flow, asks the app who the identity is, signs that user in and redirects to `next`;
  - `POST {mount}/sign-out`.

The sign-in in progress (state, PKCE verifier, nonce, `next`) is kept in the session, so there is no table for it. Each attempt can be used once and expires after ten minutes.

The app supplies two things:

- an `AuthnBackend` (axum-login's trait), which loads its users by id;
- an `IdentityResolver`, which turns an `ExternalIdentity { provider, subject, email, email_verified, name, profile }` into one of its users. It can look up a stored `(provider, subject)`, match a verified email, create a user, attach the identity to the signed-in user on a `link=1` sign-in, or refuse with a `Refusal` code.

Failures send the browser to `sign_in_path?auth_error=<code>&next=…`. A `link` sign-in goes back to `next?auth_error=<code>` instead. The built-in codes are `cancelled`, `expired`, `provider_failed`, `unavailable`, `signed_out` and `failed`.

## Usage

```rust
use muxa::auth::{AuthParts, AuthPlugin, DieselSessionStore};
use muxa::prelude::*;

muxa::app!()
    .with_plugin(DieselPlugin::new().with_migrations(MigrationsRunner::new(MIGRATIONS))).await?
    .with_plugin(AuthPlugin::new(|state: &_| {
        let pool: &DieselPool = state.select();
        AuthParts::new(Users::new(pool.dupe()), Resolver::new(pool.dupe()),
                       DieselSessionStore::new(pool.dupe()))
            .sweep_expired()
    })).await?
    .with_plugin(ApiPlugin::new(routes, openapi)).await?
    .run().await
```

```toml
[auth]
base_url = "https://example.com"     # the origin the browser sees
mount = "/api/auth"
sign_in_path = "/"
default_next = "/"

[auth.cookie]
secure = true                        # false for plain-HTTP development
ttl_secs = 2592000                   # signed out after 30 days idle

[auth.providers.google]
kind = "oidc"
issuer = "https://accounts.google.com"
client_id = "…"
# client_secret: <APP>_AUTH__PROVIDERS__GOOGLE__CLIENT_SECRET

[auth.providers.github]
kind = "oauth2"
client_id = "…"
auth_url = "https://github.com/login/oauth/authorize"
token_url = "https://github.com/login/oauth/access_token"
userinfo_url = "https://api.github.com/user"
scopes = ["read:user"]
token_auth = "post"
subject_field = "id"
name_field = "login"
```

A provider with an empty `client_id` is off. A provider that is configured but unusable (a bad URL, a missing `base_url`) is logged as an error and left off, so a bad provider setting doesn't stop the app. OpenID Connect discovery runs on first use and is refreshed after `metadata_ttl_secs`, and again whenever an ID token fails to verify, which picks up rotated signing keys. `AuthHandle`, which the plugin adds to the state, says which providers are on.

## Sessions in Postgres

With `diesel-store`, `DieselSessionStore` keeps sessions in an `auth_sessions` table through muxa-diesel's pool. The app owns the table: put `muxa_auth::store::MIGRATION_SQL` in one of its migrations. `AuthParts::sweep_expired()` deletes expired rows in the background.

## Passwords

With `password`, `muxa_auth::password::{hash, verify}` hash with argon2 on the blocking pool. They also accept bcrypt hashes such as Supabase Auth's, and `verify` returns `Verified::NeedsRehash` so the app can upgrade them. Pass `None` to `verify` for an unknown account: it still spends the time of a real check. After a successful check, call `AuthSession::login(&user)`.

## Tests

`just test-auth` runs every feature. The diesel-store tests need `MUXA_AUTH_TEST_DATABASE_URL`, which can be any database because they only create a temporary table. Without it they skip.
