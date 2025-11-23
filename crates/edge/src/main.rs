use std::process::ExitCode;

pub mod cli;
pub mod db;
pub mod fs;
pub mod proxy;
pub mod router;

fn main() -> ExitCode {
    println!("Hello, world!");
    cli::start()
}
