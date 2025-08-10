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

#[allow(dead_code)]
#[tracing::instrument(skip_all)]
fn site_root() -> PathBuf {
    std::env::var_os("WHISPERCMS_SITE_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
}
