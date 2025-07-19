use aes_gcm::aead::{Aead, KeyInit};
//use aes_gcm::aes::cipher::StreamCipher;
use aes_gcm::{Aes256Gcm, Key, Nonce};
use argon2::{
    password_hash::{PasswordHash, PasswordVerifier, SaltString},
    Algorithm, Argon2, Params, PasswordHasher, Version,
};
use rand::{rngs::OsRng, Rng};

use secrecy::{ExposeSecret, SecretString};
use serde::{Deserialize, Serialize};
use subtle::ConstantTimeEq;
use thiserror::Error;
use validator::{Validate, ValidationError, ValidationErrors};
use zeroize::Zeroize;

use std::collections::HashMap;
use std::path::Path;

const SALT_LEN: usize = 16;
const NONCE_LEN: usize = 12;
const KEY_LEN: usize = 32;

pub type ConfigMap = HashMap<String, String>;
#[derive(Debug)]
pub struct ConfigurationFile {
    ser: Serializers,
    path: Box<Path>,
    tried: Option<bool>,
}

impl ConfigurationFile {
    pub fn new(password: ValidatedPassword, path: &str) -> ConfigurationFile {
        ConfigurationFile {
            ser: Serializers::JsonEncrypted(ConfigSerializer::new(
                JsonCodec {},
                Encrypted::new(password),
            )),
            path: Path::new(path).to_path_buf().into_boxed_path(),
            tried: None,
        }
    }

    pub fn exists(&self) -> bool {
        self.path.exists()
    }

    pub fn tried(&self) -> Option<bool> {
        self.tried
    }

    pub fn load(&mut self) -> Result<ConfigMap, ConfigError> {
        match self.ser.load_from_path(&*self.path) {
            Ok(result) => {
                self.tried = Some(true);
                Ok(result)
            }
            Err(err) => {
                self.tried = Some(false);
                Err(err)
            }
        }
    }

    pub fn save(&mut self, config: ConfigMap) -> Result<(), ConfigError> {
        match self.ser.save_to_path(&config, &*self.path){
            Ok(result) => {
                self.tried = Some(true);
                Ok(result)
            }
            Err(err) => {
                self.tried = Some(false);
                Err(err)
            }
        }
    }
}

