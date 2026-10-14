//! [`Provide`]: put a ready-made value on the app state, and, on wasm,
//! [`Local`]: let a JS handle be that value.

use crate::ctx::BuildCtx;
use crate::error::Result;
use crate::plugin::Plugin;
use crate::state::State;

/// A plugin that pushes a value you already have onto the app state, so
/// later plugins and the routes callback reach it with `Selector` like any
/// plugin's resource. No config, no routes.
///
/// ```ignore
/// App::with_vars(vars)
///     .with_plugin(Provide(http_client)).await?
///     .with_plugin(WebPlugin::new(routes)).await?   // routes: S: Selector<HttpClient, _>
/// ```
///
/// On Cloudflare Workers, wrap a binding in [`Local`] first:
/// `Provide(Local::new(env.d1("DB")?))`.
pub struct Provide<T>(pub T);

impl<S, T> Plugin<S> for Provide<T>
where
    S: State,
    T: Send + Sync + 'static,
{
    type Output = T;
    type Config = ();
    const CONFIG_PREFIX: &'static str = "";

    async fn build(self, _cfg: (), _state: &S, _ctx: &mut BuildCtx) -> Result<T> {
        Ok(self.0)
    }
}

#[cfg(target_family = "wasm")]
pub use local::Local;

#[cfg(target_family = "wasm")]
mod local {
    use core::ops::Deref;
    use std::sync::Arc;

    use send_wrapper::SendWrapper;

    /// A value pinned to the thread it was created on, usable as app state on
    /// wasm — chiefly a JS handle such as a Cloudflare Worker's D1 or KV
    /// binding, which is neither `Send` nor `Sync` while plugin outputs and
    /// axum state must be both.
    ///
    /// A Worker runs each isolate on one thread, so the handle never moves.
    /// That is checked rather than assumed: touching or dropping the value
    /// from another thread panics (it is a [`SendWrapper`]), so a wrong
    /// assumption is a loud failure, never undefined behaviour.
    ///
    /// Cloning shares the value (`Arc`), so a `Local` is cheap to hand to
    /// axum as router state. It derefs to the value. Futures that await the
    /// handle's JS calls still aren't `Send`: mark such handlers with
    /// `#[worker::send]`.
    ///
    /// ```ignore
    /// let db = Local::new(env.d1("DB")?);
    /// App::with_vars(Vars::from_js(env.as_ref()))
    ///     .with_plugin(Provide(db)).await?
    ///     .with_plugin(WebPlugin::new(routes)).await?   // S: Selector<Local<D1Database>, _>
    /// ```
    pub struct Local<T>(Arc<SendWrapper<T>>);

    impl<T> Local<T> {
        /// Pin `value` to the current thread.
        pub fn new(value: T) -> Self {
            Self(Arc::new(SendWrapper::new(value)))
        }
    }

    impl<T> Clone for Local<T> {
        fn clone(&self) -> Self {
            Self(Arc::clone(&self.0))
        }
    }

    impl<T> dupe::Dupe for Local<T> {}

    impl<T> Deref for Local<T> {
        type Target = T;

        fn deref(&self) -> &T {
            &self.0
        }
    }

    impl<T> core::fmt::Debug for Local<T> {
        fn fmt(&self, fmt: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
            fmt.write_str("Local(..)")
        }
    }
}

#[cfg(test)]
mod tests {
    use figment::Figment;

    use super::*;
    use crate::state::{HNil, Selector};
    use crate::{AppBuilder, Here};

    #[tokio::test]
    async fn provide_puts_the_value_on_the_state() {
        let app = AppBuilder::<HNil>::with_figment(Figment::new())
            .with_plugin(Provide(String::from("shared")))
            .await
            .unwrap();
        let value: &String = Selector::<String, Here>::select(app.state());
        assert_eq!(value, "shared");
    }
}
