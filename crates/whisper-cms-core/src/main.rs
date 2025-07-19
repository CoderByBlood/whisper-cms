mod cli;
mod startup;

use clap::Parser;
use tracing::debug; //, error, info, trace, trace_span, warn};
use tracing_subscriber;

use cli::Args;

use startup::{Startup, StartupError, DatabaseConfiguration, DatabaseConnection};

#[tokio::main]
async fn main() -> Result<(), StartupError>{
    tracing_subscriber::fmt::init();

    let args = Args::parse();
    let startup = Startup::build(args.password, args.salt, args.port, args.address)?;
    let config = startup.get_configuration();
    //dbg!(config.borrow());
    dbg!(config.borrow().state());
    dbg!(config.borrow_mut().validate()?);
    dbg!(config.borrow().state());
    //dbg!(config.borrow().connect());
    dbg!(config.borrow().state());
    dbg!(config.borrow().connect()?.to_connect_string());
    dbg!(config.borrow().state());
    dbg!(config.borrow().connect()?.test_connection().await?);
    Ok(())
}
