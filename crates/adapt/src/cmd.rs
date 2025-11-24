use clap::{builder::ValueHint, Parser, Subcommand};
use std::path::PathBuf;

use serve::ctx::http::ContextError;

type _Result<T> = std::result::Result<T, ContextError>;

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
