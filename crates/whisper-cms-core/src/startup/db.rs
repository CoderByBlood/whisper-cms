use std::{collections::HashMap, fmt::Debug};

use secrecy::{ExposeSecret, SecretString};
use sqlx::{Connection, PgConnection};

use crate::startup::config::ConfigError;
use crate::startup::config::ConfigFile;
use crate::startup::config::ConfigMap;
use crate::startup::StartupError;

#[derive(Debug)]
pub enum DatabaseConfiguration {
    Postgres(PostgresConfig),
}

impl DatabaseConfiguration {
    pub fn new(file: ConfigFile) -> DatabaseConfiguration {
        Self::Postgres(PostgresConfig::new(file))
    }
}

impl DatabaseConfiguration {
    pub fn connect(&self) -> Result<DatabaseConnection, StartupError> {
        match self {
            Self::Postgres(pg) => pg.connect(),
        }
    }

    pub fn save(&mut self, config: HashMap<String, String>) -> Result<(), StartupError> {
        match self {
            Self::Postgres(pg) => pg.save(config),
        }
    }

    pub fn validate(&mut self) -> Result<(), StartupError> {
        match self {
            Self::Postgres(pg) => pg.validate(),
        }
    }
}


#[derive(Debug)]
pub enum DatabaseConnection {
    Postgres(PostgresConn),
}

impl DatabaseConnection {
    pub async fn test_connection(&self) -> Result<bool, ConfigError> {
        match self {
            Self::Postgres(pg) => pg.test_connection().await,
        }
    }

