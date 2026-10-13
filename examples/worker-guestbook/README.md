# worker-guestbook

A guestbook JSON API on [Cloudflare Workers](https://developers.cloudflare.com/workers/), built with muxa and [workers-rs](https://github.com/cloudflare/workers-rs), on a D1 database.

It shows the whole Workers setup:

- **The same plugin chain as a native app**, finished with `into_router()` instead of `run()`. `WebPlugin` only mounts the routes on wasm.
- **Config from vars:** `App::with_vars(Vars::from_js(env.as_ref()))`. `MUXA_GUESTBOOK__TITLE` in `wrangler.toml` sets `guestbook.title`, which the app's own `GuestbookPlugin` reads.
- **A D1 binding as app state:** `Provide(Local::new(env.d1("DB")?))`, reached in the routes callback with `Selector` like any plugin resource.
- **The `fetch` handler,** which is your code: it builds the app once per isolate, applies the `RUST_LOG` var and calls the router.
- **A Cron Trigger** for periodic work, since Workers have no background tasks.

## Routes

| | |
|---|---|
| `GET /` | the configured title |
| `GET /entries` | the 50 latest entries |
| `POST /entries` | add `{"name": "…", "message": "…"}`; 422 if the message is longer than `guestbook.max_len` |

## Run it

Needs the `wasm32-unknown-unknown` target (`rustup target add wasm32-unknown-unknown`) and [wrangler](https://developers.cloudflare.com/workers/wrangler/) (`npx wrangler`).

```bash
npx wrangler d1 migrations apply guestbook --local
npx wrangler dev
```

```bash
curl -X POST localhost:8787/entries -H 'content-type: application/json' \
     -d '{"name": "ada", "message": "hello"}'
curl localhost:8787/entries
```

To deploy, create the database with `npx wrangler d1 create guestbook`, put its id in `wrangler.toml`, apply the migrations with `--remote`, then run `npx wrangler deploy`.

## Not a workspace member

The example builds for wasm only and depends on workers-rs, which muxa itself never does, so it has its own `[workspace]`. `just check-wasm` lints it for `wasm32-unknown-unknown`.
