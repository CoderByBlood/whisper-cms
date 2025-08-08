pub mod actors;

use std::fs::File;

use clap::Parser;
use ractor::{Actor, RpcReplyPort};
use tokio::sync::oneshot;
use tracing::debug;
use tracing_flame::FlameLayer;
use tracing_subscriber::{layer::SubscriberExt, Registry};

use crate::actors::request::{Request, RequestArgs, RequestEnvelope};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
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
    let args = CliArgs::parse();
    debug!("Ags parsed");

    let (actor, handle) = Actor::spawn(None, Request, RequestArgs { args }).await?;

    let (tx, rx) = oneshot::channel();
    let envelope = RequestEnvelope::Start {
        reply: RpcReplyPort::from(tx),
    };

    actor.send_message(envelope)?;
    let reply = rx.await??;

    debug!("Request Manager built and booted and replied with {reply:?}");

    // 5. Shut down actor cleanly
    //actor.stop(None);
    let _ = handle.await;

    let _map: std::collections::HashMap<String, String> = std::collections::HashMap::from([
        ("host".into(), "localhost".into()),
        ("port".into(), "5432".into()),
        ("user".into(), "myuser".into()),
        ("password".into(), "mypassword".into()),
        ("database".into(), "mydatabase".into()),
        ("pool".into(), "15".into()),
    ]);

    Ok(())
}

/// WhisperCMS
#[derive(Parser, Clone)]
#[command(version, about, long_about = None)]
pub struct CliArgs {
    /// Password to settings
    #[arg(short, long)]
    pub password: String,

    /// Salt to use for hashing the password
    #[arg(short, long, default_value = "6Jq@bXv9LpT!r3Uz")]
    pub salt: String,

    /// Port to bind
    #[arg(short = 't', long, default_value_t = 8080)]
    pub port: u16,

    /// Address to bind
    #[arg(short = 'i', long, default_value = "0.0.0.0")]
    pub address: String,
}

/// Prevent secret leakage through `Debug`
impl core::fmt::Debug for CliArgs {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "Args(**REDACTED**)")
    }
}
