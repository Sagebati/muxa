//! Cross-origin request protection for cookie-authenticated endpoints.
//!
//! A state-changing request (anything but `GET`/`HEAD`/`OPTIONS`) is refused
//! when the browser says it comes from another site. The rule follows Go's
//! `net/http.CrossOriginProtection`:
//!
//! - `Sec-Fetch-Site: same-origin` or `none` passes;
//! - any other `Sec-Fetch-Site` passes only from a trusted origin;
//! - without `Sec-Fetch-Site` (older browsers, non-browser clients), a request
//!   with no `Origin` passes, and one with an `Origin` must match the `Host` or
//!   be trusted.
//!
//! With the session cookie on `SameSite=Lax` this covers CSRF without tokens.

use std::collections::HashSet;
use std::sync::Arc;

use axum::extract::{Request, State};
use axum::http::{HeaderMap, Method, StatusCode, header};
use axum::middleware::Next;
use axum::response::{IntoResponse as _, Response};
use url::Url;

/// Origins allowed to make state-changing requests, in serialized form.
#[derive(Debug, Clone, Default)]
pub struct TrustedOrigins(Arc<HashSet<String>>);

impl TrustedOrigins {
    pub fn new<'a>(origins: impl IntoIterator<Item = &'a str>) -> Self {
        Self(Arc::new(
            origins.into_iter().filter_map(normalize).collect(),
        ))
    }

    fn contains(&self, origin: &str) -> bool {
        normalize(origin).is_some_and(|origin| self.0.contains(&origin))
    }
}

fn normalize(origin: &str) -> Option<String> {
    let url = Url::parse(origin).ok()?;
    let origin = url.origin();
    origin.is_tuple().then(|| origin.ascii_serialization())
}

pub async fn guard(
    State(trusted): State<TrustedOrigins>,
    request: Request,
    next: Next,
) -> Response {
    if allowed(request.method(), request.headers(), &trusted) {
        next.run(request).await
    } else {
        (StatusCode::FORBIDDEN, "cross-origin request refused").into_response()
    }
}

fn allowed(method: &Method, headers: &HeaderMap, trusted: &TrustedOrigins) -> bool {
    if matches!(*method, Method::GET | Method::HEAD | Method::OPTIONS) {
        return true;
    }
    let origin = headers
        .get(header::ORIGIN)
        .and_then(|value| value.to_str().ok());

    if let Some(site) = headers.get("sec-fetch-site") {
        if site == "same-origin" || site == "none" {
            return true;
        }
        return origin.is_some_and(|origin| trusted.contains(origin));
    }

    let Some(origin) = origin else {
        return true;
    };
    if trusted.contains(origin) {
        return true;
    }
    let host = headers
        .get(header::HOST)
        .and_then(|value| value.to_str().ok());
    match (Url::parse(origin), host) {
        (Ok(origin), Some(host)) => {
            let origin_host = match (origin.host_str(), origin.port()) {
                (Some(name), Some(port)) => format!("{name}:{port}"),
                (Some(name), None) => name.to_owned(),
                _ => return false,
            };
            origin_host.eq_ignore_ascii_case(host)
        }
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use axum::http::HeaderValue;

    use super::*;

    fn headers(pairs: &[(&'static str, &'static str)]) -> HeaderMap {
        let mut map = HeaderMap::new();
        for (name, value) in pairs {
            map.insert(*name, HeaderValue::from_static(value));
        }
        map
    }

    fn trusted() -> TrustedOrigins {
        TrustedOrigins::new(["https://app.example", "http://127.0.0.1:5173/"])
    }

    #[test]
    fn safe_methods_always_pass() {
        let cross = headers(&[
            ("sec-fetch-site", "cross-site"),
            ("origin", "https://evil.example"),
        ]);
        assert!(allowed(&Method::GET, &cross, &trusted()));
    }

    #[test]
    fn fetch_metadata_decides_when_present() {
        let post = Method::POST;
        assert!(allowed(
            &post,
            &headers(&[("sec-fetch-site", "same-origin")]),
            &trusted()
        ));
        assert!(allowed(
            &post,
            &headers(&[("sec-fetch-site", "none")]),
            &trusted()
        ));
        assert!(!allowed(
            &post,
            &headers(&[
                ("sec-fetch-site", "cross-site"),
                ("origin", "https://evil.example")
            ]),
            &trusted()
        ));
        assert!(!allowed(
            &post,
            &headers(&[("sec-fetch-site", "same-site")]),
            &trusted()
        ));
        assert!(allowed(
            &post,
            &headers(&[
                ("sec-fetch-site", "cross-site"),
                ("origin", "http://127.0.0.1:5173")
            ]),
            &trusted()
        ));
    }

    #[test]
    fn origin_against_host_without_fetch_metadata() {
        let post = Method::POST;
        assert!(allowed(&post, &headers(&[]), &trusted()));
        assert!(allowed(
            &post,
            &headers(&[
                ("origin", "http://localhost:3000"),
                ("host", "localhost:3000")
            ]),
            &trusted()
        ));
        assert!(!allowed(
            &post,
            &headers(&[
                ("origin", "https://evil.example"),
                ("host", "localhost:3000")
            ]),
            &trusted()
        ));
        assert!(!allowed(
            &post,
            &headers(&[("origin", "null"), ("host", "x")]),
            &trusted()
        ));
        assert!(allowed(
            &post,
            &headers(&[("origin", "https://app.example")]),
            &trusted()
        ));
    }
}
