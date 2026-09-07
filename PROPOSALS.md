# muxa — project review & change proposals

A full review of the workspace (all crates, examples, manifests, justfile) with
concrete change proposals, ordered by priority. Nothing here is implemented yet
— each item is a proposal.

---

## A. Correctness & security

### A1. The launch banner prints secrets to stderr

`muxa-web/src/banner.rs` renders the **entire merged figment** as TOML:

```rust
match figment.extract::<toml::Value>() { ... }
```

The figment holds raw strings — `sqlx.url`, `diesel.url`, `sentry.dsn`,
anything from `MUXA_*` env vars. The `SecretString` wrappers only protect the
*typed* configs after extraction; the banner bypasses them and prints
`postgres://user:password@host/db` in clear text on every startup (banner
defaults to **on**). This directly undercuts the project's own
secrets-are-redacted convention.

**Proposal:** redact values whose key path matches a deny-list
(`*.url`, `*.dsn`, `*password*`, `*secret*`, `*token*`, `*key*`) before
rendering — print `url = "«redacted»"`. Alternatively print keys only. Redaction
should be the default with no way to opt into printing secrets.

### A2. `SqlxConfig.url` is a plain `String`

`muxa-diesel` and `muxa-sentry` wrap their secrets in `secrecy::SecretString`;
`muxa-sqlx`'s Postgres URL — the same class of secret — is a bare `String`
(`crates/muxa-sqlx/src/pg.rs`), so any `Debug` of the config leaks credentials.

**Proposal:** change `SqlxConfig.url` to `SecretString`, mirror the
`url_is_redacted_in_debug` test from `muxa-diesel`. (SQLite URLs carry no
credentials; leave `SqliteConfig` as is.)

### A3. Shutdown flush race: background tasks are never joined

`AppBuilder::run` (`muxa-core/src/app.rs`) spawns tasks and **drops the
`JoinHandle`s**. After the serve future returns, `run()` returns immediately
and `main` exits. The `otel-shutdown` task (which flushes the OTLP batch
exporters on `shutdown.cancelled()`) races process exit — the final batch of
traces/metrics/logs is frequently lost, which is exactly what that task exists
to prevent. Two related gaps:

- If the serve loop ends for any reason other than the ctrl-c bridge (bind
  error mid-run, listener error), the shutdown token is **never cancelled**, so
  tasks waiting on it never even start their cleanup.
- Task panics are invisible (handles dropped).

**Proposal:** in `run()`: keep the `JoinHandle`s; after `serve(router).await`
returns (ok or err), call `self.ctx.shutdown.cancel()`, then `join` all task
handles with a bounded grace period (e.g. 10s, configurable via `[web]` or a
core key), logging tasks that time out or panicked. Then return the serve
result.

### A4. Bind address parsing rejects IPv6 hosts and hostnames

`serve_loop` does `format!("{}:{}", cfg.host, cfg.port).parse::<SocketAddr>()`.
Verified: `"::1:3000"` and `"localhost:3000"` both fail to parse. So
`host = "::1"` or `host = "localhost"` — perfectly reasonable configs — error
out with an opaque `AddrParseError`.

**Proposal:** try `host.parse::<IpAddr>()` first and build the
`SocketAddr` from `(ip, port)` (this fixes bare IPv6); fall back to
`tokio::net::lookup_host((host, port))` for hostnames. Also improve the error
to name the offending config key.

### A5. A malformed Sentry DSN is silently swallowed

`muxa-sentry`:

```rust
let dsn = raw_dsn.and_then(|raw| raw.parse::<sentry::types::Dsn>().ok());
```

A typo'd DSN yields a no-op client, while the startup log — gated on
`raw_dsn.is_some()`, not on `dsn` — still says `muxa-sentry: initialized`. The
user believes error tracking is live; nothing is ever sent.

**Proposal:** fail the build (`Err(Error::other("sentry: invalid DSN"))`) on a
non-empty DSN that doesn't parse. Misconfigured observability should be loud at
startup, not discovered during an incident.

### A6. Graceful shutdown ignores SIGTERM

