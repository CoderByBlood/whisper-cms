use std::fs::File;

use axum::{body::Body, middleware::from_fn, ServiceExt as _}; // <-- bring the ext trait into scope
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

    // 1) Anything first (even a no-op) locks in Body type
    let routes = from_fn(pass_through).layer(routes);

    // 2) Then normalize (runs first at runtime)
    let routes = NormalizePathLayer::trim_trailing_slash().layer(routes);

    let app = routes.into_make_service();

    let addr = std::net::SocketAddr::from(([127, 0, 0, 1], 8080));
    let listener = tokio::net::TcpListener::bind(addr).await?;
    let local = listener.local_addr()?;
    tracing::info!("listening on http://{local}");

    axum::serve(listener, app).await?;
    Ok(())
}

async fn pass_through(req: axum::http::Request<Body>, next: axum::middleware::Next) 
    -> axum::response::Response 
{ 
    next.run(req).await 
}