#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    //#[error("Format error: {0}")]
    //Format(&'static str),
    #[error("Transformation error: {0}")]
    Transformation(String),

    #[error("Serde error: {0}")]
    Serde(#[from] serde_json::Error),

    #[error("Database error: {0}")]
    Database(#[from] sqlx::Error),
}

pub trait FormatCodec: Send + Sync {
    fn encode(&self, map: &ConfigMap) -> Result<Vec<u8>, ConfigError>;
    fn decode(&self, data: &[u8]) -> Result<ConfigMap, ConfigError>;
}
#[derive(Debug)]
pub struct JsonCodec;

impl FormatCodec for JsonCodec {
    fn encode(&self, map: &ConfigMap) -> Result<Vec<u8>, ConfigError> {
        Ok(serde_json::to_vec(map)?)
    }

    fn decode(&self, data: &[u8]) -> Result<ConfigMap, ConfigError> {
        Ok(serde_json::from_slice(data)?)
    }
}

pub trait Transformation: Send + Sync {
    fn pack(&self, input: &[u8]) -> Result<Vec<u8>, ConfigError>;
    fn unpack(&self, input: &[u8]) -> Result<Vec<u8>, ConfigError>;
}
#[derive(Debug)]
pub struct Encrypted {
    password: ValidatedPassword,
    argon2: Argon2<'static>,
}

impl Encrypted {
    pub fn new(password: ValidatedPassword) -> Self {
        let params = Params::new(65536, 3, 1, Some(KEY_LEN)).expect("Invalid Argon2 parameters");
        let argon2 = Argon2::new(Algorithm::Argon2id, Version::V0x13, params);

        Self { password, argon2 }
    }

    fn derive_key(&self, salt: &[u8]) -> Result<[u8; KEY_LEN], ConfigError> {
        let mut key = [0u8; KEY_LEN];
        self.argon2
            .hash_password_into(self.password.raw.expose_secret().as_bytes(), salt, &mut key)
            .map_err(|e| ConfigError::Transformation(format!("Argon2 KDF failed: {e}")))?;
        Ok(key)
    }
}

impl Transformation for Encrypted {
    fn pack(&self, input: &[u8]) -> Result<Vec<u8>, ConfigError> {
        let mut rng = OsRng;

        // Generate random salt
        let mut salt = [0u8; SALT_LEN];
        rng.fill(&mut salt);

        // Derive encryption key
        let key_bytes = self.derive_key(&salt)?;
        let key = Key::<Aes256Gcm>::from_slice(&key_bytes);

        // Generate random nonce
        let mut nonce_bytes = [0u8; NONCE_LEN];
        rng.fill(&mut nonce_bytes);
        let nonce = Nonce::from_slice(&nonce_bytes);

        // Encrypt
        let cipher = Aes256Gcm::new(key);
        let ciphertext = cipher
            .encrypt(nonce, input)
            .map_err(|e| ConfigError::Transformation(format!("Encryption failed: {e}")))?;

        // Compose output: [salt || nonce || ciphertext]
        let mut result = Vec::with_capacity(SALT_LEN + NONCE_LEN + ciphertext.len());
        result.extend_from_slice(&salt);
        result.extend_from_slice(&nonce_bytes);
        result.extend_from_slice(&ciphertext);

        Ok(result)
    }

    fn unpack(&self, input: &[u8]) -> Result<Vec<u8>, ConfigError> {
        if input.len() < SALT_LEN + NONCE_LEN {
            return Err(ConfigError::Transformation("Input too short".into()));
        }

        // Split input
        let salt = &input[..SALT_LEN];
        let nonce_bytes = &input[SALT_LEN..SALT_LEN + NONCE_LEN];
        let ciphertext = &input[SALT_LEN + NONCE_LEN..];

        // Derive encryption key
        let key_bytes = self.derive_key(salt)?;
        let key = Key::<Aes256Gcm>::from_slice(&key_bytes);
        let nonce = Nonce::from_slice(nonce_bytes);

        // Decrypt
        let cipher = Aes256Gcm::new(key);
        let plaintext = cipher
            .decrypt(nonce, ciphertext)
            .map_err(|e| ConfigError::Transformation(format!("Decryption failed: {e}")))?;

        Ok(plaintext)
    }
}

pub trait Serializer {
    fn save_to_path(&self, map: &ConfigMap, path: &Path) -> Result<(), ConfigError>;
    fn load_from_path(&self, path: &Path) -> Result<ConfigMap, ConfigError>;
}

#[derive(Debug)]
enum Serializers {
    JsonEncrypted(ConfigSerializer<JsonCodec, Encrypted>),
}

impl Serializer for Serializers {
    fn load_from_path(&self, path: &Path) -> Result<ConfigMap, ConfigError> {
        match self {
            Serializers::JsonEncrypted(ser) => ser.load_from_path(path),
        }
    }

    fn save_to_path(&self, map: &ConfigMap, path: &Path) -> Result<(), ConfigError> {
        match self {
            Serializers::JsonEncrypted(ser) => ser.save_to_path(map, path),
        }
    }
}
#[derive(Debug)]
pub struct ConfigSerializer<F, T>
where
    F: FormatCodec,
    T: Transformation,
{
    format: F,
    transformation: T,
}

impl<F, T> ConfigSerializer<F, T>
where
    F: FormatCodec,
    T: Transformation,
{
    pub fn new(format: F, transformation: T) -> Self {
        Self {
            format,
            transformation,
        }
    }

    pub fn save_to_path(&self, map: &ConfigMap, path: &Path) -> Result<(), ConfigError> {
        let encoded = self.format.encode(map)?;
        let packed = self.transformation.pack(&encoded)?;
        std::fs::write(path, packed)?;
        Ok(())
    }

    pub fn load_from_path(&self, path: &Path) -> Result<ConfigMap, ConfigError> {
        let data = std::fs::read(path)?;
        let unpacked = self.transformation.unpack(&data)?;
        let map = self.format.decode(&unpacked)?;
        Ok(map)
    }
}

/// Intermediate struct for validation only
#[derive(Validate)]
struct RawPasswordInput {
    #[validate(length(min = 8))]
    #[validate(custom(function = "validate_password_strength"))]
    password: String,

    #[validate(length(min = 16))]
    salt: String,
}

/// Prevent secret leakage through `Debug`
impl core::fmt::Debug for RawPasswordInput {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "RawPasswordInput(**REDACTED**)")
    }
}

