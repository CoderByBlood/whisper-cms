mod config;
pub mod db;
mod settings;

use std::{net::AddrParseError, num::ParseIntError};

use data_encoding::BASE32_NOPAD;
use secrecy::{ExposeSecret, SecretString};
use std::future::Future;
use thiserror::Error;
use tokio::{
    runtime::{Handle, Runtime, RuntimeFlavor},
    task,
};
use validator::ValidationErrors; // for .now_or_never()

use config::{ConfigError, ConfigFile, ValidatedPassword};

use crate::{
    request::ManagerError,
    startup::{
        db::{DatabaseConfiguration, DatabaseConnection},
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
    #[error("Failed at step: {0:?} - because of error: {1}")]
    Startup(Checkpoint, StartupError),

    #[error("Failed at step: {0:?} - because of error: {1}")]
    Config(Checkpoint, ConfigError),

    #[error("Failed at step: {0:?} - with message: {1}")]
    Step(Checkpoint, &'static str),
}

impl ProcessError {
    pub fn checkpoint(&self) -> &Checkpoint {
        match self {
            ProcessError::Startup(cp, _) => cp,
            ProcessError::Step(cp, _) => cp,
            ProcessError::Config(cp, _) => cp,
        }
    }
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
    Missing,   //.exists() -> false
    Exists,    //.exists() -> true && .load() -> fails
    Loaded,    //.load() -> succeeds && .apply() -> fails
    Applied,   //.apply() -> succeeds && .test_connect() -> fails
    Connected, //.test_connection() -> succeeds && .validate() -> fails
    Validated, //.validate() -> succeeds && .execute() -> fails
    Ready,     //.execute() -> succeeds
}
#[derive(Debug)]
pub struct Process {
    file: Option<ConfigFile>,
    config: Option<DatabaseConfiguration>,
    conn: Option<DatabaseConnection>,
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
            Err(ProcessError::Step(
                Checkpoint::Missing,
                "Configuration file does not exist",
            ))
        }
    }

    #[tracing::instrument(skip_all)]
    fn load_config(&mut self) -> Result<Checkpoint, ProcessError> {
        self.config_exists()?;
        self.config = Some(DatabaseConfiguration::new(self.file.take().unwrap()));
        Ok(Checkpoint::Loaded)
    }

    #[tracing::instrument(skip_all)]
    fn apply_config(&mut self) -> Result<Checkpoint, ProcessError> {
        let step = self.load_config()?;
        let db = self.config.as_mut().unwrap();

        match db.validate() {
            Ok(_) => match db.connect() {
                Ok(conn) => {
                    self.conn = Some(conn);
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

        let result = block_in_runtime(async { conn.test_connection().await });

        match result {
            Ok(truth) => match truth {
                true => Ok(Checkpoint::Connected),
                false => Err(ProcessError::Step(step, "Connection to Database failed")),
            },
            Err(e) => Err(ProcessError::Config(step, e)),
        }
    }

    #[tracing::instrument(skip_all)]
    fn validate_settings(&mut self) -> Result<(), ProcessError> {
        let _step = self.connect_db()?;
        Err(ProcessError::Step(
            Checkpoint::Validated,
            "Code Path Not Implemented",
        ))
    }
}

pub fn block_in_runtime<F>(fut: F) -> F::Output
where
    F: Future,
{
    match Handle::try_current() {
        Ok(handle) => match handle.runtime_flavor() {
            // Multi-threaded runtime (Axum): safe to block
            RuntimeFlavor::MultiThread => task::block_in_place(|| handle.block_on(fut)),

            // Single-threaded runtime (#[tokio::test]): we cannot block the executor thread
            RuntimeFlavor::CurrentThread | _ => {
                // Manually poll the future without requiring 'static
                futures::executor::block_on(fut)
            }
        },

        // Outside a runtime: create a temporary one
        Err(_) => Runtime::new().unwrap().block_on(fut),
    }
}
