mod config;

use std::{
    net::{AddrParseError, IpAddr},
    num::ParseIntError,
    path::Path,
};

use data_encoding::BASE32_NOPAD;
use secrecy::{ExposeSecret, SecretString};
use sqlx::{postgres::PgPoolOptions, Connection, PgConnection, PgPool};
use thiserror::Error;
use validator::ValidationErrors;

use config::{ConfigError, ConfigSerializer, Encrypted, JsonCodec, ValidatedPassword};

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
    //#[error("Database access error: {0}")]
    //Database(#[from] sqlx::Error),
}

pub struct Startup {
    hashed: SecretString,
    port: u16,
    ip: IpAddr,
    ser: ConfigSerializer<JsonCodec, Encrypted>,
}

impl Startup {
    pub fn build(
        password: String,
        salt: String,
        port: u16,
        address: String,
    ) -> Result<Self, StartupError> {
        let ip: IpAddr = address.parse()?;

        let valid_password = ValidatedPassword::build(password, salt)?;
        let hashed = SecretString::new(valid_password.as_hashed().to_string().into_boxed_str());

        let format = JsonCodec {};
        let transformation = Encrypted::new(valid_password);

        Ok(Self {
            hashed,
            port,
            ip,
            ser: ConfigSerializer::new(format, transformation),
        })
    }
}

pub trait DatabaseConfig {
    async fn test_connection(&self) -> Result<bool, ConfigError>;
    fn to_connect_string(&self) -> String;
}

pub struct PostgresConfig {
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

impl DatabaseConfig for PostgresConfig {
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

pub trait Configuration {
    type DBConfig: DatabaseConfig;
    fn get_configuration(&self) -> Result<Option<Self::DBConfig>, StartupError>;
}

impl Configuration for Startup {
    type DBConfig = PostgresConfig;

    fn get_configuration(&self) -> Result<Option<Self::DBConfig>, StartupError> {
        let hash = self.hashed.expose_secret();
        // Step 1: Reverse the string
        let reversed: String = hash.chars().rev().collect();

        // Step 2: Base32 encode the reversed bytes
        let encoded = BASE32_NOPAD.encode(reversed.as_bytes());

        // Step 3: Normalize to lowercase
        let lowercase = encoded.to_lowercase();

        // Step 4: Truncate to 128 characters
        let mut filename: String = lowercase.chars().take(128).collect();

        filename.push_str(".enc");

        let file = Path::new(filename.as_str());

        if file.exists() {
            let de_ser = self.ser.load_from_path(file)?;
            let host = de_ser
                .get("host")
                .ok_or(ConfigError::Format("missing `host` key".to_string()))?
                .to_owned();

            let port: u16 = de_ser
                .get("port")
                .ok_or(ConfigError::Format("missing `port` key".to_string()))?
                .parse()?;

            let user = de_ser
                .get("user")
                .ok_or(ConfigError::Format("missing `user` key".to_string()))?
                .to_owned();

            let password = de_ser
                .get("password")
                .ok_or(ConfigError::Format("missing `password` key".to_string()))?
                .to_owned();

            let database = de_ser
                .get("database")
                .ok_or(ConfigError::Format("missing `database` key".to_string()))?
                .to_owned();

            return Ok(Some(PostgresConfig {
                host,
                port,
                user,
                password,
                database,
            }));
        }

        Ok(None)
    }
}

#[cfg(test)]
mod tests {
    use argon2::password_hash;

    use super::*; // Bring functions from outer scope

    #[test]
    fn test_startup_get_configuration_none() -> Result<(), StartupError> {
        let password = "125anc$$DD".to_string();
        let salt = "123456789101112135".to_string();
        let port = 34;
        let address = "0.0.0.0".to_string();

        let startup = Startup::build(password, salt, port, address)?;

        assert!(startup.get_configuration()?.is_none());
        Ok(())
    }

    #[test]
    fn test_postgres_connect_string() {
        let config = PostgresConfig {
            host: "localhost".to_string(),
            port: 123,
            user: "user".to_string(),
            password: "pass".to_string(),
            database: "db".to_string(),
        };

        assert_eq!(
            "postgresql://user:pass@localhost:123/db",
            config.to_connect_string()
        )
    }

    #[tokio::test]
    #[ignore = "requires database"]
    async fn test_postgres_connection() {
        let config = PostgresConfig {
            host: "localhost".to_string(),
            port: 5432,
            user: "myuser".to_string(),
            password: "mypassword".to_string(),
            database: "mydatabase".to_string(),
        };

        assert!(config.test_connection().await.unwrap());
    }
}
