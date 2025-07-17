use clap::Parser;

/// WhisperCMS
#[derive(Parser, Debug)]
#[command(version, about, long_about = None)]
pub struct Args {
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
