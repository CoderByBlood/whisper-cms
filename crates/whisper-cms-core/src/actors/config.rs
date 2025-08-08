use data_encoding::BASE32_NOPAD;
use libsql::{params, Builder, Database};
use ractor::{Actor, ActorProcessingErr, ActorRef, RpcReplyPort};

use secrecy::{ExposeSecret, SecretString};
use thiserror::Error;
use tracing::debug;

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use crate::actors::request::ValidatedPassword;
use crate::CliArgs;

pub type ConfigMap = HashMap<String, String>;

#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("Could not startup due to: {0}")]
    Internal(&'static str),

    #[error("Could not startup due to: {0}")]
    Transformation(String),

    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error("Serde error: {0}")]
    Serde(#[from] serde_json::Error),

    #[error("Database error: {0}")]
    LibSql(#[from] libsql::Error),
}

#[derive(Debug)]
pub struct ConfigArgs {
    pub args: CliArgs,
}

#[derive(Debug)]
pub enum ConfigEnvelope {
    Load {
        reply: RpcReplyPort<Result<ConfigReply, ConfigError>>,
    },
}

#[derive(Debug)]
pub enum ConfigReply {
    Load(ConfigMap),
}

#[derive(Debug)]
pub struct ConfigState {
//    password: ValidatedPassword,
    file: ConfigFile,
}

#[derive(Debug)]
pub struct Config;

impl Actor for Config {
    type Msg = ConfigEnvelope;
    type State = ConfigState;
    type Arguments = ConfigArgs;

    #[tracing::instrument(skip_all)]
    async fn pre_start(
        &self,
        _me: ActorRef<Self::Msg>,
        args: Self::Arguments,
    ) -> Result<Self::State, ActorProcessingErr> {
        debug!("with {args:?}");
        let raw = args.args.password;
        let salt = args.args.salt;
        let password = ValidatedPassword::build(raw, salt)?;
        let hashed = SecretString::from(password.as_hashed());

        let hash = hashed.expose_secret();
        // Step 1: Reverse the string
        let reversed: String = hash.chars().rev().collect();

        // Step 2: Base32 encode the reversed bytes
        let encoded = BASE32_NOPAD.encode(reversed.as_bytes());

        // Step 3: Normalize to lowercase
        let lowercase = encoded.to_lowercase();

        // Step 4: Truncate to 128 characters
        let mut path: String = lowercase.chars().take(128).collect();

        // Step 5: Add the extension
        path.push_str(".enc");
        let file = ConfigFile::as_local_db(path);

        Ok(ConfigState { /*password,*/ file })
    }

    #[tracing::instrument(skip_all)]
    async fn handle(
        &self,
        _me: ActorRef<Self::Msg>,
        msg: Self::Msg,
        state: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        debug!("{msg:?} with state {state:?}");

        match msg {
            ConfigEnvelope::Load { reply } => {
                match state.file.load().await {
                    Ok(map) => Ok(reply.send(Ok(ConfigReply::Load(map)))?),
                    Err(e) => Ok(reply.send(Err(e))?)
                }
            }
        }
    }
}

#[derive(Debug)]
pub struct ConfigFile {
    ser: Serializer,
    path: PathBuf,
    tried: Option<bool>,
}

impl ConfigFile {
    #[tracing::instrument(skip_all)]
    pub fn as_local_db(path: String) -> Self {
        let ser = Serializer::JsonNoopSql(LibSqlSerializer::new(JsonCodec, Noop));

        Self {
            ser,
            path: PathBuf::from(path),
            tried: None,
        }
    }

    #[tracing::instrument(skip_all)]
    pub fn exists(&self) -> bool {
        self.path.exists()
    }

    #[tracing::instrument(skip_all)]
    pub fn tried(&self) -> Option<bool> {
        self.tried
    }

    #[tracing::instrument(skip_all)]
    pub async fn load(&mut self) -> Result<ConfigMap, ConfigError> {
        self.ser
            .load_from_path(&*self.path)
            .await
            .map(|result| {
                self.tried = Some(true);
                result
            })
            .map_err(|err| {
                self.tried = Some(false);
                err
            })
    }

    #[tracing::instrument(skip_all)]
    pub async fn save(&mut self, config: ConfigMap) -> Result<(), ConfigError> {
        self.ser
            .save_to_path(&config, &*self.path)
            .await
            .map(|result| {
                self.tried = Some(true);
                result
            })
            .map_err(|err| {
                self.tried = Some(false);
                err
            })
    }
}

pub trait FormatCodec: Send + Sync + std::fmt::Debug {
    fn encode(&self, map: &ConfigMap) -> Result<Vec<u8>, ConfigError>;
    fn decode(&self, data: &[u8]) -> Result<ConfigMap, ConfigError>;
}
#[derive(Debug)]
pub struct JsonCodec;

impl FormatCodec for JsonCodec {
    #[tracing::instrument(skip_all)]
    fn encode(&self, map: &ConfigMap) -> Result<Vec<u8>, ConfigError> {
        Ok(serde_json::to_vec(map)?)
    }

    #[tracing::instrument(skip_all)]
    fn decode(&self, data: &[u8]) -> Result<ConfigMap, ConfigError> {
        Ok(serde_json::from_slice(data)?)
    }
}

pub trait Transformation: Send + Sync + std::fmt::Debug {
    fn pack(&self, input: &[u8]) -> Result<Vec<u8>, ConfigError>;
    fn unpack(&self, input: &[u8]) -> Result<Vec<u8>, ConfigError>;
}

#[derive(Debug)]
pub struct Noop;

impl Transformation for Noop {
    fn pack(&self, input: &[u8]) -> Result<Vec<u8>, ConfigError> {
        Ok(input.to_vec())
    }

    fn unpack(&self, input: &[u8]) -> Result<Vec<u8>, ConfigError> {
        Ok(input.to_vec())
    }
}

#[derive(Debug)]
enum Serializer {
    JsonNoopSql(LibSqlSerializer<JsonCodec, Noop>),
}

impl Serializer {
    async fn load_from_path(&mut self, path: &Path) -> Result<ConfigMap, ConfigError> {
        match self {
            Serializer::JsonNoopSql(ser) => ser.load_from_path(path).await,
        }
    }

    async fn save_to_path(&mut self, map: &ConfigMap, path: &Path) -> Result<(), ConfigError> {
        match self {
            Serializer::JsonNoopSql(ser) => ser.save_to_path(map, path).await,
        }
    }
}
#[derive(Debug)]
pub struct FileSerializer<F, T>
where
    F: FormatCodec,
    T: Transformation,
{
    format: F,
    transformation: T,
}

impl<F, T> FileSerializer<F, T>
where
    F: FormatCodec,
    T: Transformation,
{
    #[tracing::instrument(skip_all)]
    pub fn new(format: F, transformation: T) -> Self {
        Self {
            format,
            transformation,
        }
    }

    #[tracing::instrument(skip_all)]
    pub fn save_to_path(&self, map: &ConfigMap, path: &Path) -> Result<(), ConfigError> {
        let encoded = self.format.encode(map)?;
        let packed = self.transformation.pack(&encoded)?;
        std::fs::write(path, packed)?;
        Ok(())
    }

    #[tracing::instrument(skip_all)]
    pub fn load_from_path(&self, path: &Path) -> Result<ConfigMap, ConfigError> {
        let data = std::fs::read(path)?;
        let unpacked = self.transformation.unpack(&data)?;
        let map = self.format.decode(&unpacked)?;
        Ok(map)
    }
}
#[derive(Debug)]
pub struct LibSqlSerializer<F, T>
where
    F: FormatCodec,
    T: Transformation,
{
    format: F,
    transformation: T,
    database: Option<Database>,
}

impl<F, T> LibSqlSerializer<F, T>
where
    F: FormatCodec,
    T: Transformation,
{
    #[tracing::instrument(skip_all)]
    pub fn new(format: F, transformation: T) -> Self {
        Self {
            format,
            transformation,
            database: None,
        }
    }

    #[tracing::instrument(skip_all)]
    pub async fn save_to_path(&mut self, map: &ConfigMap, path: &Path) -> Result<(), ConfigError> {
        let db = self.build_database(path).await?;
        let conn = db.connect()?;

        // Create table if it doesn't exist
        conn.execute(
            "CREATE TABLE IF NOT EXISTS settings (id INTEGER PRIMARY KEY, data TEXT NOT NULL)",
            (),
        )
        .await?;

        let encoded = self.format.encode(map)?;
        let packed = self.transformation.pack(&encoded)?;

        conn.execute(
            "INSERT INTO settings (id, data) VALUES (?1, ?2)",
            (1, packed.as_slice()),
        )
        .await?;
        Ok(())
    }

    #[tracing::instrument(skip_all)]
    pub async fn load_from_path(&mut self, path: &Path) -> Result<ConfigMap, ConfigError> {
        let db = self.build_database(path).await?;
        let conn = db.connect()?;

        let mut stmt = conn
            .prepare("SELECT data FROM settings WHERE id = ?1")
            .await?;
        let mut rows = stmt.query(params![1i64]).await?;

        if let Some(row) = rows.next().await? {
            let data: Vec<u8> = row.get(0)?;
            let unpacked = self.transformation.unpack(&data)?;
            let map = self.format.decode(&unpacked)?;
            Ok(map)
        } else {
            Err(ConfigError::Transformation("No Data Found".to_string()))
        }
    }

    async fn build_database(&mut self, path: &Path) -> Result<&Database, ConfigError> {
        if self.database.is_some() {
            // If already built, just return a reference
            Ok(self.database.as_ref().unwrap())
        } else {
            // Build database connection
            let db = Builder::new_local(path).build().await?;
            self.database = Some(db);
            Ok(self.database.as_ref().unwrap())
        }
    }
}




#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[tokio::test]
    async fn test_json_codec_roundtrip() {
        let codec = JsonCodec;
        let mut map = ConfigMap::new();
        map.insert("key".into(), "value".into());

        let encoded = codec.encode(&map).unwrap();
        let decoded = codec.decode(&encoded).unwrap();

        assert_eq!(map, decoded);
    }

    #[tokio::test]
    async fn test_validated_password_build_verify() {
        let password = String::from("StrongPass1$");
        let salt = "longsufficientlysalt".into();

        let validated = ValidatedPassword::build(password.clone(), salt).unwrap();
        assert!(validated.verify(&password));
        assert!(!validated.verify("wrongpass"));
    }

    #[tokio::test]
    async fn test_validated_password_eq_secure_false_for_different_hashes() {
        let pw1 =
            ValidatedPassword::build("StrongPass1$".into(), "salt123456789012".into()).unwrap();
        let pw2 =
            ValidatedPassword::build("StrongPass1$".into(), "salt999999999999".into()).unwrap();
        assert!(!pw1.eq_secure(&pw2)); // different salts → different hashes
    }

    #[tokio::test]
    async fn test_json_db_config_serializer_save_and_load() {
        //let password =
        //    ValidatedPassword::build("StrongPass1$".into(), "longsufficientlysalt".into()).unwrap();
        let codec = JsonCodec;
        let enc = Noop;
        let mut serializer = LibSqlSerializer::new(codec, enc);

        let mut map = ConfigMap::new();
        map.insert("api_key".into(), "1234567890".into());

        let dir = tempdir().unwrap();
        let path = dir.path().join("config.enc");

        serializer.save_to_path(&map, &path).await.unwrap();
        let restored = serializer.load_from_path(&path).await.unwrap();

        assert_eq!(map, restored);
    }

    #[tokio::test]
    async fn test_validation_fails_on_weak_password() {
        let weak_password = "password".into(); // No uppercase, digit, or symbol
        let salt = "longsufficientlysalt".into();

        let result = ValidatedPassword::build(weak_password, salt);
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_validated_password_debug_does_not_expose_secret() {
        let password =
            ValidatedPassword::build("StrongPass1$".into(), "longsufficientlysalt".into()).unwrap();
        let debug_str = format!("{:?}", password);
        assert!(debug_str.contains("**REDACTED**"));
    }

    #[tokio::test]
    async fn test_validated_password_serde_protected() {
        let password =
            ValidatedPassword::build("StrongPass1$".into(), "longsufficientlysalt".into()).unwrap();
        let serialized = serde_json::to_string(&password).unwrap();
        assert_eq!(serialized, "\"**REDACTED**\"");

        let json = "\"irrelevant\"";
        let result: Result<ValidatedPassword, _> = serde_json::from_str(json);
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_database_configuration_file_save_and_load_updates_tried() {
        //let password =
        //    ValidatedPassword::build("StrongPass1$".into(), "longsufficientlysalt".into()).unwrap();

        let dir = tempdir().unwrap();
        let path = dir.path().join("config_file.enc");
        let path_str = path.to_str().unwrap();

        let mut file = ConfigFile::as_local_db(path_str.to_owned());

        let mut map = ConfigMap::new();
        map.insert("theme".into(), "dark".into());

        assert_eq!(file.tried(), None);

        file.save(map.clone()).await.unwrap();
        assert_eq!(file.tried(), Some(true));

        let loaded = file.load().await.unwrap();
        assert_eq!(file.tried(), Some(true));
        assert_eq!(loaded, map);
    }

    #[tokio::test]
    async fn test_database_configuration_file_load_fails_sets_tried_false() {
        //let password =
        //    ValidatedPassword::build("StrongPass1$".into(), "longsufficientlysalt".into()).unwrap();

        let dir = tempdir().unwrap();
        let path = dir.path().join("missing.enc");
        let path_str = path.to_str().unwrap();

        let mut file = ConfigFile::as_local_db(path_str.to_owned());
        let result = file.load().await;

        assert!(result.is_err());
        assert_eq!(file.tried(), Some(false));
    }
}