    pub fn to_connect_string(&self) -> String {
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

impl PostgresConn {
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
    pub fn new(file: ConfigFile) -> Self {
        Self { file, conn: None }
    }

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

impl PostgresConfig {
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
    fn connect(&self) -> Result<DatabaseConnection, StartupError> {
        match &self.conn {
            Some(conn) => Ok(DatabaseConnection::Postgres(conn.clone())),
            None => Err(StartupError::Mapping(
                "Could not connect - no connection configured - invalid state",
            )),
        }
    }
}

#[cfg(test)]
mod tests {
    use secrecy::SecretString;
    use std::collections::HashMap;
    use tempfile::{tempdir, NamedTempFile};

    use super::*;

    fn build_config_map(valid: bool) -> HashMap<String, String> {
        let mut map = HashMap::new();
        if valid {
            map.insert("host".into(), "localhost".into());
            map.insert("port".into(), "5432".into());
            map.insert("user".into(), "admin".into());
            map.insert("password".into(), "password123!".into());
            map.insert("database".into(), "mydb".into());
        } else {
            map.insert("host".into(), "localhost".into());
            // missing port, user, etc.
        }
        map
    }

    fn new_pg_config(temp_path: &str) -> PostgresConfig {
        let pw = SecretString::new("Password123!".into());
        let salt = "thisisaverysecuresalt";
        let vpw = crate::startup::config::ValidatedPassword::build(
            pw.expose_secret().to_string(),
            salt.to_string(),
        )
        .unwrap();
        let file = ConfigFile::new(vpw, temp_path.to_string());
        PostgresConfig::new(file)
    }

    #[tokio::test]
    async fn test_valid_connection_save_and_validate() {
        let temp_file = NamedTempFile::new().unwrap();
        let temp_path = temp_file.path().to_str().unwrap().to_string();

        let mut config = new_pg_config(&temp_path);
        let valid_map = build_config_map(true);
        config.save(valid_map.clone()).unwrap();

        config.validate().unwrap();

        let db_conn = config.connect().unwrap();
        assert_eq!(
            db_conn.to_connect_string(),
            PostgresConn {
                host: "localhost".into(),
                port: 5432,
                user: "admin".into(),
                password: SecretString::new("password123!".into()),
                database: "mydb".into(),
            }
            .to_connect_string()
        );
    }

    #[tokio::test]
    async fn test_save_invalid_config_fails() {
        let temp_file = NamedTempFile::new().unwrap();
        let temp_path = temp_file.path().to_str().unwrap().to_string();

        let mut config = new_pg_config(&temp_path);
        let bad_map = build_config_map(false);
        let result = config.save(bad_map);
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_validate_when_file_missing_returns_error() {
        let temp_file = NamedTempFile::new().unwrap();
        let mut temp_path = temp_file.path().to_str().unwrap().to_string();

        temp_path.push_str("nonexistent.enc");

        let mut config = new_pg_config(&temp_path);
        let result = config.validate();
        assert!(result.is_err());
        assert_eq!(
            format!("{}", result.unwrap_err()),
            "Could not map configuration error: Nothing to validate, file doesn't exist"
        );
    }

    #[tokio::test]
    async fn test_connect_fails_if_not_configured() {
        let temp_file = NamedTempFile::new().unwrap();
        let temp_path = temp_file.path().to_str().unwrap().to_string();

        let config = new_pg_config(&temp_path);
        let result = config.connect();
        assert!(result.is_err());
        assert_eq!(
        format!("{}", result.unwrap_err()),
        "Could not map configuration error: Could not connect - no connection configured - invalid state"
    );
    }

    fn mock_config_file_with_missing_file() -> ConfigFile {
        let dir = tempdir().unwrap();
        let path = dir.path().join("nonexistent.enc");
        ConfigFile::new(
            crate::startup::config::ValidatedPassword::build(
                "Password123!".into(),
                "secureSaltString123456".into(),
            )
            .unwrap(),
            String::from(path.to_str().unwrap()),
        )
    }

    #[tokio::test]
    async fn test_validate_missing_file_returns_error() {
        let mut config = PostgresConfig::new(mock_config_file_with_missing_file());
        let result = config.validate();
        assert!(matches!(result, Err(StartupError::Mapping(msg)) if msg.contains("doesn't exist")));
    }

    #[tokio::test]
    async fn test_validate_failed_load_returns_error() {
        let mut file = mock_config_file_with_missing_file();
        let _saved = file.save(HashMap::new());
        let mut config = PostgresConfig::new(file);
        let result = config.validate();
        assert!(
            matches!(result, Err(StartupError::Mapping(msg)) if msg.contains("failed to load"))
        );
    }

    #[tokio::test]
    async fn test_save_missing_fields() {
        let mut config = PostgresConfig::new(mock_config_file_with_missing_file());
        let map: HashMap<String, String> = HashMap::new(); // no keys
        let result = config.save(map);
        assert!(matches!(result, Err(StartupError::Mapping(msg)) if msg.contains("missing")));
    }

    #[tokio::test]
    async fn test_connect_without_validation_fails() {
        let config = PostgresConfig::new(mock_config_file_with_missing_file());
        let result = config.connect();
        assert!(matches!(result, Err(StartupError::Mapping(msg)) if msg.contains("invalid state")));
    }

    #[tokio::test]
    async fn test_connect_after_save_and_validate() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("valid.enc");
        let mut config = PostgresConfig::new(ConfigFile::new(
            crate::startup::config::ValidatedPassword::build("Password123!".into(), "secureSaltString123456".into())
                .unwrap(),
            String::from(path.to_str().unwrap()),
        ));

        let mut map = HashMap::new();
        map.insert("host".into(), "localhost".into());
        map.insert("port".into(), "5432".into());
        map.insert("user".into(), "user".into());
        map.insert("password".into(), "secret".into());
        map.insert("database".into(), "db".into());

        config.save(map).unwrap();
        config.validate().unwrap();
        let result = config.connect();
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_validate_with_file_exists_and_not_tried() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("exists.enc");

        // Create dummy encrypted config
        let password =
            crate::startup::config::ValidatedPassword::build("Password123!".into(), "secureSaltString123456".into())
                .unwrap();
        let mut config_file = ConfigFile::new(password, String::from(path.to_str().unwrap()));

        // Write a valid config to disk manually to simulate file existence
        let mut map = HashMap::new();
        map.insert("host".into(), "localhost".into());
        map.insert("port".into(), "5432".into());
        map.insert("user".into(), "user".into());
        map.insert("password".into(), "secret".into());
        map.insert("database".into(), "db".into());

        config_file.save(map).unwrap();

        let mut config = PostgresConfig::new(config_file);
        let result = config.validate();
        assert!(result.is_ok());
    }
}
