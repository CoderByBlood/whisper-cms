use crate::ctx::{AppCtx, AppError};
use std::path::Path;

type Result<T> = std::result::Result<T, AppError>;

// ---------------- Business logic layer ----------------

pub async fn start_command(ctx: &AppCtx) -> Result<()> {
    // Example: verify dir RW
    ensure_read_write(&ctx.root_dir())
    // Pretend to start server...}
}

// Same read/write validation logic from your original code
fn ensure_read_write(dir: &Path) -> Result<()> {
    use std::fs::{self, OpenOptions};

    fs::read_dir(dir).map_err(|e| AppError::Msg(format!("Directory not readable: {e}")))?;
    let probe = dir.join(".whispercms_perm_check");
    let f = OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(&probe)
        .map_err(|e| AppError::Msg(format!("Directory not writable: {e}")))?;
    drop(f);
    fs::remove_file(&probe).map_err(|e| AppError::Msg(format!("Cleanup failed: {e}")))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{fs, path::PathBuf};

    use tempfile::{tempdir, NamedTempFile};

    // ---- Test strategy ------------------------------------------------------
    //
    // - We test `ensure_read_write` directly for fine-grained behavior.
    // - For `start_command`, the only logic is calling `ensure_read_write(ctx.root_dir())`.
    //   Since `AppCtx`’s constructor isn’t defined here, we mirror the behavior with a tiny
    //   local helper `start_with_root()` that calls `ensure_read_write` on a provided path.
    //   This keeps tests deterministic and independent of how AppCtx is built.
    //
    // If you want true end-to-end tests against `start_command(&AppCtx)` specifically,
    // you can add a feature-gated test that constructs a real `AppCtx` (see the block at the end).

    async fn start_with_root(root: &Path) -> Result<()> {
        ensure_read_write(root)
    }

    // ---------------- Positive cases ----------------

    #[test]
    fn ensure_read_write_ok_on_empty_temp_dir() {
        let td = tempdir().expect("create temp dir");
        ensure_read_write(td.path()).expect("dir should be readable & writable");
    }

    #[tokio::test]
    async fn start_like_wrapper_ok() {
        let td = tempdir().expect("create temp dir");
        start_with_root(td.path())
            .await
            .expect("wrapper should pass on a normal dir");
    }

    // ---------------- Negative: nonexistent / wrong type ----------------

    #[test]
    fn ensure_read_write_fails_when_dir_missing() {
        let missing: PathBuf = if cfg!(windows) {
            // unlikely to exist on Windows
            r"C:\__definitely__\__not__\__here__".into()
        } else {
            "/definitely/not/here/__whispercms__".into()
        };
        let err = ensure_read_write(&missing).unwrap_err();
        let msg = format!("{err}");
        assert!(
            msg.to_lowercase().contains("not readable"),
            "expected 'not readable' error, got: {msg}"
        );
    }

    #[test]
    fn ensure_read_write_fails_when_path_is_file() {
        let f = NamedTempFile::new().expect("create temp file");
        let err = ensure_read_write(f.path()).unwrap_err();
        let msg = format!("{err}");
        // read_dir(file) fails; message should reflect unreadable directory
        assert!(
            msg.to_lowercase().contains("not readable"),
            "expected 'not readable' error for file path, got: {msg}"
        );
    }

    // ---------------- Negative: permission scenarios (Unix-only) -------------

    // Make directory unreadable: listing entries should fail.
    #[cfg(unix)]
    #[test]
    fn ensure_read_write_fails_when_unreadable() {
        use std::os::unix::fs::PermissionsExt;

        let td = tempdir().unwrap();
        let d = td.path().join("unreadable");
        fs::create_dir(&d).unwrap();

        // Remove read bits (keep execute so path resolution is possible)
        let mut p = fs::metadata(&d).unwrap().permissions();
        p.set_mode(0o311); // --x--x--x plus owner write (listing should fail)
        fs::set_permissions(&d, p).unwrap();

        let err = ensure_read_write(&d).unwrap_err();
        let msg = format!("{err}");
        assert!(
            msg.to_lowercase().contains("not readable"),
            "expected 'not readable', got: {msg}"
        );
    }

    // Make directory non-writable: creating probe file should fail.
    #[cfg(unix)]
    #[test]
    fn ensure_read_write_fails_when_not_writable() {
        use std::os::unix::fs::PermissionsExt;

        let td = tempdir().unwrap();
        let d = td.path().join("readonly");
        fs::create_dir(&d).unwrap();

        // Read+execute only (no write) for everyone
        let mut p = fs::metadata(&d).unwrap().permissions();
        p.set_mode(0o555);
        fs::set_permissions(&d, p).unwrap();

        let err = ensure_read_write(&d).unwrap_err();
        let msg = format!("{err}");
        assert!(
            msg.to_lowercase().contains("not writable"),
            "expected 'not writable', got: {msg}"
        );
    }

    // ---------------- Platform sanity (Windows) ------------------------------

    // On Windows, toggling directory writability reliably in unit tests requires ACL work.
    // We still validate the positive case explicitly.
    #[cfg(windows)]
    #[test]
    fn ensure_read_write_ok_on_windows_normal_dir() {
        let td = tempdir().unwrap();
        ensure_read_write(td.path()).expect("should be readable & writable");
    }
}
