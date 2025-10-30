use crate::config::paths::install_json;
use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::fs;
use std::io::Write;

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
    let s = fs::read_to_string(&path)?;
    Ok(Some(serde_json::from_str(&s)?))
}

#[tracing::instrument(skip_all)]
pub fn save(state: &Resume) -> Result<()> {
    let path = install_json();
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }

    // Atomic-ish write: write a temp file then rename.
    let tmp = path.with_extension("tmp");
    {
        let mut f = fs::File::create(&tmp)?;
        let json = serde_json::to_string_pretty(state)?;
        f.write_all(json.as_bytes())?;
        // Best-effort flush
        let _ = f.sync_all();
    }
    fs::rename(&tmp, &path)?;
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