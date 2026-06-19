# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## What this is

muxa is a batteries-included Rust web framework layered on axum 0.8. It is a Cargo workspace: a set of `muxa-*` integration crates, a `muxa` facade crate that re-exports them behind Cargo features, and a `hello` example app. Edition 2024, MSRV 1.95. Published from `github.com/samublaise/muxa`.

The framework's lints and `clippy.toml` are intentionally kept **identical** to a downstream consumer app (katago-ws) so the framework and its consumer enforce the same bar — mirror any lint change in both places.

## Commands

Tasks are driven by `just` (see `justfile`):

- `just check` — the full CI gate: `fmt-check` + `lint` + `test` + `test-pgmq`. Run this before declaring work done.
- `just lint` — `cargo clippy --workspace --all-targets --no-deps -- -D warnings`. Warnings are errors.
- `just fmt` / `just fmt-check` — format / check formatting.
- `just test` — `cargo test --workspace` (default features).
- `just test-pgmq` — pgmq capability composition tests across **both** backends: `cargo test -p muxa-pgmq --features sqlx,diesel-async --tests`. These are not covered by `just test` and must be run separately.
- `just hello` / `just web-only` — run the example binaries (`crates`-less, under `examples/hello`).
- `just doc` — build + open workspace docs.

Run a single test the usual way, e.g. `cargo test -p muxa-core select_deeper` or `cargo test -p muxa-sqlx --test capability`.

Some crates have feature-gated tests/impls — when testing a specific crate, pass the features that light up the code under test (mirroring `test-pgmq`).

## Architecture

### Plugin chain → HList state

An app is built by chaining plugins, each `await`ed:

```rust
App::with_config_file("muxa.toml")
    .with_plugin(OtelPlugin).await?      // observability first
    .with_plugin(SqlitePlugin).await?    // a DB pool
    .with_plugin(WebPlugin::new(routes)).await?  // web ALWAYS last
    .run().await
```

There is **no `dyn Plugin` and no runtime registry** — composition is entirely compile-time. The key type is `AppBuilder<S>` where `S` is a **heterogeneous list (HList)** of plugin outputs (`muxa-core/src/state.rs`). Each `with_plugin` call:
1. reads the plugin's config slice from the figment,
2. calls `Plugin::build(cfg, &state, &mut ctx)`,
3. pushes the plugin's `Output` onto the HList, growing `S` by one type.

`HCons`/`HNil` build the list; the `Selector<T, Idx>` trait (with phantom indices `Here`/`There<I>`) extracts a value of a given type from anywhere in the list. This is how a later plugin reaches an earlier plugin's resource.

### Capability traits — cross-plugin wiring

A plugin requires a resource from an earlier plugin by adding a **capability trait bound on `S`** in its `impl Plugin<S>`. The canonical example is `HasPgExecutorFor<B, Idx>` (`muxa-core/src/capability.rs`): a single blanket impl says "any state HList containing `B::Pool` satisfies this capability." Pool crates (`muxa-sqlx`, `muxa-diesel`) each define a zero-size `PgmqBackend` marker (`SqlxBackend`, `DieselBackend`) naming their pool type; the consumer (`muxa-pgmq`) is generic as `PgmqPlugin<B, Idx>`. The `Idx` phantom exists only to satisfy Rust's unconstrained-type-param rule (E0207) and is normally inferred / defaulted to `Here` when the consumer immediately follows its provider in the chain.

This pattern is how you add a plugin that consumes another's resource without orphan-rule trouble: define the backend marker in the provider crate, keep the blanket capability impl in `muxa-core`.

### BuildCtx — the build-time side channels

