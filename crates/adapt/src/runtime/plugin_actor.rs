// crates/adapt/src/runtime/plugin_actor.rs

use crate::runtime::error::RuntimeError;
use crate::runtime::plugin::PluginRuntime;
use crate::{core::RequestContext, js::engine::BoaEngine};
use tokio::sync::{mpsc, oneshot};

/// Commands handled by the plugin actor.
enum PluginCommand {
    /// Call `init_all(&ctx)` on the runtime.
    InitAll {
        ctx: RequestContext,
        reply: oneshot::Sender<Result<(), RuntimeError>>,
    },

    /// Call `before_all(&mut ctx)` and return the updated context.
    BeforeAll {
        ctx: RequestContext,
        reply: oneshot::Sender<Result<RequestContext, RuntimeError>>,
    },

    /// Call `after_all(&mut ctx)` and return the updated context.
    AfterAll {
        ctx: RequestContext,
        reply: oneshot::Sender<Result<RequestContext, RuntimeError>>,
    },

    /// Stop the actor loop.
    Shutdown,
}

/// Client handle used by the rest of the system (HTTP, etc.).
///
/// This is `Clone` so you can store it in `State` and clone per request.
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
    pub fn spawn(runtime: PluginRuntime<BoaEngine>) -> Self {
        let (tx, rx) = mpsc::unbounded_channel::<PluginCommand>();

        // Spawn the actor loop as a !Send task bound to the LocalSet thread.
        tokio::task::spawn_local(async move {
            plugin_actor_loop(runtime, rx).await;
        });

        Self { tx }
    }

    /// Call `init_all(ctx)` in the actor.
    pub async fn init_all(&self, ctx: RequestContext) -> Result<(), RuntimeError> {
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

    /// Run `before_all` against the runtime and return the updated ctx.
    pub async fn before_all(&self, ctx: RequestContext) -> Result<RequestContext, RuntimeError> {
        let (reply_tx, reply_rx) = oneshot::channel();

        self.tx
            .send(PluginCommand::BeforeAll {
                ctx,
                reply: reply_tx,
            })
            .map_err(|_| channel_error("plugin actor terminated before before_all"))?;

        reply_rx
            .await
            .map_err(|_| channel_error("plugin actor dropped before_all reply"))?
    }

    /// Run `after_all` against the runtime and return the updated ctx.
    pub async fn after_all(&self, ctx: RequestContext) -> Result<RequestContext, RuntimeError> {
        let (reply_tx, reply_rx) = oneshot::channel();

        self.tx
            .send(PluginCommand::AfterAll {
                ctx,
                reply: reply_tx,
            })
            .map_err(|_| channel_error("plugin actor terminated before after_all"))?;

        reply_rx
            .await
            .map_err(|_| channel_error("plugin actor dropped after_all reply"))?
    }

    /// Fire-and-forget shutdown signal (no guarantee it’s processed).
    pub fn stop(&self) {
        let _ = self.tx.send(PluginCommand::Shutdown);
    }
}

/// Internal helper to map channel failures into a `RuntimeError`.
fn channel_error(msg: &str) -> RuntimeError {
    // Reuse an existing variant instead of forcing you to change error.rs.
    // If you prefer a dedicated variant, you can add e.g. `RuntimeError::Actor(String)`
    // and switch to that here.
    RuntimeError::ThemeBootstrap(msg.to_string())
}

