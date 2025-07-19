mod config;

use std::{
    cell::RefCell, fmt::Debug, net::{AddrParseError, IpAddr}, num::ParseIntError, path::Path, rc::Rc
};

use data_encoding::BASE32_NOPAD;
use secrecy::{ExposeSecret, SecretString};
use sqlx::{postgres::PgPoolOptions, Connection, PgConnection, PgPool};
use thiserror::Error;
use validator::ValidationErrors;

use config::{ConfigError, ConfigurationFile, ValidatedPassword};

#[derive(Debug, Error)]
pub enum StartupError {
    #[error("Password error: {0}")]
    Password(#[from] ValidationErrors),

    #[error("Could not parse port number error: {0}")]
    PortParse(#[from] ParseIntError),

    #[error("Could not parse IP address error: {0}")]
    IpParse(#[from] AddrParseError),

    #[error("Could not load configuration error: {0}")]
    Config(#[from] ConfigError),

    #[error("Could not map configuration error: {0}")]
    Mapping(&'static str),
    //#[error("Database access error: {0}")]
    //Database(#[from] sqlx::Error),
}
#[derive(Debug)]
pub struct Startup {
    password: ValidatedPassword,
    hashed: SecretString,
    port: u16,
    ip: IpAddr,
    filename: String,
}

impl Startup {
    pub fn build(
        password: String,
        salt: String,
        port: u16,
        address: String,
    ) -> Result<Self, StartupError> {
        let ip: IpAddr = address.parse()?;
        let password = ValidatedPassword::build(password, salt)?;
        let hashed = SecretString::new(password.as_hashed().to_string().into_boxed_str());

        let hash = hashed.expose_secret();
        // Step 1: Reverse the string
        let reversed: String = hash.chars().rev().collect();

        // Step 2: Base32 encode the reversed bytes
        let encoded = BASE32_NOPAD.encode(reversed.as_bytes());

        // Step 3: Normalize to lowercase
        let lowercase = encoded.to_lowercase();

        // Step 4: Truncate to 128 characters
        let mut filename: String = lowercase.chars().take(128).collect();

        // Step 5: Add the extension
        filename.push_str(".enc");

        Ok(Self {
            password,
            hashed,
            port,
            ip,
            filename,
        })
    }

    pub fn get_configuration(&self) -> impl DatabaseConfiguration {
        PostgresConfig {
            file: ConfigurationFile::new(self.password.to_owned(), &self.filename),
            conn: None,
        }
    }
}

#[derive(Debug)]
pub enum DatabaseConfigState {
    Missing,
    Exists,
    Valid,
    Failed,
}

pub trait DatabaseConfiguration: Debug {
    fn state(&self) -> DatabaseConfigState;
    fn connect(&self) -> Result<impl DatabaseConnection, StartupError>;
    fn validate(&mut self) -> Result<(), StartupError>;
}

pub trait DatabaseConnection: Debug {
    async fn test_connection(&self) -> Result<bool, ConfigError>;
    fn to_connect_string(&self) -> String;
}
#[derive(Clone, Debug)]
pub struct PostgresConn {
    /// The host for the PostgreSQL server
    host: String,

    /// The port for the PostgreSQL server
    port: u16,

    /// The username for the PostgreSQL database
    user: String,

    /// The password to the PostgreSQL database
    password: String,

    /// The name of the PostgreSQL database
    database: String,
}
#[derive(Debug)]
pub struct PostgresConfig {
    file: ConfigurationFile,
    conn: Option<PostgresConn>,
}

impl DatabaseConnection for PostgresConn {
    async fn test_connection(&self) -> Result<bool, ConfigError> {
        let mut conn = PgConnection::connect(&self.to_connect_string()).await?;

        // Test the connection using ping (available in sqlx 0.8.6)
        conn.ping().await?;
        Ok(true)
    }

    fn to_connect_string(&self) -> String {
        format!(
            "postgresql://{}:{}@{}:{}/{}",
            self.user, self.password, self.host, self.port, self.database
        )
    }
}

impl DatabaseConfiguration for PostgresConfig {
    fn state(&self) -> DatabaseConfigState {
        if !self.file.exists() {
            DatabaseConfigState::Missing
        } else if self.file.tried().is_none() {
            DatabaseConfigState::Exists
        } else if !self.file.tried().unwrap() {
            DatabaseConfigState::Failed
        } else {
            DatabaseConfigState::Valid
        }
    }

    fn validate(&mut self) -> Result<(), StartupError> {
        match self.state() {
            DatabaseConfigState::Missing | DatabaseConfigState::Failed => {
                Err(StartupError::Mapping("Nothing to validate"))
            }
            DatabaseConfigState::Exists => {
                let contents = self
                    .file
                    .load()?;

                let host = contents
                    .get("host")
                    .ok_or(StartupError::Mapping("missing `host` key"))?
                    .to_owned();

                let port = contents
                    .get("port")
                    .ok_or(StartupError::Mapping("missing `port` key"))?
                    .parse()?;

                let user = contents
                    .get("user")
                    .ok_or(StartupError::Mapping("missing `user` key"))?
                    .to_owned();

                let password = contents
                    .get("password")
                    .ok_or(StartupError::Mapping("missing `password` key"))?
                    .to_owned();

                let database = contents
                    .get("database")
                    .ok_or(StartupError::Mapping("missing `database` key"))?
                    .to_owned();

                self.conn = Some(PostgresConn {
                    host,
                    port,
                    user,
                    password,
                    database,
                });

                Ok(())
            }
            DatabaseConfigState::Valid => Ok(()),
        }
    }

    fn connect(&self) -> Result<impl DatabaseConnection, StartupError> {
        match self.state() {
            DatabaseConfigState::Valid => self
                .conn
                .clone()
                .ok_or(StartupError::Mapping("miss matched states")),
            _ => Err(StartupError::Mapping(
                "Could not map configuration due to invalid state",
            )),
        }
    }
}
