use adapt::cmd::Commands;
use clap::Parser;
use std::process::ExitCode;

/// WhisperCMS CLI — Edge Layer
#[derive(Parser, Debug)]
#[command(name = "whispercms", version, about = "WhisperCMS command-line tool")]
pub struct Cli {
    #[command(subcommand)]
    pub command: Commands,
}

#[tokio::main(flavor = "multi_thread")]
pub async fn start() -> ExitCode {
    let cli = Cli::parse();

    let _ctx = match &cli.command {
        Commands::Start(_start) => todo!("Implement start command"),
    };
}
