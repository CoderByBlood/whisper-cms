// crates/app/src/main.rs
use tracing_subscriber::{fmt::Subscriber, EnvFilter};

mod router;
mod install;
mod state;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let _ = Subscriber::builder()
        .with_env_filter(EnvFilter::from_default_env())
        .try_init();

    let state = install::probe::probe()?;
    let app = router::build(state);

    let addr = std::net::SocketAddr::from(([127, 0, 0, 1], 8080));
    let listener = tokio::net::TcpListener::bind(addr).await?;
    let local = listener.local_addr()?;
    tracing::info!("listening on http://{local}");

    axum::serve(listener, app).await?;
    Ok(())
}
