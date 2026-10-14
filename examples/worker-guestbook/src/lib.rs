//! A guestbook JSON API on Cloudflare Workers, built with muxa.
//!
//! The plugin chain is the same as on a native app; only the end differs:
//! `into_router()` instead of `run()`, and the Worker's `fetch` handler
//! calling the router.

use axum::extract::State as AxumState;
use axum::http::StatusCode;
use axum::routing::get;
use axum::{Json, Router};
use muxa::prelude::*;
use muxa::{TelemetryRegistry, Vars};
use serde::{Deserialize, Serialize};
use tokio::sync::OnceCell;
use tower::Service as _;
use worker::wasm_bindgen::JsValue;
use worker::{Context, D1Database, Env, HttpRequest, ScheduleContext, ScheduledEvent, event};

// ── An app plugin: reads [guestbook] (MUXA_GUESTBOOK__* vars) and puts the
//    settings on the app state. Same code as on native muxa.

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
struct GuestbookConfig {
    title: String,
    max_len: usize,
}

impl Default for GuestbookConfig {
    fn default() -> Self {
        Self {
            title: "Guestbook".into(),
            max_len: 140,
        }
    }
}

#[derive(Clone)]
struct Settings(GuestbookConfig);

struct GuestbookPlugin;

impl<S: State> Plugin<S> for GuestbookPlugin {
    type Output = Settings;
    type Config = GuestbookConfig;
    const CONFIG_PREFIX: &'static str = "guestbook";

    async fn build(
        self,
        cfg: GuestbookConfig,
        _state: &S,
        _ctx: &mut BuildCtx,
    ) -> muxa::Result<Settings> {
        tracing::info!(title = %cfg.title, max_len = cfg.max_len, "guestbook configured");
        Ok(Settings(cfg))
    }
}

// ── Routes. The D1 binding is on the app state like any plugin resource:
//    `Local` makes the JS handle usable as state.

type Db = Local<D1Database>;

#[derive(Clone)]
struct AppState {
    settings: Settings,
    db: Db,
}

fn routes<S, SettingsIdx, DbIdx>(state: &S) -> Router
where
    S: Selector<Settings, SettingsIdx> + Selector<Db, DbIdx>,
{
    let state = AppState {
        settings: Selector::<Settings, SettingsIdx>::select(state).clone(),
        db: Selector::<Db, DbIdx>::select(state).dupe(),
    };
    Router::new()
        .route("/", get(home))
        .route("/entries", get(list_entries).post(add_entry))
        .with_state(state)
}

async fn home(AxumState(state): AxumState<AppState>) -> String {
    state.settings.0.title
}

#[derive(Serialize, Deserialize)]
struct Entry {
    name: String,
    message: String,
}

type ApiError = (StatusCode, String);

fn internal(err: worker::Error) -> ApiError {
    tracing::error!(%err, "D1 call failed");
    (StatusCode::INTERNAL_SERVER_ERROR, "database error".into())
}

// Awaiting a binding's JS call isn't `Send`; `#[worker::send]` lets axum
// accept the handler anyway (a Worker runs on one thread).
#[worker::send]
async fn list_entries(AxumState(state): AxumState<AppState>) -> Result<Json<Vec<Entry>>, ApiError> {
    let rows = state
        .db
        .prepare("SELECT name, message FROM entries ORDER BY id DESC LIMIT 50")
        .all()
        .await
        .map_err(internal)?;
    Ok(Json(rows.results().map_err(internal)?))
}

#[worker::send]
async fn add_entry(
    AxumState(state): AxumState<AppState>,
    Json(entry): Json<Entry>,
) -> Result<StatusCode, ApiError> {
    let max_len = state.settings.0.max_len;
    if entry.message.len() > max_len {
        return Err((
            StatusCode::UNPROCESSABLE_ENTITY,
            format!("message over {max_len} bytes"),
        ));
    }
    state
        .db
        .prepare("INSERT INTO entries (name, message) VALUES (?1, ?2)")
        .bind(&[
            JsValue::from_str(&entry.name),
            JsValue::from_str(&entry.message),
        ])
        .map_err(internal)?
        .run()
        .await
        .map_err(internal)?;
    tracing::debug!(name = %entry.name, "entry added");
    Ok(StatusCode::CREATED)
}

// ── The chain: config from the Worker's vars, the D1 binding as state, the
//    web plugin last — finished with into_router().

async fn app(env: &Env) -> muxa::Result<Router> {
    let db = env
        .d1("DB")
        .map_err(|err| muxa::Error::other(err.to_string()))?;
    let (router, _state) = App::with_vars(Vars::from_js(env.as_ref()))
        .with_plugin(GuestbookPlugin)
        .await?
        .with_plugin(Provide(Local::new(db)))
        .await?
        .with_plugin(WebPlugin::new(routes))
        .await?
        .into_router()?;
    Ok(router)
}

// ── Workers entry points.

#[event(fetch)]
async fn fetch(
    mut req: HttpRequest,
    env: Env,
    _ctx: Context,
) -> worker::Result<axum::http::Response<axum::body::Body>> {
    // Build the app once per isolate; concurrent first requests share the
    // build, and a failed build isn't cached, so the next request retries.
    static ROUTER: OnceCell<Router> = OnceCell::const_new();
    let router = ROUTER
        .get_or_try_init(|| async {
            // No process environment on Workers: take RUST_LOG from a var.
            if let Ok(directives) = env.var("RUST_LOG")
                && let Err(err) = TelemetryRegistry::install().set_level(&directives.to_string())
            {
                tracing::warn!(%err, "ignoring the RUST_LOG var");
            }
            app(&env).await
        })
        .await
        .map_err(|err| worker::Error::RustError(err.to_string()))?;

    req.extensions_mut().insert(env); // for handlers taking `Extension<Env>`
    Ok(router.clone().call(req).await?)
}

/// Nightly prune (the Cron Trigger in wrangler.toml), outside the router.
#[event(scheduled)]
async fn prune(_event: ScheduledEvent, env: Env, _ctx: ScheduleContext) {
    let Ok(db) = env.d1("DB") else { return };
    let pruned = db
        .prepare("DELETE FROM entries WHERE created_at < datetime('now', '-30 days')")
        .run()
        .await;
    if let Err(err) = pruned {
        tracing::warn!(%err, "nightly prune failed");
    }
}
