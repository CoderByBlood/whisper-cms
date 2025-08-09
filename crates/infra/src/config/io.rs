
use std::fs::{self, File};
use std::io::Write;
use std::path::Path;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use anyhow::Result;

pub fn write_atomic<P: AsRef<Path>>(path: P, data: &str) -> Result<()> {
    let path = path.as_ref();
    let tmp = path.with_extension("tmp");
    if let Some(parent) = path.parent() { fs::create_dir_all(parent)?; }
    { let mut f = File::create(&tmp)?; f.write_all(data.as_bytes())?; f.sync_all()?; }
    #[cfg(unix)]
    { fs::set_permissions(&tmp, fs::Permissions::from_mode(0o600))?; }
    fs::rename(&tmp, path)?; Ok(())
}
