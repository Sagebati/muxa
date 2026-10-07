# muxa-telemetry

The telemetry kernel of [muxa](../muxa). It owns the process-wide `tracing` subscriber and lets plugins attach their own layers to it after it is installed.

It depends on no integration and knows none of the plugins that use it.

You rarely use this crate directly. `muxa-core` installs the subscriber when an `App` is created and hands the registry to plugins as `ctx.telemetry`.

## What gets installed

The first `TelemetryRegistry::install()` in a process sets a global subscriber made of:

- a `fmt` layer writing to stdout, filtered by `RUST_LOG` (default `info`);
- one reloadable slot holding a growable stack of plugin layers.

Installing up front means logs emitted before any observability plugin is built are not lost.

A process has one global subscriber, so it has one slot. Later `install()` calls return a handle to the same slot. A second `App` in the same process, in tests for example, shares it.

## Attaching a layer

Inside a plugin's `build`:

```rust
ctx.telemetry.add_layer(tracing_opentelemetry::layer().with_tracer(tracer));
```

The layer must be a `Layer<Registry> + Send + Sync + 'static`. It starts receiving spans and events emitted from that point on, filtered by the same `RUST_LOG` levels as stdout.

The slot holds one layer per type. Adding a layer whose type is already there replaces the old one, so rebuilding an app does not stack duplicates.

## Keeping an exporter out of its own way

A layer that ships events over the network would also capture what its own transport logs, and export that too. The plugin that owns such a layer names the crates on its export path:

```rust
ctx.telemetry.exclude_targets(&["h2", "hyper", "reqwest", "tower"]);
```

Those targets are then hidden from every plugin layer. Matching is by prefix, like a `RUST_LOG` directive: `"hyper"` also covers `hyper::client` and `hyper_util`. They still reach stdout, so export errors stay visible locally.

`muxa-otel` and `muxa-sentry` both do this for their transports.

## If you install your own subscriber

If a global subscriber already exists when the first `App` is created, the registry is detached: `add_layer` and `exclude_targets` become silent no-ops, so plugin layers are dropped. `is_installed()` tells you which case you are in. `TelemetryRegistry::external()` builds a detached registry on purpose.

## License

MIT OR Apache-2.0
