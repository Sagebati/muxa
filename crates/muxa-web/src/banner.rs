//! Launch banner. Printed once to stderr when the server binds, showing the
//! bound URL and the mounted router prefixes.
//!
//! It deliberately does not print configuration: the web plugin only sees its
//! own section, and raw configuration values include other plugins' secrets.

use std::fmt::Write as _;
use std::io::Write as _;
use std::net::SocketAddr;

use muxa_core::{Mount, RouterRegistry};

/// Render the "routes (mounted)" block of the banner, at `WebPlugin::build`
/// time: the registry is consumed by `compose()` before the serve loop runs, so
/// it can't be read from there. One `String`, however many mounts.
pub fn routes(registry: &RouterRegistry) -> String {
    let mut out = String::from("  routes (mounted)\n");
    let fallback = registry.has_fallback().then_some((FALLBACK, "fallback"));
    let rows: Vec<(&str, &str)> = registry
        .mounts()
        .map(|(prefix, mode)| {
            let tag = match mode {
                Mount::Auto => "auto",
                Mount::Manual => "manual",
            };
            (display_prefix(prefix), tag)
        })
        .chain(fallback)
        .collect();
    let Some(prefix_width) = rows.iter().map(|(prefix, _)| prefix.len()).max() else {
        out.push_str("    (none — only plugin-installed middleware will run)\n");
        return out;
    };
    for (prefix, tag) in rows {
        let _ = writeln!(out, "    {prefix:prefix_width$}  ({tag})");
    }
    out
}

/// Write the launch banner to stderr.
///
/// `bound` is the actual bound `SocketAddr` (in case the user set
/// `port = 0` and the OS picked one). `routes` is the block rendered by
/// [`routes`] at `WebPlugin::build` time.
pub fn print(bound: SocketAddr, routes: &str) {
    let mut out = std::io::stderr().lock();
    let _ = writeln!(out);
    let _ = writeln!(out, "🪡  muxa serving at http://{bound}/");
    let _ = writeln!(out);
    let _ = write!(out, "{routes}");
    let _ = writeln!(out);
}

/// How the banner shows the app's fallback: whatever no mount matched.
const FALLBACK: &str = "/*";

fn display_prefix(prefix: &str) -> &str {
    if prefix.is_empty() { "/" } else { prefix }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn routes_are_aligned_and_tagged() {
        let mut registry = RouterRegistry::default();
        registry.mount("", axum::Router::new());
        registry.mount_manual("/admin", axum::Router::new());
        assert_eq!(
            routes(&registry),
            "  routes (mounted)\n    /       (auto)\n    /admin  (manual)\n"
        );
    }

    #[test]
    fn the_fallback_is_listed_last() {
        let mut registry = RouterRegistry::default();
        registry.fallback(axum::Router::new()).unwrap();
        registry.mount("/api", axum::Router::new());
        assert_eq!(
            routes(&registry),
            "  routes (mounted)\n    /api  (auto)\n    /*    (fallback)\n"
        );
    }

    #[test]
    fn no_routes_says_so() {
        assert!(routes(&RouterRegistry::default()).contains("(none"));
    }
}
