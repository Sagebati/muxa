# muxa-core

The core of [muxa](../muxa): the `Plugin` trait, the typed application state, `App` / `AppBuilder`, `BuildCtx`, config loading, `RunMode` and the error type. It contains no integrations.

Applications normally use the [`muxa`](../muxa) facade, which re-exports everything here. Depend on `muxa-core` directly when you write a plugin crate.

```toml
[dependencies]
muxa-core = { git = "https://github.com/Sagebati/muxa" }
```

## Writing a plugin

A plugin reads its config section, builds one resource, and can register routes, background tasks and middleware on the way.

```rust
use axum::Router;
use axum::routing::get;
use muxa_core::prelude::*;
use serde::Deserialize;

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct GreeterConfig {
    greeting: String,
}

#[derive(Clone)]
struct Greeter {
    greeting: String,
}

struct GreeterPlugin;

impl<S: State> Plugin<S> for GreeterPlugin {
    type Output = Greeter;                             // pushed onto the app state
    type Config = GreeterConfig;
    const CONFIG_PREFIX: &'static str = "greeter";     // [greeter] / MUXA_GREETER__*

    async fn build(self, cfg: GreeterConfig, _state: &S, ctx: &mut BuildCtx) -> Result<Greeter> {
        ctx.router
            .mount("/greeter", Router::new().route("/health", get(|| async { "ok" })));
        ctx.tasks.spawn("greeter-heartbeat", |shutdown| async move {
            shutdown.cancelled().await;
        });
        Ok(Greeter { greeting: cfg.greeting })
    }
}
```

- `Output` is `()` for a plugin that only registers routes, tasks or middleware.
- `Config` must implement `Default`. When the section is missing, the plugin gets the default. A section that is present but does not deserialize is an error, so a typo fails startup. Set `CONFIG_PREFIX` to `""` for a plugin with no config.
- A plugin sees only the sections it names, never the whole configuration. To build `Config` from more than one section, override `read_config` and call `sections.get("name")` for each.
- The future returned by `build` does not have to be `Send`. It is awaited inline, never spawned. Background tasks do have to be `Send + 'static`.
- If `build` fails, `with_plugin` returns `Error::PluginBuild` tagged with the plugin's type name.

## State

`AppBuilder<S>` carries `S`, a heterogeneous list of every plugin output so far. Each `with_plugin` grows it by one type. There is no `dyn Plugin` and no runtime registry.

`Selector<T, Idx>` borrows a `T` from anywhere in the list. `Idx` is a phantom position that the compiler infers:

```rust
let app = App::default().with_plugin(GreeterPlugin).await?;
let greeter: &Greeter = Selector::<Greeter, _>::select(app.state());
```

### Requiring an earlier plugin's output

Put the bound on `S`. The plugin type has to carry the index parameter, otherwise the impl is rejected with E0207 (unconstrained type parameter):

```rust
use std::marker::PhantomData;
use muxa_core::Here;

struct ShoutPlugin<Idx = Here>(PhantomData<fn() -> Idx>);

impl<S, Idx> Plugin<S> for ShoutPlugin<Idx>
where
    S: State + Selector<Greeter, Idx>,
    Idx: 'static,
{
    type Output = ();
    type Config = ();
    const CONFIG_PREFIX: &'static str = "";

    async fn build(self, _cfg: (), state: &S, _ctx: &mut BuildCtx) -> Result<()> {
        let greeter: &Greeter = state.select();
        tracing::info!(greeting = %greeter.greeting, "greeter is available");
        Ok(())
    }
}
```

```rust
App::default()
    .with_plugin(GreeterPlugin).await?
    .with_plugin(ShoutPlugin(PhantomData)).await?
```

Adding `ShoutPlugin` to a chain that has no `GreeterPlugin` before it is a compile error.

### Capability traits

