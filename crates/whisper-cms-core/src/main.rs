mod cli;
mod startup;

use clap::Parser;
use tracing::debug; //, error, info, trace, trace_span, warn};
use tracing_subscriber;

use cli::Args;

use crate::startup::{Configuration, DatabaseConfig, Startup, StartupError};

#[tokio::main]
async fn main() -> Result<(), StartupError>{
    tracing_subscriber::fmt::init();

    let args = Args::parse();
    let startup = Startup::build(args.password, args.salt, args.port, args.address)?;
    let config = startup.get_configuration()?;

    return match config {
        None => Ok(()),
        Some(config) => {
            config.test_connection().await?;
            Ok(())
        },
    }
}
