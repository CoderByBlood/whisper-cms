mod cli;
mod request;
mod startup;

use std::{collections::HashMap, fs::File};

use clap::Parser;
use tracing::debug;
use tracing_flame::FlameLayer;
use tracing_subscriber::{self, layer::SubscriberExt, Registry};

use cli::Args;

use request::Manager;
use startup::{Startup, StartupError};

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

    debug!("BEGIN main");
    let args = Args::parse();
    debug!("Ags parsed");
    let startup = Startup::build(args.password, args.salt)?;
    debug!("Startup process built");
    let mut req_mgr = Manager::build(startup)?;
    debug!("Request Manager built and booting");
    req_mgr.boot(args.address, args.port).await?;

    let _map: HashMap<String, String> = HashMap::from([
        ("host".into(), "localhost".into()),
        ("port".into(), "5432".into()),
        ("user".into(), "myuser".into()),
        ("password".into(), "mypassword".into()),
        ("database".into(), "mydatabase".into()),
        ("pool".into(), "15".into()),
    ]);

    Ok(())
}
