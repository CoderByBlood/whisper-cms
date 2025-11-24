// crates/adapt/src/runtime/theme_actor.rs

use crate::js::engine::BoaEngine;
use crate::runtime::bootstrap::BoundTheme;
use crate::runtime::error::RuntimeError;
use serve::ctx::http::{RequestContext, ResponseBodySpec};
use std::collections::HashMap;
use tokio::sync::{mpsc, oneshot};

/// Commands handled by the theme actor.
enum ThemeCommand {
    /// Call `init` on all themes with the given context.
    InitAll {
        ctx: RequestContext,
        reply: oneshot::Sender<Result<(), RuntimeError>>,
    },

    /// Render using a specific theme id.
    Render {
        theme_id: String,
        ctx: RequestContext,
        reply: oneshot::Sender<Result<ResponseBodySpec, RuntimeError>>,
    },

    /// Stop the actor loop.
    Shutdown,
}

/// Client handle for the theme actor.
///
/// This is what your HTTP layer will use to render via a theme.
#[derive(Clone)]
pub struct ThemeRuntimeClient {
    tx: mpsc::UnboundedSender<ThemeCommand>,
}

impl ThemeRuntimeClient {
    /// Spawn the theme actor on its own dedicated thread.
    ///
    /// The `BoundTheme<BoaEngine>` values (and all Boa contexts) live only
    /// on that thread.
    pub fn spawn(themes: Vec<BoundTheme<BoaEngine>>) -> Self {
        let (tx, rx) = mpsc::unbounded_channel::<ThemeCommand>();

        tokio::task::spawn_local(async move {
            theme_actor_loop(themes, rx).await;
        });

        Self { tx }
    }

    /// Initialize all themes with a context (optional, but often useful at boot).
    pub async fn init_all(&self, ctx: RequestContext) -> Result<(), RuntimeError> {
        let (reply_tx, reply_rx) = oneshot::channel();

        self.tx
            .send(ThemeCommand::InitAll {
                ctx,
                reply: reply_tx,
            })
            .map_err(|_| channel_error("theme actor terminated before init_all"))?;

        reply_rx
            .await
            .map_err(|_| channel_error("theme actor dropped init_all reply"))?
    }

    /// Render using a specific theme id.
    pub async fn render(
        &self,
        theme_id: &str,
        ctx: RequestContext,
    ) -> Result<ResponseBodySpec, RuntimeError> {
        let (reply_tx, reply_rx) = oneshot::channel();

        self.tx
            .send(ThemeCommand::Render {
                theme_id: theme_id.to_string(),
                ctx,
                reply: reply_tx,
            })
            .map_err(|_| channel_error("theme actor terminated before render"))?;

        reply_rx
            .await
            .map_err(|_| channel_error("theme actor dropped render reply"))?
    }

    /// Fire-and-forget shutdown signal.
    pub fn stop(&self) {
        let _ = self.tx.send(ThemeCommand::Shutdown);
    }
}

fn channel_error(msg: &str) -> RuntimeError {
    RuntimeError::ThemeBootstrap(msg.to_string())
}

