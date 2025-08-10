use anyhow::{Context, Result};
use std::fs::{self, File};
use std::io::{Read, Write};
use std::path::Path;

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

/// Atomic write: write to .tmp, fsync, set 0600 (unix), rename.
#[tracing::instrument(skip_all)]
pub fn write_atomic<P: AsRef<Path>>(path: P, data: &[u8]) -> Result<()> {
    let path = path.as_ref();
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).with_context(|| format!("create dir {}", parent.display()))?;
    }
    let tmp = path.with_extension("tmp");

    {
        let mut f = File::create(&tmp).with_context(|| format!("create {}", tmp.display()))?;
        f.write_all(data)
            .with_context(|| format!("write {}", tmp.display()))?;
        f.sync_all()
            .with_context(|| format!("fsync {}", tmp.display()))?;
    }

    #[cfg(unix)]
    {
        fs::set_permissions(&tmp, fs::Permissions::from_mode(0o600))
            .with_context(|| format!("chmod 600 {}", tmp.display()))?;
    }

    fs::rename(&tmp, path)
        .with_context(|| format!("rename {} -> {}", tmp.display(), path.display()))?;

    Ok(())
}

/// Convenience: write TOML using `write_atomic`.
#[tracing::instrument(skip_all)]
pub fn write_toml<P: AsRef<Path>, T: serde::Serialize>(path: P, value: &T) -> Result<()> {
    let s = toml::to_string_pretty(value)?;
    write_atomic(path, s.as_bytes())
}

/// Read whole file into String (Ok(None) if missing).
#[tracing::instrument(skip_all)]
pub fn read_to_string_opt<P: AsRef<Path>>(path: P) -> Result<Option<String>> {
    let path = path.as_ref();
    if !path.exists() {
        return Ok(None);
    }
    let mut s = String::new();
    File::open(path)
        .and_then(|mut f| f.read_to_string(&mut s))
        .with_context(|| format!("read {}", path.display()))?;
    Ok(Some(s))
}

/// Read TOML into type (Ok(None) if missing).
#[tracing::instrument(skip_all)]
pub fn read_toml_opt<P: AsRef<Path>, T: serde::de::DeserializeOwned>(path: P) -> Result<Option<T>> {
    if let Some(s) = read_to_string_opt(path)? {
        let v = toml::from_str(&s)?;
        Ok(Some(v))
    } else {
        Ok(None)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::{Deserialize, Serialize};
    use std::fs;
    use tempfile::tempdir;

    #[derive(Debug, Serialize, Deserialize, PartialEq)]
    struct Demo {
        a: u32,
        b: String,
    }

    #[test]
    fn atomic_write_and_read_toml() {
        let dir = tempdir().unwrap();
        let p = dir.path().join("config/core.toml");

        let v = Demo {
            a: 7,
            b: "ok".into(),
        };
        write_toml(&p, &v).unwrap();

        let got: Option<Demo> = read_toml_opt(&p).unwrap();
        assert_eq!(got, Some(v));

        // overwrite is fine
        let v2 = Demo {
            a: 8,
            b: "again".into(),
        };
        write_toml(&p, &v2).unwrap();
        let got2: Option<Demo> = read_toml_opt(&p).unwrap();
        assert_eq!(got2, Some(v2));

        // ensure file exists
        assert!(p.exists());
        #[cfg(unix)]
        {
            let mode = fs::metadata(&p).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o600);
        }
    }

    #[test]
    fn read_missing_is_none() {
        let dir = tempdir().unwrap();
        let p = dir.path().join("nope.toml");
        let x: Option<Demo> = read_toml_opt(&p).unwrap();
        assert!(x.is_none());
    }
}
