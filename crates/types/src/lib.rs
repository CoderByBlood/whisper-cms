use secrecy::SecretString;
use serde::{Deserialize, Serialize};
use url::Url;


#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub enum DbKind { Embedded, Remote }

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct InstallPlan {
    // Step 1: language
    pub language: String, // "en-US"

    // Step 2: database (two-DB design)
    pub db_kind: DbKind,              // Embedded or Remote
    pub split_content: bool,          // true => separate content DB

    // URLs that will go in core.toml (always full URLs, even for embedded)
    pub db_ops_url: Url,              // e.g., sqlite://data/ops.db
    pub db_content_url: Url,          // e.g., sqlite://data/content.db

    // Tokens (only in-memory; written to secrets files by steps; never serialized to core.toml)
    #[serde(skip, default)] pub db_ops_token: Option<SecretString>,
    #[serde(skip, default)] pub db_content_token: Option<SecretString>,

    // Step 3: site info
    pub site_name: String,
    pub base_url: Url,
    pub timezone: String,             // "UTC", "America/New_York", …

    // Admin (already in your plan; keep)
    #[serde(skip, default)] pub admin_password: Option<SecretString>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum InstallState {
    NeedsInstall,
    Partial { last_step: InstallStep },
    Complete,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum InstallStep {
    GenerateSecrets,
    WriteCoreConfigs,
    WriteAdminConfig,    
    WriteDbTokens,     // NEW
    MigrateOpsDb,      // NEW
    SeedBaseline,      // moved to ops-only seed
    FlipInstalledTrue,
}
