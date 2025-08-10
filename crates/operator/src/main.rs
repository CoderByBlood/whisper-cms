//! whisperctl (operator): CLI + GUI installer entrypoint.

use std::{fs::File, net::SocketAddr, path::PathBuf};

use anyhow::Context;
use axum::body::Body; // NOTE: axum::ServiceExt for into_make_service
use clap::{Parser, Subcommand};
use domain::config::core::CoreConfig;
use tower::Layer;
use tower_http::normalize_path::NormalizePathLayer;
use tracing_flame::FlameLayer;
use tracing_subscriber::{layer::SubscriberExt, Registry};

mod actions; // install actions (post_config/post_run/…)
mod gui; // dispatcher + .fallback(dispatch)
mod phase; // Phase enum + PhaseState (no dyn)
mod plan;
mod progress;
mod routes; // install routes (config/run/done/…)
mod state;
mod steps; // OperState (installer state)

/// CLI entrypoint
#[derive(Parser)]
#[command(
    name = "whisperctl",
    version,
    about = "WhisperCMS operator (CLI + GUI installer)"
)]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Launch the GUI installer
    Gui {
        /// Bind address, e.g. 127.0.0.1:8080
        #[arg(long, default_value = "127.0.0.1:8080")]
        bind: String,

        /// Site root directory (configs, data/, content/, etc.)
        #[arg(long, default_value = ".")]
        site: PathBuf,
    },

    /// (stub) Headless init — will be wired later
    Init {
        #[arg(long, default_value = "./mysite")]
        path: PathBuf,
    },

    /// (stub) Dev runner — will try cargo-watch later
    ServeDev {
        #[arg(long, default_value = "./mysite")]
        path: PathBuf,
    },
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // minimal, practical tracing setup
    /* let fmt_layer = fmt::layer().with_target(false);
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("debug"));
    tracing_subscriber::registry()
        .with(filter)
        .with(fmt_layer)
        .init(); */

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

    let cli = Cli::parse();
    match cli.cmd {
        Cmd::Gui { bind, site } => run_gui(bind, site).await,
        Cmd::Init { path } => {
            tracing::info!("(stub) headless init at {:?}", path);
            Ok(())
        }
        Cmd::ServeDev { path } => {
            tracing::info!("(stub) serve-dev for {:?}", path);
            Ok(())
        }
    }
}

/// Start the GUI installer using the known-good layering order:
/// 1) add any middleware (locks Body type for inference),
/// 2) add NormalizePath as the OUTERMOST layer,
/// 3) convert to MakeService (trait-qualified form that compiles on Axum 0.8),
/// 4) axum::serve(listener, app).
#[tracing::instrument(skip_all)]
async fn run_gui(bind: String, site: PathBuf) -> anyhow::Result<()> {
    // Make site dir visible to infra (paths resolve under this root).
    std::env::set_var("WHISPERCMS_SITE_DIR", &site);

    // Build installer state
    let app = state::OperState::new();

    // Initial phase based on installed flag (simple probe)
    if probe_installed().unwrap_or(false) {
        app.phase.transition_to(&app, phase::Phase::Serve).await?;
    } else {
        app.phase.transition_to(&app, phase::Phase::Install).await?;
    }

    // Build the dispatcher router for the current phase
    let routes = gui::build(app.clone());

    // Outermost: normalize paths BEFORE routing so "/install/" == "/install"
    let routes = NormalizePathLayer::trim_trailing_slash().layer(routes);

    // Convert Service<Request<Body>> -> MakeService (Axum 0.8 trait-qualified form)
    let app = axum::ServiceExt::<axum::http::Request<Body>>::into_make_service(routes);

    // Bind + serve
    let addr: SocketAddr = bind.parse().context("invalid --bind address")?;
    let listener = tokio::net::TcpListener::bind(addr).await?;
    tracing::info!(
        "whisperctl GUI listening on http://{}",
        listener.local_addr()?
    );
    axum::serve(listener, app).await?;
    Ok(())
}

/// Minimal probe: if core.toml exists and `installed = true`, we treat as already installed.
/// Falls back to false on any error.
#[tracing::instrument(skip_all)]
fn probe_installed() -> anyhow::Result<bool> {
    // Resolve site-scoped path (honors WHISPERCMS_SITE_DIR if you set it earlier)
    let core_path = infra::config::paths::core_toml();

    // Try to read + parse CoreConfig; missing/any error => false
    let installed = infra::config::io::read_toml_opt::<_, CoreConfig>(&core_path)
        .ok() // swallow IO/parse errors -> treat as not installed
        .flatten() // None if file missing
        .map(|cfg| cfg.installed)
        .unwrap_or(false);

    Ok(installed)
}
