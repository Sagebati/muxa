//! End-to-end sign-in flows against mock providers (wiremock), through the
//! plugin's composed router.

#![allow(clippy::unwrap_used, reason = "test helpers fail loudly")]

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use axum::Router;
use axum::body::Body;
use axum::http::{Request, Response, StatusCode, header};
use axum::routing::get;
use figment::Figment;
use figment::providers::{Format as _, Toml};
use muxa_auth::{
    AuthParts, AuthPlugin, AuthUser, AuthnBackend, ExternalIdentity, IdentityResolver, Refusal,
    ResolveContext, Resolved, SessionUser,
};
use muxa_core::{App, HNil};
use tower::ServiceExt as _;
use tower_sessions::MemoryStore;
use url::Url;
use wiremock::matchers::{body_string_contains, header as header_is, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

// ---------------------------------------------------------------------------
// A tiny app: users in memory, identities resolve to `{provider}:{subject}`.
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
struct User {
    id: String,
    name: Option<String>,
}

impl AuthUser for User {
    type Id = String;

    fn id(&self) -> String {
        self.id.clone()
    }

    fn session_auth_hash(&self) -> &[u8] {
        self.id.as_bytes()
    }
}

#[derive(Debug, Clone, Default)]
struct Users(Arc<Mutex<HashMap<String, User>>>);

#[derive(Debug, thiserror::Error)]
#[error("never")]
struct Never;

impl AuthnBackend for Users {
    type User = User;
    type Credentials = ();
    type Error = Never;

    async fn authenticate(&self, _creds: ()) -> Result<Option<User>, Never> {
        Ok(None)
    }

    async fn get_user(&self, id: &String) -> Result<Option<User>, Never> {
        Ok(self.0.lock().unwrap().get(id).cloned())
    }
}

#[derive(Clone)]
struct Resolver(Users);

impl IdentityResolver<User> for Resolver {
    async fn resolve(
        &self,
        identity: ExternalIdentity,
        ctx: ResolveContext<'_, User>,
    ) -> Result<Resolved<User>, Refusal> {
        if identity.subject == "banned" {
            return Err(Refusal::new("banned"));
        }
        if ctx.link {
            let user = ctx.current_user.cloned().expect("link implies a user");
            return Ok(Resolved::new(user).with_query("linked", identity.provider));
        }
        let user = User {
            id: format!("{}:{}", identity.provider, identity.subject),
            name: identity.name.clone(),
        };
        self.0
            .0
            .lock()
            .unwrap()
            .insert(user.id.clone(), user.clone());
        Ok(Resolved::new(user))
    }
}

async fn whoami(SessionUser(user): SessionUser<Users>) -> String {
    format!("{} {}", user.id, user.name.unwrap_or_default())
}

async fn app(config: &str) -> Router {
    let users = Users::default();
    let figment = Figment::from(Toml::string(config));
    let mut app = App::with_figment(figment)
        .with_plugin(AuthPlugin::new(move |_: &HNil| {
            AuthParts::new(users.clone(), Resolver(users), MemoryStore::default())
        }))
        .await
        .unwrap();
    app.ctx_mut()
        .router
        .mount("/", Router::new().route("/whoami", get(whoami)));
    std::mem::take(&mut app.ctx_mut().router).compose()
}

fn oauth2_config(mock: &MockServer) -> String {
    format!(
        r#"
        [auth]
        base_url = "http://app.test"
        mount = "/auth"
        default_next = "/home"

        [auth.providers.mock]
        kind = "oauth2"
        client_id = "client"
        client_secret = "secret"
        auth_url = "{uri}/authorize"
        token_url = "{uri}/token"
        userinfo_url = "{uri}/me"
        token_auth = "post"
        scopes = ["read"]
        name_field = "username"

        [auth.providers.off]
        kind = "oauth2"
        client_id = ""
        "#,
        uri = mock.uri()
    )
}

// ---------------------------------------------------------------------------
// A cookie-keeping client over the router.
// ---------------------------------------------------------------------------

struct Browser {
    router: Router,
    cookie: Option<String>,
}

impl Browser {
    fn new(router: Router) -> Self {
        Self {
            router,
            cookie: None,
        }
    }

    async fn send(&mut self, request: axum::http::request::Builder) -> Response<Body> {
        let request = match &self.cookie {
            Some(cookie) => request.header(header::COOKIE, cookie),
            None => request,
        };
        let response = self
            .router
            .clone()
            .oneshot(request.body(Body::empty()).unwrap())
            .await
            .unwrap();
        if let Some(set) = response.headers().get(header::SET_COOKIE) {
            let pair = set.to_str().unwrap().split(';').next().unwrap().to_owned();
            // An emptied cookie is the server deleting it.
            self.cookie = (!pair.ends_with('=')).then_some(pair);
        }
        response
    }

    async fn get(&mut self, uri: &str) -> Response<Body> {
        self.send(Request::get(uri)).await
    }
}

fn location(response: &Response<Body>) -> String {
    assert_eq!(
        response.status(),
        StatusCode::SEE_OTHER,
        "expected a redirect"
    );
    response.headers()[header::LOCATION]
        .to_str()
        .unwrap()
        .to_owned()
}

fn query(url: &str, key: &str) -> Option<String> {
    Url::parse(url)
        .unwrap()
        .query_pairs()
        .find(|(name, _)| name == key)
        .map(|(_, value)| value.into_owned())
}

async fn body(response: Response<Body>) -> String {
    let bytes = axum::body::to_bytes(response.into_body(), 1 << 16)
        .await
        .unwrap();
    String::from_utf8(bytes.to_vec()).unwrap()
}

async fn mock_oauth2_provider() -> MockServer {
    let mock = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/token"))
        .and(body_string_contains("code=good-code"))
        .and(body_string_contains("code_verifier="))
        .and(body_string_contains("client_secret=secret"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "access_token": "access-1",
            "token_type": "Bearer",
            "expires_in": 3600
        })))
        .mount(&mock)
        .await;
    Mock::given(method("GET"))
        .and(path("/me"))
        .and(header_is("authorization", "Bearer access-1"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(serde_json::json!({ "id": 42, "username": "alice" })),
        )
        .mount(&mock)
        .await;
    mock
}

