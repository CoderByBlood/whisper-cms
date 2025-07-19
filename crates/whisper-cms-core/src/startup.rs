mod config;

use std::{
    collections::HashMap,
    fmt::Debug,
    net::{AddrParseError, IpAddr},
    num::ParseIntError,
};

use data_encoding::BASE32_NOPAD;
use secrecy::{ExposeSecret, SecretString};
use sqlx::{postgres::PgPoolOptions, Connection, PgConnection, PgPool};
use thiserror::Error;
use validator::ValidationErrors;

use config::{ConfigError, ConfigurationFile, ValidatedPassword};

use crate::startup::config::ConfigMap;

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
        let hashed = SecretString::from(password.as_hashed());

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

#[derive(Debug, PartialEq)]
pub enum DatabaseConfigState {
    Missing,
    Exists,
    Valid,
    Failed,
}

pub trait DatabaseConfiguration: Debug {
    fn state(&self) -> DatabaseConfigState;
    fn save(&mut self, confg: HashMap<String, String>) -> Result<(), StartupError>;
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
    password: SecretString,

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
        let conn_str = self.to_connect_string().replace(
            &format!("{:?}", self.password),
            self.password.expose_secret(),
        );

        // Test the connection using ping (available in sqlx 0.8.6)
        PgConnection::connect(&conn_str).await?.ping().await?;
        Ok(true)
    }

    fn to_connect_string(&self) -> String {
        format!(
            "postgresql://{}:{:?}@{}:{}/{}",
            self.user, self.password, self.host, self.port, self.database
        )
    }
}

impl PostgresConfig {
    fn build_connection(contents: &ConfigMap) -> Result<PostgresConn, StartupError> {
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

        Ok(PostgresConn {
            host,
            port,
            user,
            password: SecretString::from(password),
            database,
        })
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
        } else if self.conn.is_none() {
            //someone called save on the File directly
            DatabaseConfigState::Failed
        } else {
            DatabaseConfigState::Valid
        }
    }

    fn save(&mut self, config: HashMap<String, String>) -> Result<(), StartupError> {
        self.conn = Some(Self::build_connection(&config)?);
        self.file.save(config)?;
        Ok(())
    }

    fn validate(&mut self) -> Result<(), StartupError> {
        match self.state() {
            DatabaseConfigState::Missing | DatabaseConfigState::Failed => {
                Err(StartupError::Mapping("Nothing to validate"))
            }
            DatabaseConfigState::Exists => {
                self.conn = Some(Self::build_connection(&self.file.load()?)?);
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
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_startup_build_filename_generation() {
        let startup = Startup::build(
            "StrongPass1$".to_string(),
            "longsufficientlysalt".to_string(),
            5432,
            "127.0.0.1".to_string(),
        )
        .unwrap();

        assert!(startup.filename.ends_with(".enc"));
        assert!(startup.filename.len() <= 128 + 4);
    }

    #[test]
    fn test_startup_invalid_ip_fails() {
        let result = Startup::build(
            "StrongPass1$".to_string(),
            "longsufficientlysalt".to_string(),
            5432,
            "invalid_ip".to_string(),
        );
        assert!(matches!(result, Err(StartupError::IpParse(_))));
    }

    #[test]
    fn test_startup_invalid_password_fails() {
        let result = Startup::build(
            "weak".to_string(),
            "saltislongenough".to_string(),
            5432,
            "127.0.0.1".to_string(),
        );
        assert!(matches!(result, Err(StartupError::Password(_))));
    }

    #[test]
    fn test_postgres_config_state_transitions() {
        let password = ValidatedPassword::build(
            "StrongPass1$".to_string(),
            "longsufficientlysalt".to_string(),
        )
        .unwrap();

        let tmp_dir = tempfile::tempdir().unwrap();
        let path = tmp_dir.path().join("pg_config.enc");
        let path_str = path.to_str().unwrap();

        let config = PostgresConfig {
            file: ConfigurationFile::new(password, path_str),
            conn: None,
        };

        // File does not exist
        assert_eq!(config.state(), DatabaseConfigState::Missing);
    }

    #[tokio::test]
    async fn test_to_connect_string_format() {
        let conn = PostgresConn {
            host: "localhost".to_string(),
            port: 5432,
            user: "admin".to_string(),
            password: SecretString::from("supersecret"),
            database: "mydb".to_string(),
        };

        let conn_str = conn.to_connect_string();
        assert!(conn_str.contains("admin"));
        assert!(conn_str.contains("localhost"));
        assert!(conn_str.contains("5432"));
        assert!(conn_str.contains("mydb"));
    }

    #[test]
    fn test_validate_missing_file() {
        let password = ValidatedPassword::build(
            "StrongPass1$".to_string(),
            "longsufficientlysalt".to_string(),
        )
        .unwrap();

        let tmp_dir = tempfile::tempdir().unwrap();
        let path = tmp_dir.path().join("missing_file.enc");
        let path_str = path.to_str().unwrap();

        let mut config = PostgresConfig {
            file: ConfigurationFile::new(password, path_str),
            conn: None,
        };

        let result = config.validate();
        assert!(matches!(result, Err(StartupError::Mapping(_))));
    }

    #[test]
    fn test_validate_missing_keys() {
        let password = ValidatedPassword::build(
            "StrongPass1$".to_string(),
            "longsufficientlysalt".to_string(),
        )
        .unwrap();

        let tmp_dir = tempfile::tempdir().unwrap();
        let path = tmp_dir.path().join("incomplete.enc");
        let path_str = path.to_str().unwrap();

        let mut config = PostgresConfig {
            file: ConfigurationFile::new(password.clone(), path_str),
            conn: None,
        };

        let mut map = HashMap::new();
        map.insert("host".to_string(), "localhost".to_string());
        config.file.save(map).unwrap();

        let result = config.validate();
        assert!(matches!(result, Err(StartupError::Mapping(_))));
    }

    #[test]
    fn test_validate_and_connect_successful_path() {
        let password = ValidatedPassword::build(
            "StrongPass1$".to_string(),
            "longsufficientlysalt".to_string(),
        )
        .unwrap();

        let tmp_dir = tempfile::tempdir().unwrap();
        let path = tmp_dir.path().join("valid.enc");
        let path_str = path.to_str().unwrap();

        let mut config = PostgresConfig {
            file: ConfigurationFile::new(password, path_str),
            conn: None,
        };

        let mut map = HashMap::new();
        map.insert("host".to_string(), "localhost".to_string());
        map.insert("port".to_string(), "5432".to_string());
        map.insert("user".to_string(), "myuser".to_string());
        map.insert("password".to_string(), "mypassword".to_string());
        map.insert("database".to_string(), "mydatabase".to_string());

        config.save(map).unwrap();
        assert!(config.validate().is_ok());
        assert_eq!(config.state(), DatabaseConfigState::Valid);
        let conn = config.connect();
        assert!(conn.is_ok());
    }
}
