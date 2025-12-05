// crates/adapt/src/runtime/plugin_actor.rs

use crate::runtime::plugin::PluginRuntime;
use crate::{js::engine::BoaEngine, Error};
use serve::render::http::RequestContext;
use tokio::sync::{mpsc, oneshot};

/// Commands handled by the plugin actor.
///
/// The actor owns a single `PluginRuntime<BoaEngine>` instance and executes
/// all JS hooks on a single Tokio `LocalSet` thread.
enum PluginCommand {
    /// Call `init_all(&ctx)` on the runtime.
    InitAll {
        ctx: RequestContext,
        reply: oneshot::Sender<Result<(), Error>>,
    },

    /// Call `before_plugin(configured_id, &mut ctx)` for a single plugin.
    BeforePlugin {
        plugin_id: String,
        ctx: RequestContext,
        reply: oneshot::Sender<Result<RequestContext, Error>>,
    },

    /// Call `after_plugin(configured_id, &mut ctx)` for a single plugin.
    AfterPlugin {
        plugin_id: String,
        ctx: RequestContext,
        reply: oneshot::Sender<Result<RequestContext, Error>>,
    },

    /// Stop the actor loop.
    Shutdown,
}

/// Client handle used by the rest of the system (HTTP, etc.).
///
/// This is `Clone` so you can store it in Axum `State` and clone per request.
#[derive(Clone)]
pub struct PluginRuntimeClient {
    tx: mpsc::UnboundedSender<PluginCommand>,
}

impl PluginRuntimeClient {
    /// Spawn the plugin actor on the Tokio `LocalSet` thread.
    ///
    /// The `PluginRuntime<BoaEngine>` (and the underlying Boa context)
    /// will **never leave** that thread, satisfying the single-threaded
    /// requirement. This function **must** be called from within a
    /// `tokio::task::LocalSet::run_until(...)` context so that
    /// `spawn_local` is allowed.
    #[tracing::instrument(skip_all)]
    pub fn spawn(runtime: PluginRuntime<BoaEngine>) -> Self {
        let (tx, rx) = mpsc::unbounded_channel::<PluginCommand>();

        // Spawn the actor loop as a !Send task bound to the LocalSet thread.
        tokio::task::spawn_local(async move {
            plugin_actor_loop(runtime, rx).await;
        });

        Self { tx }
    }

    /// Call `init_all(ctx)` in the actor.
    #[tracing::instrument(skip_all)]
    pub async fn init_all(&self, ctx: RequestContext) -> Result<(), Error> {
        let (reply_tx, reply_rx) = oneshot::channel();

        self.tx
            .send(PluginCommand::InitAll {
                ctx,
                reply: reply_tx,
            })
            .map_err(|_| channel_error("plugin actor terminated before init_all"))?;

        reply_rx
            .await
            .map_err(|_| channel_error("plugin actor dropped init_all reply"))?
    }

    /// Run the per-plugin `before` hook against the runtime and return
    /// the updated `RequestContext`.
    #[tracing::instrument(skip_all)]
    pub async fn before_plugin(
        &self,
        plugin_id: impl Into<String>,
        ctx: RequestContext,
    ) -> Result<RequestContext, Error> {
        let plugin_id = plugin_id.into();
        let (reply_tx, reply_rx) = oneshot::channel();

        self.tx
            .send(PluginCommand::BeforePlugin {
                plugin_id,
                ctx,
                reply: reply_tx,
            })
            .map_err(|_| channel_error("plugin actor terminated before before_plugin"))?;

        reply_rx
            .await
            .map_err(|_| channel_error("plugin actor dropped before_plugin reply"))?
    }

    /// Run the per-plugin `after` hook against the runtime and return
    /// the updated `RequestContext`.
    #[tracing::instrument(skip_all)]
    pub async fn after_plugin(
        &self,
        plugin_id: impl Into<String>,
        ctx: RequestContext,
    ) -> Result<RequestContext, Error> {
        let plugin_id = plugin_id.into();
        let (reply_tx, reply_rx) = oneshot::channel();

        self.tx
            .send(PluginCommand::AfterPlugin {
                plugin_id,
                ctx,
                reply: reply_tx,
            })
            .map_err(|_| channel_error("plugin actor terminated before after_plugin"))?;

        reply_rx
            .await
            .map_err(|_| channel_error("plugin actor dropped after_plugin reply"))?
    }

    /// Fire-and-forget shutdown signal (no guarantee it’s processed).
    pub fn stop(&self) {
        let _ = self.tx.send(PluginCommand::Shutdown);
    }
}

/// Internal helper to map channel failures into a `Error`.
#[tracing::instrument(skip_all)]
fn channel_error(msg: &str) -> Error {
    // Reuse an existing variant instead of forcing you to change error.rs.
    // If you prefer a dedicated variant, you can add e.g. `Error::Actor(String)`
    // and switch to that here.
    Error::ThemeBootstrap(msg.to_string())
}

/// Actor event loop – runs on the Tokio `LocalSet` thread.
///
/// All interaction with `PluginRuntime<BoaEngine>` happens here, on a single
/// thread, so Boa's single-threaded requirement is upheld.
#[tracing::instrument(skip_all)]
async fn plugin_actor_loop(
    mut runtime: PluginRuntime<BoaEngine>,
    mut rx: mpsc::UnboundedReceiver<PluginCommand>,
) {
    while let Some(cmd) = rx.recv().await {
        match cmd {
            PluginCommand::InitAll { ctx, reply } => {
                let res = runtime.init_all(&ctx);
                let _ = reply.send(res);
            }

            PluginCommand::BeforePlugin {
                plugin_id,
                mut ctx,
                reply,
            } => {
                let res = (|| {
                    runtime.before_plugin(&plugin_id, &mut ctx)?;
                    Ok::<_, Error>(ctx)
                })();

                let _ = reply.send(res);
            }

            PluginCommand::AfterPlugin {
                plugin_id,
                mut ctx,
                reply,
            } => {
                let res = (|| {
                    runtime.after_plugin(&plugin_id, &mut ctx)?;
                    Ok::<_, Error>(ctx)
                })();

                let _ = reply.send(res);
            }

            PluginCommand::Shutdown => {
                // Break the loop; actor task will exit and drop the runtime.
                break;
            }
        }
    }
}
