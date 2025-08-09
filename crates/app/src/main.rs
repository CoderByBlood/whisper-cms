
use tracing_subscriber::{EnvFilter, fmt::Subscriber};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let _ = Subscriber::builder()
        .with_env_filter(EnvFilter::from_default_env())
        .try_init();
    println!("whispercms oneword skeleton build OK");
    Ok(())
}
