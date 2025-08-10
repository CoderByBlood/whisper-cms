use crate::config::paths::install_json;
use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Resume {
    pub last_step: Option<String>,
    pub started_at: String,
    pub plan_fingerprint: String,
}

#[tracing::instrument(skip_all)]
pub fn load() -> Result<Option<Resume>> {
    let path = install_json();
    if !path.exists() {
        return Ok(None);
    }
    Ok(Some(serde_json::from_str(&fs::read_to_string(path)?)?))
}

#[tracing::instrument(skip_all)]
pub fn save(state: &Resume) -> Result<()> {
    std::fs::create_dir_all(PathBuf::from("config"))?;

    fs::write(install_json(), serde_json::to_string_pretty(state)?)?;
    Ok(())
}

#[tracing::instrument(skip_all)]
pub fn clear() -> Result<()> {
    let path = install_json();
    if path.exists() {
        fs::remove_file(path)?;
    }
    Ok(())
}
