mod args;
mod settings;
mod ui;

use args::WhisperCmsArgs;
use clap::Parser;

use crate::settings::Settings;

fn main() {
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
                        Ok(()) => println!("{:?} was successfully encrypted", settings.output),
                        Err(err) => eprintln!("{:?}", err),
                    }
                }
                Ok(ui::Commands::Start { .. }) => eprintln!("Start is a bad command"),
                Ok(ui::Commands::Rotate { .. }) => eprintln!("Rotate is a bad command"),
                Err(err) => eprintln!("{:?}", err),
            }
        }
        args::Commands::Start { password, input } => match ui::prompt_for_start(password, input) {
            Ok(ui::Commands::Start { password, input }) => {
                match Settings::read_encrypted(&password, &input) {
                    Ok(settings) => println!("{:?} was successfully decrypted", settings.output),
                    Err(err) => eprintln!("{:?}", err),
                }
            }
            Ok(ui::Commands::Install { .. }) => eprintln!("Install is a bad command"),
            Ok(ui::Commands::Rotate { .. }) => eprintln!("Rotate is a bad command"),
            Err(err) => eprintln!("{:?}", err),
        },
        args::Commands::Rotate { old, new, config } => {
            match ui::prompt_for_rotate(old, new, config) {
                Ok(ui::Commands::Rotate { old, new, config }) => {
                    match Settings::read_encrypted(&old, &config) {
                        Ok(settings) => {
                            println!("{:?} was successfully decrypted", settings.output);
                            let mut settings = settings;
                            settings.output = config;

                            match settings.write_encrypted(&new) {
                                Ok(()) => {
                                    println!("{:?} was successfully encrypted", settings.output)
                                }
                                Err(err) => eprintln!("{:?}", err),
                            }
                        }
                        Err(err) => eprintln!("{:?}", err),
                    }
                }
                Ok(ui::Commands::Install { .. }) => eprintln!("Install is a bad command"),
                Ok(ui::Commands::Start { .. }) => eprintln!("Start is a bad command"),
                Err(err) => eprintln!("{:?}", err),
            }
        }
    }
}
