use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "WhisperCMS", version, about)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    Install {
        #[arg(short, long, env = "WHISPER_CONFIG_PASSWORD")]
        password: Option<String>,

        #[arg(short, long, env = "WHISPER_CONFIG_FILE")]
        output: Option<String>,

        #[arg(short = 'H', long, env = "PGHOST")]
        pghost: Option<String>,

        #[arg(short = 'P', long, env = "PGPORT")]
        pgport: Option<String>,

        #[arg(short = 'U', long, env = "PGUSER")]
        pguser: Option<String>,

        #[arg(short = 'W', long, env = "PGPASSWORD")]
        pgpassword: Option<String>,

        #[arg(short = 'D', long, env = "PGDATABASE")]
        pgdatabase: Option<String>,
    },
    Start {
        #[arg(short, long, env = "WHISPER_CONFIG_PASSWORD")]
        password: Option<String>,

        #[arg(short, long, env = "WHISPER_CONFIG_FILE")]
        input: Option<String>,
    },
    Rotate {
        #[arg(short, long)]
        old: String,

        #[arg(short, long)]
        new: String,

        #[arg(short, long)]
        config: String,
    },
}
fn main() {
    let cli = Cli::parse();

    match &cli.command {
        Commands::Install {
            password: _,
            output: _,
            pghost: _,
            pgport: _,
            pguser: _,
            pgpassword: _,
            pgdatabase: _,
        } => {
            //
        }
        Commands::Start {
            password: _,
            input: _,
        } => {
            //
        }
        Commands::Rotate {
            old: _,
            new: _,
            config: _,
        } => {}
    }
}
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_install_subcommand_with_all_args() {
        let cli = Cli::parse_from([
            "whispercms",
            "install",
            "--password",
            "supersecret",
            "--output",
            "config.enc",
            "-H",
            "localhost",
            "-P",
            "5432",
            "-U",
            "admin",
            "-W",
            "password123",
            "-D",
            "whisper_db",
        ]);

        match cli.command {
            Commands::Install {
                password,
                output,
                pghost,
                pgport,
                pguser,
                pgpassword,
                pgdatabase,
            } => {
                assert_eq!(password.unwrap(), "supersecret");
                assert_eq!(output.unwrap(), "config.enc");
                assert_eq!(pghost.unwrap(), "localhost");
                assert_eq!(pgport.unwrap(), "5432");
                assert_eq!(pguser.unwrap(), "admin");
                assert_eq!(pgpassword.unwrap(), "password123");
                assert_eq!(pgdatabase.unwrap(), "whisper_db");
            }
            _ => panic!("Expected Install command"),
        }
    }

    #[test]
    fn parse_start_subcommand_with_args() {
        let cli = Cli::parse_from([
            "whispercms",
            "start",
            "--password",
            "startsecret",
            "--input",
            "config.enc",
        ]);

        match cli.command {
            Commands::Start { password, input } => {
                assert_eq!(password.unwrap(), "startsecret");
                assert_eq!(input.unwrap(), "config.enc");
            }
            _ => panic!("Expected Start command"),
        }
    }

    #[test]
    fn parse_rotate_subcommand() {
        let cli = Cli::parse_from([
            "whispercms",
            "rotate",
            "--old",
            "oldpassword",
            "--new",
            "newpassword",
            "--config",
            "new-config.enc",
        ]);

        match cli.command {
            Commands::Rotate { old, new, config } => {
                assert_eq!(old, "oldpassword");
                assert_eq!(new, "newpassword");
                assert_eq!(config, "new-config.enc");
            }
            _ => panic!("Expected Rotate command"),
        }
    }

    #[test]
    fn fails_without_subcommand() {
        let result = Cli::try_parse_from(["whispercms"]);
        assert!(result.is_err());
    }
}
