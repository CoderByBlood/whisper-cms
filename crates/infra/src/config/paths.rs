use std::path::PathBuf;

pub fn core_toml() -> PathBuf {
    PathBuf::from("config/core.toml")
}
pub fn admin_toml() -> PathBuf {
    PathBuf::from("config/admin.toml")
}
pub fn install_json() -> PathBuf {
    PathBuf::from("config/install.json")
}
