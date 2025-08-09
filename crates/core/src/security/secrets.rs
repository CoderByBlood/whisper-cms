
use rand_core::{OsRng, RngCore};
use crate::types::Secrets;

pub fn generate() -> Secrets {
    let mut hmac_key = vec![0u8; 32];
    let mut csrf_salt = vec![0u8; 16];
    OsRng.fill_bytes(&mut hmac_key);
    OsRng.fill_bytes(&mut csrf_salt);
    Secrets { hmac_key, csrf_salt }
}
