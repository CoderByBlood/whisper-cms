
#[derive(Debug, Clone)]
pub struct Secrets {
    pub hmac_key: Vec<u8>,
    pub csrf_salt: Vec<u8>,
}