// ---------------------------------------------------------------------------
// OAuth2
// ---------------------------------------------------------------------------

#[tokio::test]
async fn oauth2_sign_in_round_trip() {
    let mock = mock_oauth2_provider().await;
    let mut browser = Browser::new(app(&oauth2_config(&mock)).await);

    let response = browser.get("/auth/mock/start?next=/games%3Fpage%3D2").await;
    let authorize = location(&response);
    assert!(authorize.starts_with(&format!("{}/authorize?", mock.uri())));
    assert_eq!(
        query(&authorize, "code_challenge_method").as_deref(),
        Some("S256")
    );
    assert_eq!(query(&authorize, "scope").as_deref(), Some("read"));
    assert_eq!(
        query(&authorize, "redirect_uri").as_deref(),
        Some("http://app.test/auth/mock/callback")
    );
    let state = query(&authorize, "state").unwrap();
    let cookie_before = browser
        .cookie
        .clone()
        .expect("start sets the session cookie");

    let response = browser
        .get(&format!("/auth/mock/callback?code=good-code&state={state}"))
        .await;
    assert_eq!(location(&response), "/games?page=2");
    assert_ne!(
        browser.cookie.as_ref(),
        Some(&cookie_before),
        "session id rotated"
    );

    let response = browser.get("/whoami").await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(body(response).await, "mock:42 alice");

    // The attempt is single use.
    let response = browser
        .get(&format!("/auth/mock/callback?code=good-code&state={state}"))
        .await;
    assert_eq!(location(&response), "/?auth_error=expired");
}

#[tokio::test]
async fn unknown_state_is_expired() {
    let mock = mock_oauth2_provider().await;
    let mut browser = Browser::new(app(&oauth2_config(&mock)).await);
    browser.get("/auth/mock/start?next=/games").await;

    let response = browser
        .get("/auth/mock/callback?code=good-code&state=forged")
        .await;
    assert_eq!(location(&response), "/?auth_error=expired");
    assert_eq!(
        browser.get("/whoami").await.status(),
        StatusCode::UNAUTHORIZED
    );
}

#[tokio::test]
async fn cancelling_at_the_provider_keeps_next() {
    let mock = mock_oauth2_provider().await;
    let mut browser = Browser::new(app(&oauth2_config(&mock)).await);
    let authorize = location(&browser.get("/auth/mock/start?next=/games").await);
    let state = query(&authorize, "state").unwrap();

    let response = browser
        .get(&format!(
            "/auth/mock/callback?error=access_denied&state={state}"
        ))
        .await;
    assert_eq!(location(&response), "/?auth_error=cancelled&next=%2Fgames");
}

