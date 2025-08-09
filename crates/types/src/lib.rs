
use secrecy::SecretString;
use serde::{Deserialize, Serialize};
use url::Url;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InstallPlan {
    pub site_name: String,
    pub base_url: Url,
    pub timezone: String,
    #[serde(skip)]
    pub admin_password: Option<SecretString>,
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
    MigrateDb,
    SeedBaseline,
    WriteAdminConfig,
    FlipInstalledTrue,
}
