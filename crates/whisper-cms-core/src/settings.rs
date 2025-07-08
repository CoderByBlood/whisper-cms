use serde::{Serialize, Deserialize};
use derive_more::From;
use argon2::Argon2;
use chacha20poly1305::{ChaCha20Poly1305, Key, Nonce};
use chacha20poly1305::aead::{Aead, KeyInit};
use rand::random;
use std::fs::File;
use std::io::{Read, Write};
use bincode::{self, config, Encode, Decode};

#[derive(Serialize, Deserialize, Encode, Decode, Debug, PartialEq, From)]
pub struct Settings {
    pub output: String,
    pub pghost: String,
    pub pgport: u16,
    pub pguser: String,
    pub pgpassword: String,
    pub pgdatabase: String,
}

impl Settings {
    /// Derive a 32-byte encryption key from the password and salt using Argon2
    fn derive_key(password: &str, salt: &[u8]) -> [u8; 32] {
        let argon2 = Argon2::default();
        let mut key = [0u8; 32];
        argon2
            .hash_password_into(password.as_bytes(), salt, &mut key)
            .expect("Argon2 hashing failed");
        key
    }

    /// Write encrypted Settings to the given file
    pub fn write_encrypted(&self, password: &str) -> std::io::Result<()> {
        // Random salt and nonce
        let salt: [u8; 16] = random();
        let nonce_bytes: [u8; 12] = random();
        let nonce = Nonce::from_slice(&nonce_bytes);

        // Derive key
        let key_bytes = Self::derive_key(password, &salt);
        let key = Key::from_slice(&key_bytes);
        let cipher = ChaCha20Poly1305::new(key);

        // Serialize
        let bin_config = config::standard();
        let serialized = bincode::encode_to_vec(self, bin_config)
            .expect("Serialization failed");

        // Encrypt
        let ciphertext = cipher.encrypt(nonce, serialized.as_ref())
            .expect("Encryption failed");

        // Write: [ salt (16) | nonce (12) | ciphertext (...)]
        let mut file = File::create(&self.output)?;
        file.write_all(&salt)?;
        file.write_all(&nonce_bytes)?;
        file.write_all(&ciphertext)?;
        Ok(())
    }

    /// Read encrypted Settings from the given file
    pub fn read_encrypted(password: &str, path: &str) -> std::io::Result<Self> {
        let mut file = File::open(path)?;
        let mut buffer = Vec::new();
        file.read_to_end(&mut buffer)?;

        if buffer.len() < 28 {
            return Err(std::io::Error::new(std::io::ErrorKind::InvalidData, "File too short"));
        }

        // Extract salt, nonce, ciphertext
        let salt = &buffer[0..16];
        let nonce_bytes = &buffer[16..28];
        let ciphertext = &buffer[28..];
        let nonce = Nonce::from_slice(nonce_bytes);

        // Derive key
        let key_bytes = Self::derive_key(password, salt);
        let key = Key::from_slice(&key_bytes);
        let cipher = ChaCha20Poly1305::new(key);

        // Decrypt
        let plaintext = cipher.decrypt(nonce, ciphertext.as_ref())
            .map_err(|_| std::io::Error::new(std::io::ErrorKind::InvalidData, "Decryption failed"))?;

        // Deserialize
        let bin_config = config::standard();
        let (settings, _len): (Settings, usize) = bincode::decode_from_slice(&plaintext, bin_config)
            .map_err(|_| std::io::Error::new(std::io::ErrorKind::InvalidData, "Deserialization failed"))?;

        Ok(settings)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn make_test_settings(path: &str) -> Settings {
        Settings {
            output: path.to_string(),
            pghost: "localhost".to_string(),
            pgport: 5432,
            pguser: "test_user".to_string(),
            pgpassword: "test_pgpassword".to_string(),
            pgdatabase: "test_db".to_string(),
        }
    }

    #[test]
    fn test_encrypt_and_decrypt_round_trip() {
        let path: &'static str = "test_config.enc";
        let password = "correct_password";
        let settings = make_test_settings(path);

        // Write
        settings.write_encrypted(password).expect("Failed to write encrypted");

        // Read back
        let loaded = Settings::read_encrypted(password, path).expect("Failed to read encrypted");

        // Assert equality
        assert_eq!(settings.output, loaded.output);
        assert_eq!(settings.pghost, loaded.pghost);
        assert_eq!(settings.pgport, loaded.pgport);
        assert_eq!(settings.pguser, loaded.pguser);
        assert_eq!(settings.pgpassword, loaded.pgpassword);
        assert_eq!(settings.pgdatabase, loaded.pgdatabase);

        // Clean up
        fs::remove_file(path).unwrap();
    }

    #[test]
    fn test_wrong_password_fails() {
        let correct_password = "correct_password";
        let wrong_password = "wrong_password";
        let path = "test_config_wrong_password.enc";
        let settings = make_test_settings(path);

        // Write with correct password
        settings.write_encrypted(correct_password).expect("Failed to write encrypted");

        // Try to read with wrong password
        let result = Settings::read_encrypted(wrong_password, path);

        assert!(result.is_err(), "Decryption should fail with wrong password");

        // Clean up
        fs::remove_file(path).unwrap();
    }

    #[test]
    fn test_file_too_short_errors() {
        let path = "test_short_file.enc";
        let mut file = File::create(path).unwrap();
        file.write_all(&[0u8; 10]).unwrap();
        file.flush().unwrap();

        let result = Settings::read_encrypted("any_password", path);
        assert!(result.is_err(), "Should error on too-short file");

        fs::remove_file(path).unwrap();
    }
}