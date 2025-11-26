use std::process::ExitCode;

use tracing::info;
use tracing_subscriber::{fmt, prelude::*, EnvFilter};

pub mod cli;
pub mod cmd;
pub mod db;
pub mod fs;
pub mod proxy;
pub mod router;

fn main() -> ExitCode {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("warn")); // fallback

    tracing_subscriber::registry()
        .with(filter)
        .with(fmt::layer().with_file(true).with_line_number(true))
        .init();

    info!("logging setup");
    info!("engaging clap to parse commandline");
    cli::start()
}