/// Actor event loop – runs on the Tokio `LocalSet` thread.
///
/// All interaction with `PluginRuntime<BoaEngine>` happens here, on a single
/// thread, so Boa's single-threaded requirement is upheld.
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

            PluginCommand::BeforeAll { mut ctx, reply } => {
                let res = (|| {
                    runtime.before_all(&mut ctx)?;
                    Ok::<_, RuntimeError>(ctx)
                })();

                let _ = reply.send(res);
            }

            PluginCommand::AfterAll { mut ctx, reply } => {
                let res = (|| {
                    runtime.after_all(&mut ctx)?;
                    Ok::<_, RuntimeError>(ctx)
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::RequestContext;
    use serde_json::json;
    use std::collections::HashMap;
    use tokio::runtime::Builder as RtBuilder;
    use tokio::task::LocalSet;

    /// Build a minimal-but-valid RequestContext for actor calls.
    fn dummy_ctx() -> RequestContext {
        RequestContext::builder()
            .path("/test")
            .method("GET")
            .version("HTTP/1.1")
            .headers(json!({}))
            .params(json!({}))
            .content_meta(json!({ "title": "test" }))
            .theme_config(json!({}))
            .plugin_configs(HashMap::new())
            // No streams for this test
            .build()
    }

    // ─────────────────────────────────────────────────────────────────────
    // channel_error helper
    // ─────────────────────────────────────────────────────────────────────

    #[test]
    fn channel_error_wraps_message_in_runtime_error() {
        let err = channel_error("something went wrong");
        match err {
            RuntimeError::ThemeBootstrap(msg) => {
                assert!(
                    msg.contains("something went wrong"),
                    "expected message to be propagated, got {msg:?}"
                );
            }
            other => panic!("expected ThemeBootstrap, got {other:?}"),
        }
    }

    // ─────────────────────────────────────────────────────────────────────
    // Client send failures (tx -> rx closed before send)
    // ─────────────────────────────────────────────────────────────────────

    #[test]
    fn init_all_returns_channel_error_when_send_fails() {
        let rt = RtBuilder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();

        rt.block_on(async {
            let (tx, rx) = mpsc::unbounded_channel::<PluginCommand>();
            drop(rx); // simulate actor already terminated

            let client = PluginRuntimeClient { tx };
            let ctx = dummy_ctx();

            let res = client.init_all(ctx).await;
            assert!(res.is_err(), "expected error when channel is closed");
            match res.unwrap_err() {
                RuntimeError::ThemeBootstrap(msg) => {
                    assert!(
                        msg.contains("terminated before init_all"),
                        "unexpected message: {msg}"
                    );
                }
                other => panic!("expected ThemeBootstrap, got {other:?}"),
            }
        });
    }

    #[test]
    fn before_all_returns_channel_error_when_send_fails() {
        let rt = RtBuilder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();

        rt.block_on(async {
            let (tx, rx) = mpsc::unbounded_channel::<PluginCommand>();
            drop(rx);

            let client = PluginRuntimeClient { tx };
            let ctx = dummy_ctx();

            let res = client.before_all(ctx).await;
            assert!(res.is_err(), "expected error when channel is closed");
            match res.unwrap_err() {
                RuntimeError::ThemeBootstrap(msg) => {
                    assert!(
                        msg.contains("terminated before before_all"),
                        "unexpected message: {msg}"
                    );
                }
                other => panic!("expected ThemeBootstrap, got {other:?}"),
            }
        });
    }

    #[test]
    fn after_all_returns_channel_error_when_send_fails() {
        let rt = RtBuilder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();

        rt.block_on(async {
            let (tx, rx) = mpsc::unbounded_channel::<PluginCommand>();
            drop(rx);

            let client = PluginRuntimeClient { tx };
            let ctx = dummy_ctx();

            let res = client.after_all(ctx).await;
            assert!(res.is_err(), "expected error when channel is closed");
            match res.unwrap_err() {
                RuntimeError::ThemeBootstrap(msg) => {
                    assert!(
                        msg.contains("terminated before after_all"),
                        "unexpected message: {msg}"
                    );
                }
                other => panic!("expected ThemeBootstrap, got {other:?}"),
            }
        });
    }

    // ─────────────────────────────────────────────────────────────────────
    // Reply failures (oneshot dropped by actor side)
    // ─────────────────────────────────────────────────────────────────────

    #[test]
    fn init_all_returns_channel_error_when_reply_dropped() {
        let rt = RtBuilder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();

        rt.block_on(async {
            let (tx, mut rx) = mpsc::unbounded_channel::<PluginCommand>();
            let client = PluginRuntimeClient { tx };
            let ctx = dummy_ctx();

            let client_fut = client.init_all(ctx);
            let handler_fut = async {
                if let Some(PluginCommand::InitAll { reply, .. }) = rx.recv().await {
                    drop(reply); // simulate actor dropping reply sender
                }
            };

            let (res, _) = tokio::join!(client_fut, handler_fut);

            assert!(res.is_err(), "expected error when reply is dropped");
            match res.unwrap_err() {
                RuntimeError::ThemeBootstrap(msg) => {
                    assert!(
                        msg.contains("dropped init_all reply"),
                        "unexpected message: {msg}"
                    );
                }
                other => panic!("expected ThemeBootstrap, got {other:?}"),
            }
        });
    }

    #[test]
    fn before_all_returns_channel_error_when_reply_dropped() {
        let rt = RtBuilder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();

        rt.block_on(async {
            let (tx, mut rx) = mpsc::unbounded_channel::<PluginCommand>();
            let client = PluginRuntimeClient { tx };
            let ctx = dummy_ctx();

            let client_fut = client.before_all(ctx);
            let handler_fut = async {
                if let Some(PluginCommand::BeforeAll { reply, .. }) = rx.recv().await {
                    drop(reply);
                }
            };

            let (res, _) = tokio::join!(client_fut, handler_fut);

            assert!(res.is_err(), "expected error when reply is dropped");
            match res.unwrap_err() {
                RuntimeError::ThemeBootstrap(msg) => {
                    assert!(
                        msg.contains("dropped before_all reply"),
                        "unexpected message: {msg}"
                    );
                }
                other => panic!("expected ThemeBootstrap, got {other:?}"),
            }
        });
    }

    #[test]
    fn after_all_returns_channel_error_when_reply_dropped() {
        let rt = RtBuilder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();

        rt.block_on(async {
            let (tx, mut rx) = mpsc::unbounded_channel::<PluginCommand>();
            let client = PluginRuntimeClient { tx };
            let ctx = dummy_ctx();

            let client_fut = client.after_all(ctx);
            let handler_fut = async {
                if let Some(PluginCommand::AfterAll { reply, .. }) = rx.recv().await {
                    drop(reply);
                }
            };

            let (res, _) = tokio::join!(client_fut, handler_fut);

            assert!(res.is_err(), "expected error when reply is dropped");
            match res.unwrap_err() {
                RuntimeError::ThemeBootstrap(msg) => {
                    assert!(
                        msg.contains("dropped after_all reply"),
                        "unexpected message: {msg}"
                    );
                }
                other => panic!("expected ThemeBootstrap, got {other:?}"),
            }
        });
    }

    // ─────────────────────────────────────────────────────────────────────
    // Positive path: spawn actor + happy calls
    // ─────────────────────────────────────────────────────────────────────

    #[test]
    fn spawn_and_init_all_succeeds_with_empty_runtime() {
        let rt = RtBuilder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let local = LocalSet::new();

        rt.block_on(local.run_until(async {
            let engine = BoaEngine::new();
            let runtime = PluginRuntime::new(engine).expect("No Plugin Runtime");

            let client = PluginRuntimeClient::spawn(runtime);

            let ctx = dummy_ctx();
            let res = client.init_all(ctx).await;

            assert!(res.is_ok(), "init_all should succeed with empty runtime");

            client.stop(); // best-effort shutdown
        }));
    }

    #[test]
    fn spawn_then_before_all_roundtrips_context() {
        let rt = RtBuilder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let local = LocalSet::new();

        rt.block_on(local.run_until(async {
            let engine = BoaEngine::new();
            let runtime = PluginRuntime::new(engine).expect("No Plugin Runtime");

            let client = PluginRuntimeClient::spawn(runtime);

            let ctx = dummy_ctx();
            let path_before = ctx.req_path.clone();

            let new_ctx = client
                .before_all(ctx.clone())
                .await
                .expect("before_all should succeed");
            assert_eq!(
                new_ctx.req_path, path_before,
                "before_all with empty runtime should not mutate ctx path"
            );

            client.stop();
        }));
    }

    #[test]
    fn spawn_then_after_all_roundtrips_context() {
        let rt = RtBuilder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let local = LocalSet::new();

        rt.block_on(local.run_until(async {
            let engine = BoaEngine::new();
            let runtime = PluginRuntime::new(engine).expect("No Plugin Runtime");

            let client = PluginRuntimeClient::spawn(runtime);

            let ctx = dummy_ctx();
            let path_before = ctx.req_path.clone();

            let new_ctx = client
                .after_all(ctx.clone())
                .await
                .expect("after_all should succeed");
            assert_eq!(
                new_ctx.req_path, path_before,
                "after_all with empty runtime should not mutate ctx path"
            );

            client.stop();
        }));
    }

    // ─────────────────────────────────────────────────────────────────────
    // stop() sends Shutdown command
    // ─────────────────────────────────────────────────────────────────────

    #[test]
    fn stop_sends_shutdown_command() {
        let rt = RtBuilder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();

        rt.block_on(async {
            let (tx, mut rx) = mpsc::unbounded_channel::<PluginCommand>();
            let client = PluginRuntimeClient { tx };

            client.stop();

            // We should see a Shutdown command on the channel.
            if let Some(cmd) = rx.recv().await {
                match cmd {
                    PluginCommand::Shutdown => { /* ok */ }
                    _other => panic!("expected Shutdown, got different command",),
                }
            } else {
                panic!("expected a Shutdown command to be sent");
            }
        });
    }
}
