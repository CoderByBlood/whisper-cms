use std::path::PathBuf;

#[tracing::instrument(skip_all)]
pub fn core_toml() -> PathBuf {
    site_root().join("config/core.toml")
}

#[tracing::instrument(skip_all)]
pub fn admin_toml() -> PathBuf {
    site_root().join("config/admin.toml")
}

#[tracing::instrument(skip_all)]
pub fn install_json() -> PathBuf {
    site_root().join("config/install.json")
}

// Make this pub(crate) if other modules need it too.
#[tracing::instrument(skip_all)]
fn site_root() -> PathBuf {
    std::env::var_os("WHISPERCMS_SITE_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
}