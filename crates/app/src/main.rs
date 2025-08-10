use std::fs::File;

use axum::body::Body;
use tower::Layer;
use tower_http::normalize_path::NormalizePathLayer;
use tracing_flame::FlameLayer;
use tracing_subscriber::{layer::SubscriberExt, Registry};

use crate::phase::Phase;

mod install;
mod middleware;
mod phase;
mod router;
mod state;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Setup flamegraph output
    let flame_file = File::create("flame.folded").expect("failed to create file");
    let flame_layer = FlameLayer::new(flame_file);

    // Print to stdout for dev logs
    let fmt_layer = tracing_subscriber::fmt::layer()
        .with_target(false)
        .with_thread_ids(true);

    // Combine layers
    let subscriber = Registry::default().with(fmt_layer).with(flame_layer);

    tracing::subscriber::set_global_default(subscriber).expect("set global subscriber");

    /*let _ = Subscriber::builder()
    .with_env_filter(EnvFilter::from_default_env())
    .try_init();*/

    let probed = install::probe::probe()?;
    let app_state = state::AppState::default();
    match probed {
        types::InstallState::Complete => {
            app_state
                .phase
                .transition_to(&app_state, Phase::Serve)
                .await?;
        }
        _ => {
            app_state
                .phase
                .transition_to(&app_state, Phase::Install)
                .await?;
        }
    }

    let routes = router::build(app_state.clone());
    let routes = NormalizePathLayer::trim_trailing_slash().layer(routes);
    let app = axum::ServiceExt::<axum::http::Request<Body>>::into_make_service(routes);

    let addr = std::net::SocketAddr::from(([127, 0, 0, 1], 8080));
    let listener = tokio::net::TcpListener::bind(addr).await?;
    let local = listener.local_addr()?;
    tracing::info!("listening on http://{local}");

    axum::serve(listener, app).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use axum::{body::Body, http::Request};
    use tower::{Layer, ServiceExt as _}; // oneshot
    use tower_http::normalize_path::NormalizePathLayer;

    #[tokio::test]
    async fn install_trailing_slash_normalizes() {
        // Build the service under test
        let app_state = crate::state::AppState::default();
        let svc = crate::router::build(app_state);
        let svc = NormalizePathLayer::trim_trailing_slash().layer(svc);

        // Drive it with two requests and compare the responses
        let resp_a = svc
            .clone()
            .oneshot(Request::get("/install").body(Body::empty()).unwrap())
            .await
            .unwrap();

        let resp_b = svc
            .clone()
            .oneshot(Request::get("/install/").body(Body::empty()).unwrap())
            .await
            .unwrap();

        assert_eq!(resp_a.status(), resp_b.status());
    }
}