/// Password strength validation rule
fn validate_password_strength(p: &str) -> Result<(), ValidationError> {
    let symbols = "!@#$%^&*()_+-=";
    let has_upper = p.chars().any(|c| c.is_uppercase());
    let has_digit = p.chars().any(|c| c.is_ascii_digit());
    let has_symbol = p.chars().any(|c| symbols.contains(c));
    let mut errors = Vec::<String>::new();

    if !has_upper {
        errors.push("password must include at least one uppercase letter".into());
    }

    if !has_digit {
        errors.push("password must include at least one digit".into());
    }

    if !has_symbol {
        errors.push(format!(
            "password must include at least one special character: {symbols}"
        ));
    }

    if errors.is_empty() {
        Ok(())
    } else {
        let error = ValidationError::new("strength").with_message(format!("{:?}", errors).into());
        Err(error)
    }
}

/// Securely validated and hashed password
#[derive(Zeroize, Clone)]
#[zeroize(drop)]
pub struct ValidatedPassword {
    raw: SecretString,
    hashed: SecretString,
}

impl ValidatedPassword {
    /// Create a new validated and hashed password
    pub fn build(raw: String, salt: String) -> Result<Self, ValidationErrors> {
        let input = RawPasswordInput {
            password: raw,
            salt: salt.clone(),
        };

        input.validate()?;

        let salt = SaltString::encode_b64(salt.as_bytes()).map_err(|e| {
            let mut errors = ValidationErrors::new();
            let err = ValidationError::new("encode")
                .with_message(format!("Base64 encoding failed: {}", e).into());
            errors.add("salt", err);
            errors
        })?;

        let hash = Argon2::default()
            .hash_password(input.password.as_bytes(), &salt)
            .map_err(|e| {
                let mut errors = ValidationErrors::new();
                let err = ValidationError::new("hash")
                    .with_message(format!("Password hashing failed: {}", e).into());
                errors.add("password", err);
                errors
            })?
            .to_string();

        Ok(Self {
            raw: SecretString::new(input.password.into_boxed_str()),
            hashed: SecretString::new(hash.into_boxed_str()),
        })
    }

    /// Verify a raw password against the stored Argon2 hash
    pub fn verify(&self, attempt: &str) -> bool {
        match PasswordHash::new(self.hashed.expose_secret()) {
            Ok(parsed) => Argon2::default()
                .verify_password(attempt.as_bytes(), &parsed)
                .is_ok(),
            Err(_) => false,
        }
    }

    /// Constant-time equality (not usually needed for Argon2 but useful for testing)
    pub fn eq_secure(&self, other: &ValidatedPassword) -> bool {
        self.hashed
            .expose_secret()
            .as_bytes()
            .ct_eq(other.hashed.expose_secret().as_bytes())
            .into()
    }

    /// For internal use only — e.g., storing to a DB
    pub fn as_hashed(&self) -> &str {
        self.hashed.expose_secret()
    }
}

/// Prevent secret leakage through `Debug`
impl core::fmt::Debug for ValidatedPassword {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "ValidatedPassword(**REDACTED**)")
    }
}

/// Prevent accidental serialization
impl Serialize for ValidatedPassword {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.serialize_str("**REDACTED**")
    }
}

