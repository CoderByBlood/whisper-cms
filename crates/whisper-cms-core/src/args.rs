use clap:: {
    //Args,
    Parser,
    Subcommand
};

#[derive(Debug, Parser)]
#[clap(author, version, about)]
pub struct WhisperCmsArgs {
    #[command(subcommand)]
    pub command: Commands,
    
}

#[derive(Debug, Subcommand)]
pub enum Commands {
    /// Install WhisperCMS and configure PostgreSQL database
    Install {
        /// The password to use to encrypt the config file
        #[arg(short, long, env = "WHISPER_CONFIG_PASSWORD")]
        password: Option<String>,

        /// The encrypted config file to create
        #[arg(short, long, env = "WHISPER_CONFIG_FILE")]
        output: Option<String>,

        /// The host for the PostgreSQL server
        #[arg(short = 'H', long, env = "PGHOST")]
        pghost: Option<String>,

        /// The port for the PostgreSQL server
        #[arg(short = 'P', long, env = "PGPORT")]
        pgport: Option<u16>,

        /// The username for the PostgreSQL database
        #[arg(short = 'U', long, env = "PGUSER")]
        pguser: Option<String>,

        /// The password to the PostgreSQL database
        #[arg(short = 'W', long, env = "PGPASSWORD")]
        pgpassword: Option<String>,

        /// The name of the PostgreSQL database
        #[arg(short = 'D', long, env = "PGDATABASE")]
        pgdatabase: Option<String>,
    },

    /// Start WhisperCMS using stored and encrypted configuration
    Start {
        /// The password to use to decrypt the config file
        #[arg(short, long, env = "WHISPER_CONFIG_PASSWORD")]
        password: Option<String>,

        /// The encrypted config file to decrypt
        #[arg(short, long, env = "WHISPER_CONFIG_FILE")]
        input: Option<String>,
    },

    /// Rotate the password for the encrypted configuration
    Rotate {
        /// The previous password to use to decrypt the config file
        #[arg(short, long)]
        old: Option<String>,

        /// The updated password to use to encrypt the config file
        #[arg(short, long)]
        new: Option<String>,

        /// The encrypted config file to reencrypt with the new password
        #[arg(short, long)]
        config: Option<String>,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_install_subcommand_with_all_args() {
        let cli = WhisperCmsArgs::parse_from([
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
                assert_eq!(pgport.unwrap(), 5432);
                assert_eq!(pguser.unwrap(), "admin");
                assert_eq!(pgpassword.unwrap(), "password123");
                assert_eq!(pgdatabase.unwrap(), "whisper_db");
            }
            _ => panic!("Expected Install command"),
        }
    }

    #[test]
    fn parse_start_subcommand_with_args() {
        let cli = WhisperCmsArgs::parse_from([
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
        let cli = WhisperCmsArgs::parse_from([
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
                assert_eq!(old.unwrap(), "oldpassword");
                assert_eq!(new.unwrap(), "newpassword");
                assert_eq!(config.unwrap(), "new-config.enc");
            }
            _ => panic!("Expected Rotate command"),
        }
    }

    #[test]
    fn fails_without_subcommand() {
        let result = WhisperCmsArgs::try_parse_from(["whispercms"]);
        assert!(result.is_err());
    }
}
