// Re-export the modules the tests (and main) need.
pub mod actions;
pub mod auth;
pub mod forms;
pub mod phase;
pub mod progress;
pub mod routes;
pub mod state;
pub mod steps;

/// Build the top-level app router that dispatches into the current Phase router.
pub fn app_router(
    app: state::OperState,
) -> tower_http::normalize_path::NormalizePath<axum::Router> {
    let dispatch = axum::Router::new()
        .fallback(
            |axum::extract::State(app): axum::extract::State<state::OperState>,
             req: axum::http::Request<axum::body::Body>| async move {
                // always dispatch via PhaseState
                app.phase.dispatch(app.clone(), req).await
            },
        )
        .with_state(app.clone())
        // ⬇️ mTLS gate wraps EVERYTHING (Boot/Install/Serve)
        .layer(axum::middleware::from_fn_with_state(
            app.clone(),
            auth::gate,
        ));

    // Normalize path BEFORE dispatch so /install/ == /install
    tower::Layer::layer(
        &tower_http::normalize_path::NormalizePathLayer::trim_trailing_slash(),
        dispatch,
    )
}