When several crates can provide the same kind of resource, a capability trait names the requirement instead of a concrete type. `HasPgExecutorFor<B, Idx>` is the one shipped here: it holds for any state that contains `B::Pool`, where `B` is a `PgBackend` marker defined by a pool crate (`SqlxBackend` in `muxa-sqlx`, `DieselBackend` in `muxa-diesel`). [`muxa-pgmq`](../muxa-pgmq) is written against it and so works over either pool.

To add a provider, define a marker type in the provider crate and implement `PgBackend` for it. The blanket impl in this crate does the rest.

## BuildCtx

`build` receives `&mut BuildCtx`, the mutable side of the build:

| Member | Use |
|---|---|
| `ctx.mode` | `RunMode::Development` or `RunMode::Production` |
| `ctx.router.mount(prefix, router)` | add routes: merged for `""` or `"/"`, nested under any other prefix |
| `ctx.router.mount_manual(prefix, router)` | hold routes back until someone calls `take_manual(prefix)` |
| `ctx.router.layer(f)` | wrap the final router. Applied in registration order, so a layer registered later ends up outside the earlier ones |
| `ctx.tasks.spawn(name, f)` | background task, started at `run()`, handed a `ShutdownToken` |
| `ctx.shutdown` | the app-wide cancellation token |
| `ctx.telemetry.add_layer(layer)` | attach a `tracing` layer to the shared subscriber ([`muxa-telemetry`](../muxa-telemetry)) |
| `ctx.telemetry.exclude_targets(&[..])` | hide the crates on an exporter's own network path from plugin layers |
| `ctx.set_serve_fn(f)` | register the serve loop. Only one plugin may; a second call is an error |

The configuration is not in `BuildCtx`. A plugin gets its own section as the `cfg` argument of `build` and nothing else.

`App::run()` spawns the tasks, composes the router and calls the serve function. When serving ends it cancels the shutdown token and waits up to ten seconds for the tasks to return, so a task can flush or drain before the process exits. With no serve function registered, which means no serving plugin in the chain, it returns an error.

Creating an `App` also installs the global `tracing` subscriber (stdout, filtered by `RUST_LOG`, default `info`), unless one is already installed.

## Config

Config is a [figment](https://docs.rs/figment) built from two layers, last one wins:

1. one TOML file: `$MUXA_CONFIG` if set, otherwise `./muxa.toml`. A missing file is fine.
2. environment variables prefixed `MUXA_`, with `__` between keys: `MUXA_PGMQ__URL` sets `pgmq.url`.

| Constructor | Loader | Config file | Env prefix |
|---|---|---|---|
| `App::default()` | `load_figment()` | `$MUXA_CONFIG` or `./muxa.toml` | `MUXA_` |
| `App::with_config_file(path)` | `load_figment_from(path)` | `path` | `MUXA_` |
| `App::with_env_prefix(p)` | `load_figment_with_prefix(p)` | `${p}CONFIG` or `./muxa.toml` | `p` |
| `App::with_config_file_and_env_prefix(path, p)` | `load_figment_from_with_prefix(path, p)` | `path` | `p` |
| `App::with_figment(figment)` | none | yours | yours |

The prefix includes its trailing separator: `"MYAPP_"`, not `"MYAPP"`.

The application owns the merged configuration and can read any of it, for settings that belong to no plugin:

```rust
let limits: Limits = app.figment().extract_inner("limits")?;
```

## RunMode

`ctx.mode` is resolved once, when the app is created:

1. from the top-level `env` key (`env = "production"` in the file, or `MUXA_ENV=production`). It is case-insensitive and accepts `dev` / `development` / `debug` and `prod` / `production` / `release`.
2. otherwise from the build profile: a debug build is `Development`, a release build is `Production`.

Plugins use it for environment-dependent defaults. `muxa-sentry`, for example, derives its `environment` tag and its sample rate from it.

## Errors

`muxa_core::Error` has four variants: `PluginBuild`, `Config`, `Io` and `Other`. `Error::other(err)` wraps any boxable error. `muxa_core::Result<T>` is the matching alias.

## License

MIT OR Apache-2.0