The `graceful-shutdown` feature bridges only `ctrl_c` (SIGINT). Containers and
orchestrators (Docker, Kubernetes, systemd) send **SIGTERM** — today those
deployments get a hard kill: no drain, no OTLP/Sentry flush.

**Proposal:** on unix, also listen for `signal(SignalKind::terminate())` and
cancel the token on whichever fires first. This is the deployment environment a
`0.0.0.0:3000` default is clearly aimed at.

### A7. `read_config` silently defaults on *any* figment error

`Plugin::read_config` treats every `find_value` error as "section absent" and
returns `Config::default()`. A broken `muxa.toml` (syntax error) therefore
doesn't fail startup — every plugin quietly runs on defaults. Combined with A5,
a bad config file can produce a running-but-misconfigured app with no signal.

**Proposal:** distinguish error kinds: a missing-key error → default (current
behavior); any other figment error (file parse failure, type mismatch) →
propagate as `Error::Config`. figment's `Error::missing()` / error `Kind` makes
this distinction possible.

---

## B. API & design gaps

### B1. Worker-mode apps can't `run()`

`run()` errors when no serve function is registered. But muxa ships `muxa-pgmq`
— a queue-consumer binary (background tasks only, no HTTP) is a natural muxa
app, and today it's impossible without a dummy `WebPlugin`.

**Proposal:** when no `serve_fn` is registered **and at least one background
task exists**, fall back to "worker mode": install the signal bridge (see A6),
then wait on the shutdown token and join tasks. Keep the current error only for
the truly-empty case (no serve fn, no tasks), which is almost certainly a
mistake.

### B2. `TelemetryRegistry::external()` is unreachable through the public API

`BuildCtx::new` unconditionally calls `TelemetryRegistry::install()`, and every
`App` constructor funnels through it. An app that installs its own
`tracing-subscriber` *after* building the `App` gets hijacked (muxa's
`try_init` wins); `external()` exists for exactly this case but nothing can
pass it in.

**Proposal:** add a constructor knob, e.g.
`App::with_figment_and_telemetry(figment, TelemetryRegistry)` or a
`AppBuilder::with_external_telemetry()` that must be called before the first
plugin. Document that muxa owns the subscriber by default.

### B3. otel feature semantics: `otlp-tonic` alone exports nothing

The docs table in `muxa-otel/src/lib.rs` says `traces` "builds an
`SdkTracerProvider`" and `otlp-tonic` "implies `traces`". In the code, the
tracer provider + span exporter are built only under
`#[cfg(feature = "tracing-bridge")]`. So `otel-otlp-tonic` + endpoint, without
`otel-tracing-bridge`, silently exports no spans — contradicting the table.
Since the only span source in a muxa app *is* `tracing`, the bridge isn't
really optional for trace export.

**Proposal:** either (a) make `otlp-tonic`/`otlp-http` imply `tracing-bridge`,
or (b) gate the provider on `traces` and only the subscriber layer on
`tracing-bridge` — and fix the table either way. (a) matches user intent best.

### B4. `RouterRegistry::layer` ordering doc is inverted

The doc says "Pushed-in-order; outermost layer first", but `compose()` applies
`out = layer(out)` in registration order, so the **last registered** layer_fn
wraps everything and is outermost (consistent with the sentry plugin's own
comment about axum layer order). Anyone sequencing auth/tracing middleware off
this doc will get it backwards.

**Proposal:** fix the doc; add a two-layer ordering unit test so the contract
is pinned.

### B5. Duplicate resource types make `Selector` unusable

Two plugins producing the same output type (e.g. two `SqlxPlugin`s for two
databases) put two `SqlxPool`s in the HList; `Selector<SqlxPool, _>` inference
then fails (ambiguous) with an HList-flavored error the user has no tools to
decode, and `HasPgExecutorFor` can't say *which* pool.

**Proposal (v0):** document the one-instance-per-type rule prominently on
`Plugin` and `Selector`. **Proposal (v1):** offer a tagging story — e.g.
`SqlxPlugin::<Tag>::tagged()` producing `Tagged<SqlxPool, Tag>` — so multiple
same-type resources become distinct types and existing machinery just works.

