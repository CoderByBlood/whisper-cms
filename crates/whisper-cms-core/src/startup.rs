mod config;
pub mod db;
mod settings;

use std::{net::AddrParseError, num::ParseIntError};

use data_encoding::BASE32_NOPAD;
use secrecy::{ExposeSecret, SecretString};
use thiserror::Error;
use validator::ValidationErrors;

use config::{ConfigError, ConfigFile, ValidatedPassword};

use crate::{
    request::ManagerError,
    startup::{
        db::{DatabaseConfiguration, DatabaseConnection, DbConfig, DbConn, PostgresConfig},
        settings::Settings,
    },
};

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

    #[error("Could not boot request manager error: {0}")]
    Request(#[from] ManagerError),

    #[error("Could not map configuration error: {0}")]
    Mapping(&'static str),
    //#[error("Database access error: {0}")]
    //Database(#[from] sqlx::Error),
}

#[derive(Debug, Error)]
pub enum ProcessError {
    #[error("Failed at step: {0:?} - because: {1}")]
    Startup(Checkpoint, StartupError),

    #[error("Failed at step: {0:?} - because: {1}")]
    Message(Checkpoint, &'static str),
}
#[derive(Debug)]
pub struct Startup {}

impl Startup {
    #[tracing::instrument(skip_all)]
    pub fn build(password: String, salt: String) -> Result<Process, StartupError> {
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

        Ok(Process::new(ConfigFile::new(password, filename)))
    }
}

#[derive(Debug)]
pub enum Checkpoint {
    Missing,   //ConfigFile.exists() -> false
    Exists,    //ConfigFile.exists() -> true
    Loaded,    //ConfigFile.load() -> fails
    Applied,   //DbConfig::new()
    Connected, //DbConfig.test_connection() -> true
    Validated, //Settings.valid() -> true
    Ready,     //Settings.applied
}
// Case 1 - config file is missing: ConfigState::Missing
// Case 2 - config file exists but is invalid: ConfigState::Exists -> config.validate() -> ConfigState::Invalid
// Case 3 - config file exists and is valid: ConfigState::Exists -> config.validate() -> ConfigState::Valid
// Case 4 - config file exists, is valid, but unable to connect to DB: ConfigState::Valid -> config.get_connection().test_connection() -> false
// Case 5 - config file exists, is valid, connects to DB: ConfigState::Valid -> config.get_connection().test_connection() -> true
// Case 6 - config file exists, is valid, connects to DB, but no settings: ConfigState::Valid -> ...test_connection() -> true -> SettingsState:Empty
// Case 7 - config file exists, is valid, connects to DB, settings exists, settings are invalid: ConfigState::Valid -> ...test_connection() -> true -> SettingsState:Invalid
// Case 8 - config file exists, is valid, connects to DB, setting exists, settings valid: ConfigState::Valid -> ...test_connection() -> true -> SettingsState:Valid
//| Field    |   None    |    Some(Ok    |    Some(Err)     |
//|----------|-----------|---------------|------------------|
//| file     | start     | exists        | load failed      |
//| config   | not tried | mapping done  | mapping failed   |
//| conn     | not tried | db connected  | connect failed   |
//| settings | not tried | setting valid | settings invalid |
#[derive(Debug)]
pub struct Process {
    file: Option<ConfigFile>,
    config: Option<DbConfig>,
    conn: Option<DbConn>,
    settings: Option<Settings>,
}

impl Process {
    #[tracing::instrument(skip_all)]
    pub fn new(file: ConfigFile) -> Self {
        Self {
            file: Some(file),
            config: None,
            conn: None,
            settings: None,
        }
    }

    #[tracing::instrument(skip_all)]
    pub fn execute(&mut self) -> Result<Checkpoint, ProcessError> {
        //start at the end and wherever it stops (errors out) is where we are
        self.validate_settings()?;
        Ok(Checkpoint::Ready)
    }

    #[tracing::instrument(skip_all)]
    fn config_exists(&mut self) -> Result<Checkpoint, ProcessError> {
        let file_ref = self.file.as_ref();
        if file_ref.is_some() && file_ref.unwrap().exists() {
            Ok(Checkpoint::Exists)
        } else {
            Err(ProcessError::Message(
                Checkpoint::Missing,
                "Configuration file does not exist",
            ))
        }
    }

    #[tracing::instrument(skip_all)]
    fn load_config(&mut self) -> Result<Checkpoint, ProcessError> {
        self.config_exists()?;

        let pg = PostgresConfig::new(self.file.take().unwrap());

        self.config = Some(DbConfig::Postgres(pg));
        Ok(Checkpoint::Loaded)
    }

    #[tracing::instrument(skip_all)]
    fn apply_config(&mut self) -> Result<Checkpoint, ProcessError> {
        let step = self.load_config()?;
        let db = self.config.as_mut().unwrap();

        match db.validate() {
            Ok(_) => match db.connect() {
                Ok(conn) => {
                    self.conn = Some(conn.to_db_conn());
                    Ok(Checkpoint::Applied)
                }
                Err(e) => Err(ProcessError::Startup(step, e)),
            },
            Err(e) => Err(ProcessError::Startup(step, e)),
        }
    }

    #[tracing::instrument(skip_all)]
    fn connect_db(&mut self) -> Result<Checkpoint, ProcessError> {
        let step = self.apply_config()?;
        let conn = self.conn.as_mut().unwrap();

        // Used only if *not* in a tokio runtime
        //let rt = Runtime::new().unwrap();
        //let result = rt.block_on(db.connect()?.test_connection());

        let result: Result<bool, StartupError> = tokio::task::block_in_place(|| {
            let test = tokio::runtime::Handle::current().block_on(conn.test_connection())?;
            Ok(test)
        });

        match result {
            Ok(truth) => match truth {
                true => Ok(Checkpoint::Connected),
                false => Err(ProcessError::Message(step, "Connection to Database failed")),
            },
            Err(e) => Err(ProcessError::Startup(step, e)),
        }
    }

    #[tracing::instrument(skip_all)]
    fn validate_settings(&mut self) -> Result<(), ProcessError> {
        let _step = self.connect_db()?;
        Err(ProcessError::Message(
            Checkpoint::Validated,
            "Code Path Not Implemented",
        ))
    }
}
