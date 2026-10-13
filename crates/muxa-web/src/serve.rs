//! The serve loop: binds, prints the banner, runs `axum::serve` until the
//! shutdown token is cancelled. Native only — wasm hosts drive the router
//! themselves.

use std::net::SocketAddr;

use axum::Router;
use muxa_core::{BoxFuture, BuildCtx, Result, ServeFn, ShutdownToken};

use crate::{WebConfig, banner};

/// Snapshot the banner data and register the serve loop on the build context.
pub fn schedule(cfg: WebConfig, ctx: &mut BuildCtx) -> Result<()> {
    // Render the banner's routes now: the serve closure runs after
    // `RouterRegistry` is consumed at `compose()` time.
    let banner_routes = cfg.banner.then(|| banner::routes(&ctx.router));

    let shutdown = ctx.shutdown.clone();
    let serve_fn: ServeFn = Box::new(move |router: Router| -> BoxFuture<'static, Result<()>> {
        Box::pin(serve_loop(router, cfg, banner_routes, shutdown))
    });
    ctx.set_serve_fn(serve_fn)
}

async fn serve_loop(
    router: Router,
    cfg: WebConfig,
    banner_routes: Option<String>,
    shutdown: ShutdownToken,
) -> Result<()> {
    // `(host, port)` goes through `ToSocketAddrs`: an IP literal is parsed
    // without a lookup, a hostname is resolved.
    let listener = tokio::net::TcpListener::bind((bind_host(&cfg.host), cfg.port)).await?;
    let bound = listener.local_addr()?;

    if let Some(routes) = banner_routes {
        banner::print(bound, &routes);
    }

    tracing::info!(%bound, "muxa-web: serving");

    // Bridge ctrl_c to the shutdown token (additive — anyone else can also
    // cancel it). Only compiled with the `graceful-shutdown` feature, which
    // also enables `tokio/signal`.
    #[cfg(feature = "graceful-shutdown")]
    {
        let signal_token = shutdown.clone();
        tokio::spawn(async move {
            if tokio::signal::ctrl_c().await.is_ok() {
                tracing::info!("ctrl-c received; signaling shutdown");
                signal_token.cancel();
            }
        });
    }

    let graceful = shutdown.clone();
    // Serve with per-connection `ConnectInfo<SocketAddr>` so handlers and tower
    // layers (e.g. the per-IP rate limiter) can read the client's peer address.
    // Additive and cheap — handlers that don't need it are unaffected.
    axum::serve(
        listener,
        router.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .with_graceful_shutdown(async move { graceful.cancelled().await })
    .await?;

    tracing::info!("muxa-web: serve loop ended");
    Ok(())
}

/// `host` without the brackets of a URL-style IPv6 literal (`[::1]` → `::1`),
/// which `ToSocketAddrs` doesn't take.
fn bind_host(host: &str) -> &str {
    host.strip_prefix('[')
        .and_then(|inner| inner.strip_suffix(']'))
        .unwrap_or(host)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bind_host_strips_ipv6_brackets_only() {
        assert_eq!(bind_host("[::1]"), "::1");
        assert_eq!(bind_host("[fe80::1%2]"), "fe80::1%2");
        assert_eq!(bind_host("::1"), "::1");
        assert_eq!(bind_host("0.0.0.0"), "0.0.0.0");
        assert_eq!(bind_host("localhost"), "localhost");
    }

    /// The forms the old `"{host}:{port}"` parse took still bind, and hostnames
    /// now do too.
    #[tokio::test]
    async fn binds_ip_literals_and_hostnames() {
        for host in ["127.0.0.1", "localhost"] {
            let listener = tokio::net::TcpListener::bind((bind_host(host), 0)).await;
            assert!(listener.is_ok(), "{host}: {listener:?}");
        }
    }
}
