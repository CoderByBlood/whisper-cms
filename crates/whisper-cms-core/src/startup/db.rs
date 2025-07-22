use std::{collections::HashMap, fmt::Debug};

use secrecy::{ExposeSecret, SecretString};
use sqlx::{postgres::PgPoolOptions, Connection, PgConnection, PgPool};
use thiserror::Error;

use crate::startup::{
    config::{ConfigError, ConfigFile},
    StartupError,
};

use crate::startup::config::ConfigMap;

pub trait DatabaseConfiguration: Debug {
    fn save(&mut self, config: HashMap<String, String>) -> Result<(), StartupError>;
    fn connect(&self) -> Result<impl DatabaseConnection, StartupError>;
    fn validate(&mut self) -> Result<(), StartupError>;
}

#[derive(Debug)]
pub enum DbConfig {
    Postgres(PostgresConfig),
}

impl DatabaseConfiguration for DbConfig {
    fn connect(&self) -> Result<impl DatabaseConnection, StartupError> {
        match self {
            Self::Postgres(pg) => pg.connect(),
        }
    }

    fn save(&mut self, config: HashMap<String, String>) -> Result<(), StartupError> {
        match self {
            Self::Postgres(pg) => pg.save(config),
        }
    }

    fn validate(&mut self) -> Result<(), StartupError> {
        match self {
            Self::Postgres(pg) => pg.validate(),
        }
    }
}

pub trait DatabaseConnection: Debug {
    async fn test_connection(&self) -> Result<bool, ConfigError>;
    fn to_connect_string(&self) -> String;
}

#[derive(Debug)]
pub enum DbConn {
    Postgres(PostgresConn),
}

impl DatabaseConnection for DbConn {
    async fn test_connection(&self) -> Result<bool, ConfigError> {
        match self {
            Self::Postgres(pg) => pg.test_connection().await,
        }
    }

    fn to_connect_string(&self) -> String {
        match self {
            Self::Postgres(pg) => pg.to_connect_string(),
        }
    }
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
    file: ConfigFile,
    conn: Option<PostgresConn>,
}

impl DatabaseConnection for PostgresConn {
    #[tracing::instrument(skip_all)]
    async fn test_connection(&self) -> Result<bool, ConfigError> {
        let conn_str = self.to_connect_string().replace(
            &format!("{:?}", self.password),
            self.password.expose_secret(),
        );

        // Test the connection using ping (available in sqlx 0.8.6)
        PgConnection::connect(&conn_str).await?.ping().await?;
        Ok(true)
    }

    #[tracing::instrument(skip_all)]
    fn to_connect_string(&self) -> String {
        format!(
            "postgresql://{}:{:?}@{}:{}/{}",
            self.user, self.password, self.host, self.port, self.database
        )
    }
}

impl PostgresConfig {
    #[tracing::instrument(skip_all)]
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
    #[tracing::instrument(skip_all)]
    fn save(&mut self, config: HashMap<String, String>) -> Result<(), StartupError> {
        self.conn = Some(Self::build_connection(&config)?);
        self.file.save(config)?;
        Ok(())
    }

    #[tracing::instrument(skip_all)]
    fn validate(&mut self) -> Result<(), StartupError> {
        match self.conn {
            None => match self.file.tried() {
                None => {
                    if self.file.exists() {
                        self.conn = Some(Self::build_connection(&self.file.load()?)?);
                        Ok(())
                    } else {
                        Err(StartupError::Mapping(
                            "Nothing to validate, file doesn't exist",
                        ))
                    }
                }
                Some(tried) => {
                    if tried {
                        self.conn = Some(Self::build_connection(&self.file.load()?)?);
                        Ok(())
                    } else {
                        Err(StartupError::Mapping(
                            "Nothing to validate, file failed to load",
                        ))
                    }
                }
            },
            Some(_) => Ok(()),
        }
    }

    #[tracing::instrument(skip_all)]
    fn connect(&self) -> Result<impl DatabaseConnection, StartupError> {
        match &self.conn {
            Some(conn) => Ok(conn.clone()),
            None => Err(StartupError::Mapping(
                "Could not connect - no connection configured - invalid state",
            )),
        }
    }
}
