mod config;
pub mod db;
mod settings;

use std::{
    f64::consts::E, fmt::Debug, net::{AddrParseError, IpAddr, SocketAddr}, num::ParseIntError
};

use data_encoding::BASE32_NOPAD;
use secrecy::{ExposeSecret, SecretString};
use sqlx::{postgres::PgPoolOptions, Connection, PgConnection, PgPool};
use thiserror::Error;
use validator::ValidationErrors;

use config::{ConfigError, ConfigFile, ValidatedPassword};

use crate::startup::{
    db::{DatabaseConfiguration, DbConfig, DbConn, PostgresConfig},
    settings::Settings,
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
    process: Process,
}

impl Startup {
    #[tracing::instrument(skip_all)]
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
            password: password.clone(),
            hashed,
            port,
            ip,
            filename: filename.to_owned(),
            process: Process::new(ConfigFile::new(password.clone(), filename.to_owned())),
        })
    }
}

#[derive(Debug, Clone)]
pub enum Checkpoint {
    Start,     //ConfigFile::new()
    Missing,   //ConfigFile.exists() -> false
    Loaded,    //ConfigFile.load() -> fails
    Applied,   //DbConfig::new()
    Connected, //DbConfig.test_connection() -> true
    Validated, //Settings.valid() -> true
    Ready,     //Settings.applied
}

//| Field    |   None    |    Some(Ok    |    Some(Err)     |
//|----------|-----------|---------------|------------------|
//| file     | start     | exists        | load failed      |
//| config   | not tried | mapping done  | mapping failed   |
//| conn     | not tried | db connected  | connect failed   |
//| settings | not tried | setting valid | settings invalid |
#[derive(Debug)]
pub struct Process {
    checkpoint: Checkpoint,
    file: Option<Result<ConfigFile, StartupError>>,
    config: Option<Result<DbConfig, StartupError>>,
    conn: Option<Result<DbConn, StartupError>>,
    settings: Option<Result<Settings, StartupError>>,
}

impl Process {
    pub fn new(file: ConfigFile) -> Self {
        Self {
            checkpoint: Checkpoint::Start,
            file: Some(Ok(file)),
            config: None,
            conn: None,
            settings: None,
        }
    }

    pub fn execute(&mut self) -> Result<(), StartupError> {
        self.validate_settings()  //start at the end and wherever it stops (errors out) is where we are
    }

    pub fn checkpoint(&self) -> Checkpoint {
        self.checkpoint.clone()
    }

    fn config_exists(&mut self) -> Result<bool, StartupError> {
        let file_ref = self.file.as_ref();
        Ok(file_ref.is_some() && file_ref.unwrap().as_ref().is_ok() && file_ref.unwrap().as_ref().unwrap().exists())
    }

    fn load_config(&mut self) -> Result<(), StartupError> {
        match self.config_exists() {
            Ok(true) => todo!("load it, populate field and checkpoint"),
            Ok(false) => todo!("return error"),
            Err(_e) => todo!("re throw"),
        }
    }

    fn apply_config(&mut self) -> Result<(), StartupError> {
        match self.load_config() {
            Ok(_) => todo!("apply the mapping, populate field and checkpoint"),
            Err(_e) => todo!("re throw"),
        }
    }

    fn connect_db(&mut self) -> Result<(), StartupError> {
        match self.apply_config() {
            Ok(_) => todo!("get the database connection and try to connect,and populate field and checkpoint"),
            Err(_e) => todo!("re throw"),
        }
    }

    fn validate_settings(&mut self) -> Result<(), StartupError> {
        match self.connect_db() {
            Ok(_) => todo!("query the database for setting and validate, and populate field and checkpoint"),
            Err(_e) => todo!("re throw"),
        }
    }
}
