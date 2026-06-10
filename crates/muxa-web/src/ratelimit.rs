//! Per-IP request rate limiting as a tower [`Layer`](tower::Layer).
//!
//! A thin wrapper over [`tower_governor`] (GCRA, via the `governor` crate). The
//! state is **in-memory, per process** — each instance keeps its own buckets, so
//! under N replicas the effective per-IP limit is N× the configured value. That's
//! fine as a coarse abuse guard; edge/LB limits are the place for hard global
//! caps.
//!
//! Build a layer with [`per_ip_layer`] and apply it to the route(s) you want to
//! protect:
//!
//! ```ignore
//! let cfg = RateLimitConfig::default();
//! let limited = ApiRouter::new()
//!     .api_route("/submit", post(handler))
//!     .layer(muxa_web::ratelimit::per_ip_layer(&cfg)?);
//! ```
//!
//! Peer-IP extraction relies on `ConnectInfo<SocketAddr>`, which the muxa-web
//! serve loop installs (`into_make_service_with_connect_info`) — so this works
//! out of the box.

use std::net::IpAddr;
use std::time::Duration;

use axum::body::Body;
use governor::middleware::StateInformationMiddleware;
use http::Request;
use muxa_core::Error;
use serde::Deserialize;
use tower_governor::GovernorLayer;
use tower_governor::governor::GovernorConfigBuilder;
use tower_governor::key_extractor::{KeyExtractor, PeerIpKeyExtractor, SmartIpKeyExtractor};
use tower_governor::errors::GovernorError;

/// How often the background janitor evicts idle per-IP buckets.
const EVICT_INTERVAL: Duration = Duration::from_secs(60);

/// Configuration for a per-IP rate limiter. Read from a `[ratelimit]`-style
/// config section.
///
/// The quota is GCRA: one request allowance is restored every [`period_ms`], and
/// up to [`burst`] may be spent at once. For example `period_ms = 10_000,
/// burst = 5` allows a burst of 5 then one more request every 10 s.
///
/// [`period_ms`]: Self::period_ms
/// [`burst`]: Self::burst
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct RateLimitConfig {
    /// Master switch. When `false`, callers should skip applying the layer.
    pub enabled: bool,
    /// Milliseconds to restore one request allowance (the sustained rate).
    pub period_ms: u64,
    /// Maximum requests allowed in a burst.
    pub burst: u32,
    /// Trust `X-Forwarded-For` / `X-Real-IP` / `Forwarded` for the client IP.
    ///
    /// Enable **only** behind a proxy you control that sets these headers —
    /// otherwise clients can spoof their IP to dodge the limit. When `false`
    /// (the default), the unspoofable peer socket IP is used (which behind a
    /// proxy is the proxy's IP, i.e. one shared bucket).
    pub trust_forwarded_for: bool,
}

impl Default for RateLimitConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            period_ms: 1000,
            burst: 5,
            trust_forwarded_for: false,
        }
    }
}

/// Key extractor that rate-limits per client IP. Delegates to tower_governor's
/// built-ins: the proxy-aware [`SmartIpKeyExtractor`] when forwarded headers are
/// trusted, else the unspoofable [`PeerIpKeyExtractor`].
#[derive(Clone)]
pub struct IpKeyExtractor {
    trust_forwarded: bool,
}

impl KeyExtractor for IpKeyExtractor {
    type Key = IpAddr;

    fn extract<T>(&self, req: &Request<T>) -> Result<Self::Key, GovernorError> {
        if self.trust_forwarded {
            SmartIpKeyExtractor.extract(req)
        } else {
            PeerIpKeyExtractor.extract(req)
        }
    }
}

/// The concrete layer type produced by [`per_ip_layer`]. `use_headers()` adds the
/// `x-ratelimit-*` / `retry-after` response headers (hence the middleware type).
pub type IpRateLimitLayer = GovernorLayer<IpKeyExtractor, StateInformationMiddleware, Body>;

/// Build a per-IP rate-limit layer from config, and spawn a background janitor
/// that evicts idle buckets so memory stays bounded.
///
/// Must be called from within a Tokio runtime (it spawns the janitor task). The
/// janitor is detached — it's a trivial periodic reclaim that ends with the
/// process.
pub fn per_ip_layer(cfg: &RateLimitConfig) -> Result<IpRateLimitLayer, Error> {
    let period = Duration::from_millis(cfg.period_ms.max(1));
    let burst = cfg.burst.max(1);

    let mut builder = GovernorConfigBuilder::default();
    builder.period(period).burst_size(burst);
    let config = builder
        .key_extractor(IpKeyExtractor {
            trust_forwarded: cfg.trust_forwarded_for,
        })
        .use_headers()
        .finish()
        .ok_or_else(|| Error::other("rate limit: invalid quota (zero burst or period)"))?;

    // Evict stale per-IP buckets periodically so the keyed limiter doesn't grow
    // unbounded under a wide spread of client IPs.
    let limiter = config.limiter().clone();
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(EVICT_INTERVAL).await;
            limiter.retain_recent();
        }
    });

    Ok(GovernorLayer::new(config))
}
