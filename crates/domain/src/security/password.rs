use argon2::{Argon2, PasswordHasher};
use password_hash::{rand_core::OsRng, PasswordHash, PasswordVerifier, SaltString};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum PasswordError {
    #[error("password too weak")]
    Weak,
    #[error(transparent)]
    Hash(#[from] password_hash::Error),
}

pub fn validate_policy(pw: &str) -> Result<(), PasswordError> {
    if pw.len() < 12 {
        return Err(PasswordError::Weak);
    }
    Ok(())
}

pub fn hash_password(pw: &str) -> Result<String, PasswordError> {
    let salt = SaltString::generate(&mut OsRng);
    let argon2 = Argon2::default();
    Ok(argon2.hash_password(pw.as_bytes(), &salt)?.to_string())
}

#[allow(dead_code)]
pub fn verify_password(pw: &str, hash: &str) -> Result<bool, PasswordError> {
    let parsed = PasswordHash::new(hash)?;
    Ok(Argon2::default()
        .verify_password(pw.as_bytes(), &parsed)
        .is_ok())
}
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn policy_rejects_short() {
        assert!(matches!(validate_policy("short"), Err(PasswordError::Weak)));
    }

    #[test]
    fn hash_and_verify_roundtrip() {
        validate_policy("averystrongpassword").unwrap();
        let h = hash_password("averystrongpassword").unwrap();
        assert!(verify_password("averystrongpassword", &h).unwrap());
        assert!(!verify_password("wrong", &h).unwrap());
    }
}
