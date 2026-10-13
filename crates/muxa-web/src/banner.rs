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
    let prefix_width = registry
        .mounts()
        .map(|(prefix, _)| display_prefix(prefix).len())
        .max();
    let Some(prefix_width) = prefix_width else {
        out.push_str("    (none — only plugin-installed middleware will run)\n");
        return out;
    };
    for (prefix, mode) in registry.mounts() {
        let display = display_prefix(prefix);
        let tag = match mode {
            Mount::Auto => "auto",
            Mount::Manual => "manual",
        };
        let _ = writeln!(out, "    {display:prefix_width$}  ({tag})");
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
    fn no_routes_says_so() {
        assert!(routes(&RouterRegistry::default()).contains("(none"));
    }
}