async fn theme_actor_loop(
    themes: Vec<BoundTheme<BoaEngine>>,
    mut rx: mpsc::UnboundedReceiver<ThemeCommand>,
) {
    // Map: theme_id → BoundTheme
    //
    // We keep themes mutable because `BoundTheme::render` takes `&mut self`
    // (engine is mutated while executing JS).
    let mut themes_by_id: HashMap<String, BoundTheme<BoaEngine>> = themes
        .into_iter()
        .map(|t| {
            let id = t.id().to_string();
            (id, t)
        })
        .collect();

    while let Some(cmd) = rx.recv().await {
        match cmd {
            ThemeCommand::InitAll { ctx, reply } => {
                let res = (|| {
                    for theme in themes_by_id.values_mut() {
                        theme.init(&ctx)?;
                    }
                    Ok::<_, RuntimeError>(())
                })();

                let _ = reply.send(res);
            }

            ThemeCommand::Render {
                theme_id,
                ctx,
                reply,
            } => {
                let res = (|| {
                    let theme = themes_by_id.get_mut(&theme_id).ok_or_else(|| {
                        RuntimeError::ThemeBootstrap(format!("unknown theme id: {theme_id}"))
                    })?;

                    theme.render(ctx)
                })();

                let _ = reply.send(res);
            }

            ThemeCommand::Shutdown => {
                break;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use serve::ctx::http::RequestContext;
    use std::collections::HashMap;
    use tokio::task::LocalSet;

    // -------------------------------------------------------------------------
    // Helpers
    // -------------------------------------------------------------------------

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

    // -------------------------------------------------------------------------
    // channel_error tests
    // -------------------------------------------------------------------------

    #[test]
    fn channel_error_wraps_message_in_themebootstrap() {
        let err = channel_error("actor issue");

        match err {
            RuntimeError::ThemeBootstrap(msg) => {
                assert_eq!(msg, "actor issue");
            }
            other => panic!("expected ThemeBootstrap error, got {other:?}"),
        }
    }

    // -------------------------------------------------------------------------
    // ThemeRuntimeClient::init_all tests
    // -------------------------------------------------------------------------

    // No actor running: send fails, should map to channel_error.
    #[tokio::test(flavor = "current_thread")]
    async fn init_all_returns_error_when_actor_not_running() {
        let (tx, rx) = mpsc::unbounded_channel::<ThemeCommand>();
        drop(rx); // simulate actor never spawned / already terminated

        // We can construct client directly because we're in the same module.
        let client = ThemeRuntimeClient { tx };

        let res = client.init_all(dummy_ctx()).await;
        match res {
            Err(RuntimeError::ThemeBootstrap(msg)) => {
                assert!(
                    msg.contains("terminated before init_all"),
                    "expected channel error message, got {msg}"
                );
            }
            Ok(_) => panic!("expected error when actor is not running"),
            Err(other) => panic!("unexpected error variant: {other:?}"),
        }
    }

    // Actor running with an empty theme set: InitAll should succeed (no-op).
    #[tokio::test(flavor = "current_thread")]
    async fn init_all_succeeds_with_empty_theme_set() {
        let local = LocalSet::new();

        local
            .run_until(async {
                let (tx, rx) = mpsc::unbounded_channel::<ThemeCommand>();
                let client = ThemeRuntimeClient { tx: tx.clone() };

                // Spawn the actor loop with an empty Vec<BoundTheme<BoaEngine>>
                tokio::task::spawn_local(theme_actor_loop(Vec::new(), rx));

                let res = client.init_all(dummy_ctx()).await;
                assert!(res.is_ok(), "init_all should succeed with no themes");

                // Clean shutdown
                client.stop();
            })
            .await;
    }

    // -------------------------------------------------------------------------
    // ThemeRuntimeClient::render tests
    // -------------------------------------------------------------------------

    // No actor running: send fails, should map to channel_error.
    #[tokio::test(flavor = "current_thread")]
    async fn render_returns_error_when_actor_not_running() {
        let (tx, rx) = mpsc::unbounded_channel::<ThemeCommand>();
        drop(rx); // simulate actor never spawned / already terminated

        let client = ThemeRuntimeClient { tx };

        let res = client.render("default", dummy_ctx()).await;
        match res {
            Err(RuntimeError::ThemeBootstrap(msg)) => {
                assert!(
                    msg.contains("terminated before render"),
                    "expected channel error message, got {msg}"
                );
            }
            Ok(_) => panic!("expected error when actor is not running"),
            Err(other) => panic!("unexpected error variant: {other:?}"),
        }
    }

    // Actor running, but theme id is unknown → ThemeBootstrap("unknown theme id: ...").
    #[tokio::test(flavor = "current_thread")]
    async fn render_unknown_theme_returns_themebootstrap_error() {
        let local = LocalSet::new();

        local
            .run_until(async {
                let (tx, rx) = mpsc::unbounded_channel::<ThemeCommand>();
                let client = ThemeRuntimeClient { tx: tx.clone() };

                // Actor with no themes available
                tokio::task::spawn_local(theme_actor_loop(Vec::new(), rx));

                let res = client.render("missing-theme", dummy_ctx()).await;
                match res {
                    Err(RuntimeError::ThemeBootstrap(msg)) => {
                        assert!(
                            msg.contains("unknown theme id: missing-theme"),
                            "unexpected error message: {msg}"
                        );
                    }
                    Ok(_) => panic!("expected error for unknown theme id"),
                    Err(other) => panic!("unexpected error variant: {other:?}"),
                }

                client.stop();
            })
            .await;
    }

    // -------------------------------------------------------------------------
    // ThemeRuntimeClient::spawn / stop tests
    // -------------------------------------------------------------------------

    // We can at least verify that spawn works with an empty theme set and that
    // the client methods are usable in that configuration.
    #[tokio::test(flavor = "current_thread")]
    async fn spawn_with_empty_themes_produces_working_client_for_unknown_theme() {
        let local = LocalSet::new();

        local
            .run_until(async {
                // This uses the real spawn, which internally calls spawn_local.
                let client = ThemeRuntimeClient::spawn(Vec::new());

                // init_all should behave as a no-op on empty theme set.
                let init_res = client.init_all(dummy_ctx()).await;
                assert!(
                    init_res.is_ok(),
                    "init_all should succeed on empty theme set"
                );

                // render with unknown theme id should give ThemeBootstrap error.
                let render_res = client.render("nonexistent", dummy_ctx()).await;
                match render_res {
                    Err(RuntimeError::ThemeBootstrap(msg)) => {
                        assert!(
                            msg.contains("unknown theme id: nonexistent"),
                            "unexpected error message: {msg}"
                        );
                    }
                    Ok(_) => panic!("expected error for unknown theme id"),
                    Err(other) => panic!("unexpected error variant: {other:?}"),
                }

                client.stop();
            })
            .await;
    }

    // Ensure stop does not panic and allows actor to finish.
    #[tokio::test(flavor = "current_thread")]
    async fn stop_sends_shutdown_and_actor_exits() {
        let local = LocalSet::new();

        local
            .run_until(async {
                let (tx, rx) = mpsc::unbounded_channel::<ThemeCommand>();
                let client = ThemeRuntimeClient { tx: tx.clone() };

                let handle = tokio::task::spawn_local(theme_actor_loop(Vec::new(), rx));

                // Send a shutdown signal and then drop the client/tx.
                client.stop();
                drop(client);
                drop(tx);

                // Actor should exit cleanly.
                let join_result = handle.await;
                assert!(
                    join_result.is_ok(),
                    "actor task should complete without panic"
                );
            })
            .await;
    }
}
