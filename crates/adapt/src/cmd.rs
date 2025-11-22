use clap::{builder::ValueHint, Parser, Subcommand};
use std::path::PathBuf;
use std::{future::Future, pin::Pin, process::ExitCode};
use tower::Service;

use crate::core::CoreError;

type Result<T> = std::result::Result<T, CoreError>;

/// Unified request passed into Tower pipeline
pub struct CliReq {
    pub cmd: Commands,
}

#[derive(Subcommand, Debug)]
pub enum Commands {
    /// Start WhisperCMS using the specified directory
    Start(StartCmd),
}

#[derive(Parser, Debug)]
pub struct StartCmd {
    /// Target directory (or set WHISPERCMS_DIR)
    ///
    /// Must exist, be a directory, and be readable & writable.
    #[arg(
        value_name = "DIR",
        env = "WHISPERCMS_DIR",
        required = true,
        value_hint = ValueHint::DirPath,
        value_parser = dir_must_exist
    )]
    pub dir: PathBuf,
}

fn dir_must_exist(s: &str) -> std::result::Result<PathBuf, String> {
    let p = PathBuf::from(s);
    if !p.exists() {
        return Err(format!("Not found: {}", p.display()));
    }
    if !p.is_dir() {
        return Err(format!("Not a directory: {}", p.display()));
    }
    Ok(p)
}

/// The Dispatcher — maps commands to business logic
pub struct Dispatcher;

impl Service<CliReq> for Dispatcher {
    type Response = ExitCode;
    type Error = CoreError;
    type Future = Pin<Box<dyn Future<Output = Result<ExitCode>> + Send>>;

    fn poll_ready(&mut self, _cx: &mut std::task::Context<'_>) -> std::task::Poll<Result<()>> {
        std::task::Poll::Ready(Ok(()))
    }

    fn call(&mut self, req: CliReq) -> Self::Future {
        Box::pin(async move {
            match req.cmd {
                Commands::Start(_cmd) => todo!("Implement Start Command"),
            }
        })
    }
}