`Plugin::build` also gets `&mut BuildCtx` (`muxa-core/src/ctx.rs`), the mutable channel separate from the (immutable, type-growing) state HList. Plugins use it to:
- `ctx.router.mount(prefix, router)` / `mount_manual(...)` / `layer(...)` — contribute routes and middleware.
- `ctx.tasks.spawn(name, |shutdown| async {...})` — register background tasks (these *must* be `Send + 'static`; they're spawned at `run()`).
- `ctx.telemetry` — push tracing-subscriber layers (otel/sentry attach via a reload handle, installed once).
- `ctx.set_serve_fn(...)` — exactly one plugin (the web plugin) fills this slot.

At `App::run`: background tasks are spawned, the router is `compose()`d (auto-mounts merged/nested by prefix, then layers applied), and the single `serve_fn` is invoked with the final router.

### Why WebPlugin is added last

`WebPlugin::new(routes)` takes a `routes: fn(&S) -> Router` callback. Its `build` runs that callback against the current state, so the state must already hold every other plugin's resource — hence web goes last. It also owns the axum serve loop + graceful-shutdown handshake via `set_serve_fn`.

### Non-Send build futures (deliberate)

`Plugin::build` returns a future that is **not** required to be `Send`. The build phase is awaited inline on the current thread, never spawned across runtimes. This is intentional: it lets plugins call sqlx `Executor<'_>` on `&mut PgConnection` inside `build` without tripping a known Rust HRTB/Send-inference limitation. The same limitation is why `muxa-pgmq` has per-backend `Plugin` impls in feature-gated submodules rather than one trait abstraction over backends. Background tasks, by contrast, *are* `Send + 'static` (enforced at `tokio::spawn`).

### Config

figment-based (`muxa-core/src/config.rs`). Layered, last wins:
1. one TOML file — `$MUXA_CONFIG` if set, else `./muxa.toml` (missing file is fine);
2. env vars prefixed `MUXA_`, `__` as the key separator — `MUXA_PGMQ__URL` → `pgmq.url`.

The `MUXA_` env prefix is the default (`DEFAULT_ENV_PREFIX`) but is **configurable** so a consumer app can namespace its own env vars: `App::with_env_prefix("MYAPP_")` or `App::with_config_file_and_env_prefix(path, "MYAPP_")` (and the free fns `load_figment_with_prefix` / `load_figment_from_with_prefix`). A custom prefix also renames the bootstrap config-path var to `{prefix}CONFIG`. The prefix includes its trailing separator.

Each plugin declares `const CONFIG_PREFIX` (e.g. `"pgmq"`) and a `Config: DeserializeOwned + Default`; an absent section falls back to `Config::default()`. Use `""` for "no config". Secret values (DB url, Sentry DSN) are wrapped in `secrecy::SecretString` so Debug/logs redact them.

## Workspace / feature conventions

- The `muxa` facade (`crates/muxa/Cargo.toml`) gates every integration behind a top-level feature and forwards the integration's own features. It uses `dep:` to gate optional deps and `muxa-pgmq?/sqlx`-style optional-activation so enabling a sub-feature without its parent is a no-op rather than a build error. `default = ["web", "otel"]`; `full` is the everything bundle.
- Internal `workspace.dependencies` use `default-features = false` so the facade controls which features cascade.
- Several third-party pins are deliberate and load-bearing — read the comments in the root `Cargo.toml` before bumping: `pgmq` is a pinned git rev of the Sagebati fork (tracks diesel-async 0.9); `aide` is exact-pinned (`=0.16.0-alpha.4`) so the `OpenApi` type unifies across crates; `diesel-async`/`diesel_migrations` track diesel 2.x. diesel-async is Postgres/MySQL only (no SQLite). pgmq is Postgres-only (no SQLite backend).
- App code conventionally does `use muxa::prelude::*;`, which brings in `App`, the `Plugin` trait, capability traits, and one canonical plugin per enabled feature.

## Crate map

- `muxa-core` — `Plugin` trait, HList `State`, capability traits, `App`/`AppBuilder`, `BuildCtx`, config, errors. No integrations.
- `muxa-telemetry` — `TelemetryRegistry` (subscriber layer kernel, reload handle).
- `muxa-web` — `WebPlugin` (serve loop + shutdown), `ratelimit` (tower_governor), `ApiPlugin` (with `openapi`).
- `muxa-sqlx` — `SqlxPlugin`/`SqlxPool` (Postgres) and `SqlitePlugin`/`SqlitePool`.
- `muxa-diesel` — `DieselPlugin` (diesel-async PG/MySQL), embedded migrations, sentry instrumentation.
- `muxa-pgmq` — `PgmqPlugin<B, Idx>`, install-only (`Output = ()`), consumes a pool via `HasPgExecutorFor`.
- `muxa-otel` / `muxa-sentry` — observability plugins (push tracing layers via `ctx.telemetry`).
- `muxa-openapi` — aide-based OpenAPI doc + re-exports `aide`/`schemars` at the pinned versions.
- `diesel-sentry` — framework-agnostic diesel→sentry instrumentation (no muxa dep); wrapped by `muxa-diesel`'s `sentry` feature.
- `muxa` — facade. `examples/hello` — end-to-end demo (`hello` = otel+sqlite+web; `web_only` = minimal).
