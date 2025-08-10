
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct AdminConfig {
    pub admin_identity: String,
    pub password_hash: String,
    pub created_at: String,
}