#[tokio::test]
async fn token_failure_is_provider_failed() {
    let mock = mock_oauth2_provider().await;
    let mut browser = Browser::new(app(&oauth2_config(&mock)).await);
    let authorize = location(&browser.get("/auth/mock/start").await);
    let state = query(&authorize, "state").unwrap();

    // Not "good-code": the token mock doesn't match, wiremock answers 404.
    let response = browser
        .get(&format!("/auth/mock/callback?code=bad-code&state={state}"))
        .await;
    assert_eq!(
        location(&response),
        "/?auth_error=provider_failed&next=%2Fhome"
    );
}

#[tokio::test]
async fn open_redirects_fall_back_to_the_default() {
    let mock = mock_oauth2_provider().await;
    let mut browser = Browser::new(app(&oauth2_config(&mock)).await);
    let authorize = location(
        &browser
            .get("/auth/mock/start?next=https://evil.example/")
            .await,
    );
    let state = query(&authorize, "state").unwrap();

    let response = browser
        .get(&format!("/auth/mock/callback?code=good-code&state={state}"))
        .await;
    assert_eq!(location(&response), "/home");
}

#[tokio::test]
async fn unknown_and_disabled_providers_are_unavailable() {
    let mock = mock_oauth2_provider().await;
    let mut browser = Browser::new(app(&oauth2_config(&mock)).await);
    for provider in ["nope", "off"] {
        let response = browser
            .get(&format!("/auth/{provider}/start?next=/games"))
            .await;
        assert_eq!(
            location(&response),
            "/?auth_error=unavailable&next=%2Fgames"
        );
    }
}

#[tokio::test]
async fn link_needs_a_signed_in_user_and_keeps_it() {
    let mock = mock_oauth2_provider().await;
    let mut browser = Browser::new(app(&oauth2_config(&mock)).await);

    let response = browser.get("/auth/mock/start?next=/settings&link=1").await;
    assert_eq!(
        location(&response),
        "/?auth_error=signed_out&next=%2Fsettings"
    );

    // Sign in, then link.
    let authorize = location(&browser.get("/auth/mock/start").await);
    let state = query(&authorize, "state").unwrap();
    browser
        .get(&format!("/auth/mock/callback?code=good-code&state={state}"))
        .await;
    let authorize = location(&browser.get("/auth/mock/start?next=/settings&link=1").await);
    let state = query(&authorize, "state").unwrap();
    let response = browser
        .get(&format!("/auth/mock/callback?code=good-code&state={state}"))
        .await;
    assert_eq!(location(&response), "/settings?linked=mock");
    assert_eq!(body(browser.get("/whoami").await).await, "mock:42 alice");
}

#[tokio::test]
async fn sign_out_is_guarded_against_cross_origin_posts() {
    let mock = mock_oauth2_provider().await;
    let mut browser = Browser::new(app(&oauth2_config(&mock)).await);
    let authorize = location(&browser.get("/auth/mock/start").await);
    let state = query(&authorize, "state").unwrap();
    browser
        .get(&format!("/auth/mock/callback?code=good-code&state={state}"))
        .await;

    let response = browser
        .send(
            Request::post("/auth/sign-out")
                .header("sec-fetch-site", "cross-site")
                .header(header::ORIGIN, "https://evil.example"),
        )
        .await;
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
    assert_eq!(browser.get("/whoami").await.status(), StatusCode::OK);

    let response = browser
        .send(Request::post("/auth/sign-out").header("sec-fetch-site", "same-origin"))
        .await;
    assert_eq!(response.status(), StatusCode::NO_CONTENT);
    assert_eq!(
        browser.get("/whoami").await.status(),
        StatusCode::UNAUTHORIZED
    );
}

// ---------------------------------------------------------------------------
// OpenID Connect (HS256 ID tokens signed with the client secret)
// ---------------------------------------------------------------------------

#[cfg(feature = "oidc")]
mod oidc {
    use base64::Engine as _;
    use base64::engine::general_purpose::URL_SAFE_NO_PAD;
    use hmac::{Hmac, Mac as _};
    use sha2::Sha256;

    use super::*;

    // ---------------------------------------------------------------------------

    const OIDC_SECRET: &str = "oidc-client-secret-with-enough-bytes";