impl<'de> Deserialize<'de> for ValidatedPassword {
    fn deserialize<D>(_deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        Err(serde::de::Error::custom(
            "Deserialization of ValidatedPassword is not allowed",
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn test_json_codec_roundtrip() {
        let codec = JsonCodec;
        let mut map = ConfigMap::new();
        map.insert("key".into(), "value".into());

        let encoded = codec.encode(&map).unwrap();
        let decoded = codec.decode(&encoded).unwrap();

        assert_eq!(map, decoded);
    }

    #[test]
    fn test_validated_password_build_verify() {
        let password = "StrongPass1$".to_string();
        let salt = "longsufficientlysalt".to_string();

        let validated = ValidatedPassword::build(password.clone(), salt).unwrap();
        assert!(validated.verify(&password));
        assert!(!validated.verify("wrongpass"));
    }

    #[test]
    fn test_validated_password_eq_secure_false_for_different_hashes() {
        let pw1 =
            ValidatedPassword::build("StrongPass1$".to_string(), "salt123456789012".to_string())
                .unwrap();
        let pw2 =
            ValidatedPassword::build("StrongPass1$".to_string(), "salt999999999999".to_string())
                .unwrap();
        assert!(!pw1.eq_secure(&pw2)); // different salts → different hashes
    }

    #[test]
    fn test_encrypted_pack_unpack_roundtrip() {
        let password = ValidatedPassword::build(
            "StrongPass1$".to_string(),
            "longsufficientlysalt".to_string(),
        )
        .unwrap();
        let enc = Encrypted::new(password);

        let original = b"my super secret data";
        let packed = enc.pack(original).unwrap();
        let unpacked = enc.unpack(&packed).unwrap();

        assert_eq!(original, &unpacked[..]);
    }

    #[test]
    fn test_config_serializer_save_and_load() {
        let password = ValidatedPassword::build(
            "StrongPass1$".to_string(),
            "longsufficientlysalt".to_string(),
        )
        .unwrap();
        let codec = JsonCodec;
        let enc = Encrypted::new(password);
        let serializer = ConfigSerializer::new(codec, enc);

        let mut map = ConfigMap::new();
        map.insert("api_key".to_string(), "1234567890".to_string());

        let dir = tempdir().unwrap();
        let path = dir.path().join("config.enc");

        serializer.save_to_path(&map, &path).unwrap();
        let restored = serializer.load_from_path(&path).unwrap();

        assert_eq!(map, restored);
    }

    #[test]
    fn test_validation_fails_on_weak_password() {
        let weak_password = "password".to_string(); // No uppercase, digit, or symbol
        let salt = "longsufficientlysalt".to_string();

        let result = ValidatedPassword::build(weak_password, salt);
        assert!(result.is_err());
    }

    #[test]
    fn test_unpack_fails_on_short_input() {
        let password = ValidatedPassword::build(
            "StrongPass1$".to_string(),
            "longsufficientlysalt".to_string(),
        )
        .unwrap();
        let enc = Encrypted::new(password);

        let short_input = vec![0u8; 10]; // too short for salt + nonce
        let result = enc.unpack(&short_input);
        assert!(result.is_err());
    }

    #[test]
    fn test_validated_password_debug_does_not_expose_secret() {
        let password = ValidatedPassword::build(
            "StrongPass1$".to_string(),
            "longsufficientlysalt".to_string(),
        )
        .unwrap();
        let debug_str = format!("{:?}", password);
        assert!(debug_str.contains("**REDACTED**"));
    }

    #[test]
    fn test_validated_password_serde_protected() {
        let password = ValidatedPassword::build(
            "StrongPass1$".to_string(),
            "longsufficientlysalt".to_string(),
        )
        .unwrap();
        let serialized = serde_json::to_string(&password).unwrap();
        assert_eq!(serialized, "\"**REDACTED**\"");

        let json = "\"irrelevant\"";
        let result: Result<ValidatedPassword, _> = serde_json::from_str(json);
        assert!(result.is_err());
    }
}
