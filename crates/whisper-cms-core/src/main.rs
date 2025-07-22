mod cli;
mod startup;
mod request;

use std::{collections::HashMap, fs::File};

use clap::Parser;
use tracing::debug;
use tracing_flame::FlameLayer;
use tracing_subscriber::{self, layer::SubscriberExt, Registry};

use cli::Args;

use startup::{Startup, StartupError};
use request::Manager;

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
    let mut startup = Startup::build(args.password, args.salt)?;
    match &startup.execute() {
        Ok(_) => debug!("Successfully Executed: {:?}", startup.checkpoint()),
        Err(e) => debug!("Failed At: {:?}: {}",startup.checkpoint() ,e),
    }

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
