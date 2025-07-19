mod cli;
mod startup;

use std::collections::HashMap;

use clap::Parser;
use tracing::debug; //, error, info, trace, trace_span, warn};
use tracing_subscriber;

use cli::Args;

use startup::{
    DatabaseConfigState, DatabaseConfiguration, DatabaseConnection, Startup, StartupError,
};

#[tokio::main]
async fn main() -> Result<(), StartupError> {
    tracing_subscriber::fmt::init();

    let args = Args::parse();
    let startup = Startup::build(args.password, args.salt, args.port, args.address)?;
    let mut config = startup.get_configuration();
    let map = HashMap::from([
        ("host".to_owned(), "localhost".to_owned()),
        ("port".to_owned(), "5432".to_owned()),
        ("user".to_owned(), "myuser".to_owned()),
        ("password".to_owned(), "mypassword".to_owned()),
        ("database".to_owned(), "mydatabase".to_owned()),
        ("pool".to_owned(), "15".to_owned()),
    ]);

    dbg!(&config);
    dbg!(config.state());

    match config.validate() {
        Err(_) => {
            dbg!(&map);
            config.save(map)
        },
        ok @ Ok(_) => ok,
    }?;

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
