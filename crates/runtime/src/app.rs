use axum::body::Body;
use tower::Layer;
use tower_http::normalize_path::NormalizePathLayer;
use crate::router::build;
use crate::state::RunState;

#[derive(Clone)]
pub struct RunCfg {
    pub addr: std::net::SocketAddr,
    pub data_dir: std::path::PathBuf,
    pub db_url: String,
}

#[tracing::instrument(skip_all)]
pub async fn run(cfg: RunCfg) -> anyhow::Result<()> {
    let state = RunState::new(cfg.data_dir.clone(), cfg.db_url.clone());

    let routes = build(state);
    let routes = NormalizePathLayer::trim_trailing_slash().layer(routes);
    let app = axum::ServiceExt::<axum::http::Request<Body>>::into_make_service(routes); // method style works fine here

    let listener = tokio::net::TcpListener::bind(cfg.addr).await?;
    axum::serve(listener, app).await?;
    Ok(())
}