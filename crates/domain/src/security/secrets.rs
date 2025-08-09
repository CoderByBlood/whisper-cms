use crate::types::Secrets;
use rand_core::{OsRng, RngCore};

pub fn generate() -> Secrets {
    let mut hmac_key = vec![0u8; 32];
    let mut csrf_salt = vec![0u8; 16];
    OsRng.fill_bytes(&mut hmac_key);
    OsRng.fill_bytes(&mut csrf_salt);
    Secrets {
        hmac_key,
        csrf_salt,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn sizes_and_nonzero() {
        let s = generate();
        assert_eq!(s.hmac_key.len(), 32);
        assert_eq!(s.csrf_salt.len(), 16);
        assert!(s.hmac_key.iter().any(|&b| b != 0));
        assert!(s.csrf_salt.iter().any(|&b| b != 0));
    }
}