### B6. Mode-aware defaults now that `RunMode` exists

`RunMode` landed but only muxa-sentry uses it. Two obvious consumers:

- **Bind host:** default `0.0.0.0` unconditionally is a mild dev footgun
  (exposes the dev server to the LAN, triggers firewall prompts). Rocket-style:
  `127.0.0.1` in `Development`, `0.0.0.0` in `Production`; explicit config wins.
- **Banner:** default on in `Development`, off in `Production` (where stderr is
  log-scraped and the config dump — see A1 — is least welcome).

### B7. Error messages hardcode the `MUXA_` prefix

`muxa-sqlx`/`muxa-diesel` errors say e.g. "set in config or `MUXA_SQLX__URL`",
but the env prefix is configurable — a `MYAPP_`-prefixed app gets misleading
advice. **Proposal:** store the active prefix in `BuildCtx` (it's known at
figment construction) and let messages/docs reference it.

---

## C. Repo hygiene & release readiness

### C1. `README.md` is declared but doesn't exist

`workspace.package` sets `readme = "README.md"`; there is no README anywhere.
`cargo publish` fails on this, and the GitHub landing page is empty. muxa has a
genuinely distinctive pitch (compile-time plugin composition, HList state,
capability traits) that deserves a front page: quickstart (`web_only` →
`hello`), the plugin-chain diagram, feature table, MSRV.

### C2. No license files

`license = "MIT OR Apache-2.0"` with no `LICENSE-MIT` / `LICENSE-APACHE` in the
repo. Add both (standard texts) before anything is published.

### C3. No CI

The justfile says `just check` is "what CI should run", but there is no
`.github/workflows/`. Add a workflow: `fmt-check` + `lint` + `test` on stable
(MSRV 1.95 job too), plus `test-pgmq` (compile-only, needs no DB), with cargo
caching. The lint bar (`-D warnings`, heavy clippy set) only holds if a robot
enforces it.

### C4. `repository` metadata points at the wrong owner

`workspace.package.repository = "https://github.com/samublaise/muxa"` (and
CLAUDE.md repeats it), but the actual remote is `github.com/Sagebati/muxa`.
Align to the canonical home in both places.

### C5. The pgmq git dependency blocks crates.io

`pgmq` is a pinned git rev of the Sagebati fork; crates.io forbids git deps, so
`muxa-pgmq` (and the facade's `pgmq` feature) cannot be published as-is. Not
actionable today, but worth a tracked plan: publish the fork under its own
crate name, or vendor the thin installer/queue surface muxa actually uses.

### C6. `full` isn't full

CLAUDE.md calls `full` "the everything bundle", but it omits `ratelimit`,
`sqlite`, all `diesel*` features, and every otel export feature
(`otel-otlp-*`, `otel-metrics`, `otel-logs`, `otel-tracing-bridge`) — so
`features = ["full"]` gives an otel plugin that exports nothing. Either extend
it (diesel and sqlx do coexist) or rename/re-document it as the
sqlx-flavored bundle it actually is.

### C7. No runtime integration tests

All pgmq/capability tests are compile-only (deliberate and good), but nothing
exercises a real Postgres: pgmq install SQL, diesel migrations
(`AsyncConnectionWrapper` + `spawn_blocking` + single transaction), pool
startup. A `#[ignore]`-by-default (or env-gated) integration test against the
existing docker-compose Postgres, wired as `just test-integration`, would catch
regressions the type system can't.

---

## Suggested order of attack

| Batch | Items | Theme |
|-------|-------|-------|
| 1 | A1, A2, A5 | stop leaking / silently dropping secrets & misconfig |
| 2 | A3, A6, B1 | shutdown correctness + worker mode (one `run()` rework) |
| 3 | A4, A7, B4 | small, self-contained fixes |
| 4 | C1–C4 | release hygiene (README, licenses, CI, metadata) |
| 5 | B2, B3, B6, B7, C6 | API polish |
| 6 | B5, C5, C7 | larger design/roadmap items |

Batches 1–4 are low-risk and could land as a handful of focused PRs; batch 6
items deserve their own design discussion.
