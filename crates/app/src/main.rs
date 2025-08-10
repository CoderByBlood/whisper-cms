use std::fs::File;

use axum::{middleware::from_fn_with_state, ServiceExt}; // <-- bring the ext trait into scope
use tower::Layer;
use tower_http::normalize_path::NormalizePathLayer;
use tracing_flame::FlameLayer;
use tracing_subscriber::{layer::SubscriberExt, Registry};

mod install;
mod middleware;
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
    if matches!(probed, types::InstallState::Complete) {
        app_state.set_installed(true);
    }

    let routes = router::build(app_state.clone());

    // 1) Maintenance gate (inner)
    let routes = from_fn_with_state(app_state.clone(), middleware::maint::gate).layer(routes);

    // 2) Normalize paths (outermost; trims "/install/" → "/install" BEFORE routing)
    let routes = NormalizePathLayer::trim_trailing_slash().layer(routes);

    // 3) Convert to MakeService (method-style; no turbofish)
    let app = routes.into_make_service();

    let addr = std::net::SocketAddr::from(([127, 0, 0, 1], 8080));
    let listener = tokio::net::TcpListener::bind(addr).await?;
    let local = listener.local_addr()?;
    tracing::info!("listening on http://{local}");

    axum::serve(listener, app).await?;
    Ok(())
}
