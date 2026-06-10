//! Rocket-style launch banner. Printed once to stderr when the server
//! binds, showing the bound URL, mounted router prefixes, and the merged
//! figment configuration as TOML.

use std::io::Write as _;
use std::net::SocketAddr;

use muxa_core::Mount;

/// Write the launch banner to stderr.
///
/// `bound` is the actual bound `SocketAddr` (in case the user set
/// `port = 0` and the OS picked one). `mounts` is the snapshot of
/// router prefixes taken from `RouterRegistry::mounts()` at
/// `WebPlugin::build` time.
pub fn print(bound: SocketAddr, figment: &figment::Figment, mounts: &[(String, Mount)]) {
    let mut out = std::io::stderr().lock();
    let _ = writeln!(out);
    let _ = writeln!(out, "🪡  muxa serving at http://{bound}/");
    let _ = writeln!(out);

    // ── mounts ──
    let _ = writeln!(out, "  routes (mounted)");
    if mounts.is_empty() {
        let _ = writeln!(
            out,
            "    (none — only plugin-installed middleware will run)"
        );
    } else {
        let prefix_width = mounts
            .iter()
            .map(|(prefix, _)| display_prefix(prefix).len())
            .max()
            .unwrap_or(1);
        for (prefix, mode) in mounts {
            let display = display_prefix(prefix);
            let tag = match mode {
                Mount::Auto => "auto",
                Mount::Manual => "manual",
            };
            let _ = writeln!(out, "    {display:prefix_width$}  ({tag})");
        }
    }
    let _ = writeln!(out);

    // ── effective configuration ──
    match figment.extract::<toml::Value>() {
        Ok(value) => match toml::to_string_pretty(&value) {
            Ok(rendered) if !rendered.trim().is_empty() => {
                let _ = writeln!(out, "  configuration");
                for line in rendered.lines() {
                    let _ = writeln!(out, "    {line}");
                }
                let _ = writeln!(out);
            }
            Ok(_) => {
                let _ = writeln!(out, "  configuration  (empty figment)");
                let _ = writeln!(out);
            }
            Err(err) => {
                let _ = writeln!(out, "  configuration  (failed to serialize as TOML: {err})");
                let _ = writeln!(out);
            }
        },
        Err(err) => {
            let _ = writeln!(out, "  configuration  (could not extract figment: {err})");
            let _ = writeln!(out);
        }
    }
}

fn display_prefix(prefix: &str) -> &str {
    if prefix.is_empty() { "/" } else { prefix }
}