    fn sign_hs256(claims: &serde_json::Value) -> String {
        let header = URL_SAFE_NO_PAD.encode(br#"{"alg":"HS256","typ":"JWT"}"#);
        let payload = URL_SAFE_NO_PAD.encode(serde_json::to_vec(claims).unwrap());
        let mut mac = Hmac::<Sha256>::new_from_slice(OIDC_SECRET.as_bytes()).unwrap();
        mac.update(format!("{header}.{payload}").as_bytes());
        let signature = URL_SAFE_NO_PAD.encode(mac.finalize().into_bytes());
        format!("{header}.{payload}.{signature}")
    }

    async fn mock_oidc_provider() -> MockServer {
        let mock = MockServer::start().await;
        let issuer = mock.uri();
        Mock::given(method("GET"))
            .and(path("/.well-known/openid-configuration"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "issuer": issuer,
                "authorization_endpoint": format!("{issuer}/authorize"),
                "token_endpoint": format!("{issuer}/token"),
                "jwks_uri": format!("{issuer}/jwks"),
                "response_types_supported": ["code"],
                "subject_types_supported": ["public"],
                "id_token_signing_alg_values_supported": ["HS256"]
            })))
            .mount(&mock)
            .await;
        Mock::given(method("GET"))
            .and(path("/jwks"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(serde_json::json!({ "keys": [] })),
            )
            .mount(&mock)
            .await;
        mock
    }

    async fn mount_token(mock: &MockServer, nonce: &str) {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        let id_token = sign_hs256(&serde_json::json!({
            "iss": mock.uri(),
            "sub": "google-123",
            "aud": "oidc-client",
            "iat": now,
            "exp": now + 300,
            "nonce": nonce,
            "email": "alice@example.com",
            "email_verified": true,
            "name": "Alice"
        }));
        Mock::given(method("POST"))
            .and(path("/token"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "access_token": "access-oidc",
                "token_type": "Bearer",
                "expires_in": 3600,
                "id_token": id_token
            })))
            .mount(mock)
            .await;
    }

    fn oidc_config(mock: &MockServer) -> String {
        format!(
            r#"
            [auth]
            base_url = "http://app.test"
            mount = "/auth"

            [auth.providers.google]
            kind = "oidc"
            issuer = "{uri}"
            client_id = "oidc-client"
            client_secret = "{OIDC_SECRET}"
            "#,
            uri = mock.uri()
        )
    }

    #[tokio::test]
    async fn oidc_sign_in_checks_the_nonce() {
        let mock = mock_oidc_provider().await;
        let mut browser = Browser::new(app(&oidc_config(&mock)).await);

        let authorize = location(&browser.get("/auth/google/start?next=/games").await);
        let scope = query(&authorize, "scope").unwrap();
        assert!(scope.split(' ').any(|scope| scope == "openid"), "{scope}");
        let state = query(&authorize, "state").unwrap();
        let nonce = query(&authorize, "nonce").unwrap();
        mount_token(&mock, &nonce).await;

        let response = browser
            .get(&format!("/auth/google/callback?code=any&state={state}"))
            .await;
        assert_eq!(location(&response), "/games");
        assert_eq!(
            body(browser.get("/whoami").await).await,
            "google:google-123 Alice"
        );
    }

    #[tokio::test]
    async fn oidc_rejects_a_token_for_another_nonce() {
        let mock = mock_oidc_provider().await;
        let mut browser = Browser::new(app(&oidc_config(&mock)).await);

        let authorize = location(&browser.get("/auth/google/start?next=/games").await);
        let state = query(&authorize, "state").unwrap();
        mount_token(&mock, "someone-elses-nonce").await;

        let response = browser
            .get(&format!("/auth/google/callback?code=any&state={state}"))
            .await;
        assert_eq!(
            location(&response),
            "/?auth_error=provider_failed&next=%2Fgames"
        );
        assert_eq!(
            browser.get("/whoami").await.status(),
            StatusCode::UNAUTHORIZED
        );
    }
}

#[tokio::test]
async fn resolver_refusals_reach_the_page() {
    let mock = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "access_token": "access-1",
            "token_type": "Bearer"
        })))
        .mount(&mock)
        .await;
    Mock::given(method("GET"))
        .and(path("/me"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(serde_json::json!({ "id": "banned" })),
        )
        .mount(&mock)
        .await;
    let mut browser = Browser::new(app(&oauth2_config(&mock)).await);
    let authorize = location(&browser.get("/auth/mock/start?next=/games").await);
    let state = query(&authorize, "state").unwrap();

    let response = browser
        .get(&format!("/auth/mock/callback?code=x&state={state}"))
        .await;
    assert_eq!(location(&response), "/?auth_error=banned&next=%2Fgames");
}
