use std::path::PathBuf;

#[tracing::instrument(skip_all)]
pub fn core_toml() -> PathBuf {
    PathBuf::from("config/core.toml")
}

#[tracing::instrument(skip_all)]
pub fn admin_toml() -> PathBuf {
    PathBuf::from("config/admin.toml")
}

#[tracing::instrument(skip_all)]
pub fn install_json() -> PathBuf {
    PathBuf::from("config/install.json")
}
