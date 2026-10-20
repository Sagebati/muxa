//! The HTTP client the OAuth2 / OpenID Connect crates call through.
//!
//! oauth2 and openidconnect ship an adapter for reqwest 0.12 only; this is the
//! same adapter over the reqwest 0.13 the rest of the workspace uses, so the
//! build carries one reqwest.

use std::future::Future;
use std::pin::Pin;
use std::time::Duration;

use oauth2::{AsyncHttpClient, HttpClientError, HttpRequest, HttpResponse};

/// A reqwest client that never follows redirects, as the oauth2 crate asks
/// (following them would let a hostile provider point token requests anywhere).
#[derive(Debug, Clone)]
pub struct HttpClient(reqwest::Client);

impl HttpClient {
    /// Build the client.
    ///
    /// # Errors
    /// When the TLS backend fails to initialise.
    pub fn new() -> Result<Self, reqwest::Error> {
        reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(Duration::from_secs(15))
            .user_agent(concat!("muxa-auth/", env!("CARGO_PKG_VERSION")))
            .build()
            .map(Self)
    }

    /// The underlying reqwest client, for userinfo calls.
    #[must_use]
    pub fn inner(&self) -> &reqwest::Client {
        &self.0
    }
}

impl<'c> AsyncHttpClient<'c> for HttpClient {
    type Error = HttpClientError<reqwest::Error>;
    type Future =
        Pin<Box<dyn Future<Output = Result<HttpResponse, Self::Error>> + Send + Sync + 'c>>;

    fn call(&'c self, request: HttpRequest) -> Self::Future {
        Box::pin(async move {
            let response = self
                .0
                .execute(request.try_into().map_err(Box::new)?)
                .await
                .map_err(Box::new)?;

            let mut builder = http::Response::builder()
                .status(response.status())
                .version(response.version());
            for (name, value) in response.headers() {
                builder = builder.header(name, value);
            }
            builder
                .body(response.bytes().await.map_err(Box::new)?.to_vec())
                .map_err(HttpClientError::Http)
        })
    }
}
