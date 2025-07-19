mod cli;
mod startup;

use clap::Parser;
use tracing::debug; //, error, info, trace, trace_span, warn};
use tracing_subscriber;

use cli::Args;

use startup::{Startup, StartupError, DatabaseConfiguration, DatabaseConnection, DatabaseConfigState};

#[tokio::main]
async fn main() -> Result<(), StartupError>{
    tracing_subscriber::fmt::init();

    let args = Args::parse();
    let startup = Startup::build(args.password, args.salt, args.port, args.address)?;
    let mut config = startup.get_configuration();
    dbg!(&config);
    dbg!(config.state());
    dbg!(config.validate()?);
    dbg!(config.state());
    dbg!(config.connect()?);
    dbg!(config.state());
    dbg!(config.connect()?.to_connect_string());
    dbg!(config.state());
    dbg!(config.connect()?.test_connection().await?);
    Ok(())
}
