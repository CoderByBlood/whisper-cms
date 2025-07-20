mod cli;
mod startup;

use std::{collections::HashMap, fs::File};

use clap::Parser;
use tracing::debug;
use tracing_flame::FlameLayer;
use tracing_subscriber::{self, layer::SubscriberExt, Registry};

use cli::Args;

use startup::{DatabaseConfiguration, DatabaseConnection, Startup, StartupError};

#[tokio::main]
async fn main() -> Result<(), StartupError> {
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

    let args = Args::parse();
    let startup = Startup::build(args.password, args.salt, args.port, args.address)?;
    let mut config = startup.get_configuration();
    let map = HashMap::from([
        ("host".into(), "localhost".into()),
        ("port".into(), "5432".into()),
        ("user".into(), "myuser".into()),
        ("password".into(), "mypassword".into()),
        ("database".into(), "mydatabase".into()),
        ("pool".into(), "15".into()),
    ]);

    debug!("{:?}", &config);
    debug!("{:?}", config.state());

    match config.validate() {
        Err(_) => {
            dbg!(&map);
            config.save(map)
        }
        ok @ Ok(_) => ok,
    }?;

    debug!("{:?}", config.state());
    debug!("{:?}", config.validate()?);
    debug!("{:?}", config.state());
    debug!("{:?}", config.connect()?);
    debug!("{:?}", config.state());
    debug!("{:?}", config.connect()?.to_connect_string());
    debug!("{:?}", config.state());
    debug!("{:?}", config.connect()?.test_connection().await?);
    Ok(())
}
