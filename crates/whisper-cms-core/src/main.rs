mod args;
mod settings;
mod ui;

use tracing::{info, warn, error};
use tracing_subscriber;

use args::WhisperCmsArgs;
use clap::Parser;

use crate::settings::Settings;

fn main() {
    tracing_subscriber::fmt::init();
    let args = WhisperCmsArgs::parse();

    match &args.command {
        args::Commands::Install {
            password,
            output,
            pghost,
            pgport,
            pguser,
            pgpassword,
            pgdatabase,
        } => {
            match ui::prompt_for_install(
                password, output, pghost, pgport, pguser, pgpassword, pgdatabase,
            ) {
                Ok(ui::Commands::Install {
                    password,
                    output,
                    pghost,
                    pgport,
                    pguser,
                    pgpassword,
                    pgdatabase,
                }) => {
                    let settings = Settings {
                        output: output,
                        pghost: pghost,
                        pgport: pgport,
                        pguser: pguser,
                        pgpassword: pgpassword,
                        pgdatabase: pgdatabase,
                    };

                    match settings.write_encrypted(&password) {
                        Ok(()) => info!("{:?} was successfully encrypted", settings.output),
                        Err(err) => error!("{:?}", err),
                    }
                }
                Ok(ui::Commands::Start { .. }) => warn!("Start is a bad command"),
                Ok(ui::Commands::Rotate { .. }) => warn!("Rotate is a bad command"),
                Err(err) => error!("{:?}", err),
            }
        }
        args::Commands::Start { password, input } => match ui::prompt_for_start(password, input) {
            Ok(ui::Commands::Start { password, input }) => {
                match Settings::read_encrypted(&password, &input) {
                    Ok(settings) => info!("{:?} was successfully decrypted", settings.output),
                    Err(err) => error!("{:?}", err),
                }
            }
            Ok(ui::Commands::Install { .. }) => warn!("Install is a bad command"),
            Ok(ui::Commands::Rotate { .. }) => warn!("Rotate is a bad command"),
            Err(err) => error!("{:?}", err),
        },
        args::Commands::Rotate { old, new, config } => {
            match ui::prompt_for_rotate(old, new, config) {
                Ok(ui::Commands::Rotate { old, new, config }) => {
                    match Settings::read_encrypted(&old, &config) {
                        Ok(settings) => {
                            info!("{:?} was successfully decrypted", settings.output);
                            let mut settings = settings;
                            settings.output = config;

                            match settings.write_encrypted(&new) {
                                Ok(()) => {
                                    info!("{:?} was successfully encrypted", settings.output)
                                }
                                Err(err) => error!("{:?}", err),
                            }
                        }
                        Err(err) => error!("{:?}", err),
                    }
                }
                Ok(ui::Commands::Install { .. }) => warn!("Install is a bad command"),
                Ok(ui::Commands::Start { .. }) => warn!("Start is a bad command"),
                Err(err) => error!("{:?}", err),
            }
        }
    }
}
