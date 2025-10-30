use std::path::{Path, PathBuf};

#[derive(Clone, Debug)]
pub struct Paths {
    root: PathBuf,
}
impl Paths {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }
    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn core_toml(&self) -> PathBuf {
        self.root.join("config/core.toml")
    }
    pub fn admin_toml(&self) -> PathBuf {
        self.root.join("config/admin.toml")
    }
    pub fn install_json(&self) -> PathBuf {
        self.root.join("config/install.json")
    }
    pub fn secrets_dir(&self) -> PathBuf {
        self.root.join("secrets")
    }
    pub fn secrets_libsql_dir(&self) -> PathBuf {
        self.secrets_dir().join("libsql")
    }
    pub fn secrets_ops_token(&self) -> PathBuf {
        self.secrets_libsql_dir().join("ops_token")
    }
    pub fn secrets_content_token(&self) -> PathBuf {
        self.secrets_libsql_dir().join("content_token")
    }
    pub fn data_dir(&self) -> PathBuf {
        self.root.join("data")
    }
    pub fn data_ops_path(&self) -> PathBuf {
        self.data_dir().join("ops.db")
    }
    pub fn data_content_path(&self) -> PathBuf {
        self.data_dir().join("content.db")
    }
    pub fn schema_dir(&self) -> PathBuf {
        self.root.join("schema")
    }
    pub fn schema_ops_dir(&self) -> PathBuf {
        self.schema_dir().join("ops")
    }
}

tokio::task_local! {
    static TL_PATHS: Paths;
}

/// Run a future with `paths` set as the task-local root.
/// Child tasks spawned *inside* this scope inherit it.
pub fn with_paths<F, R>(paths: Paths, with: F) -> impl std::future::Future<Output = R>
where
    F: std::future::Future<Output = R>,
{
    TL_PATHS.scope(paths, with)
}

fn env_default_root() -> PathBuf {
    std::env::var_os("WHISPERCMS_SITE_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
}

fn current_paths() -> Paths {
    // If we’re inside a task with TL set, use it; otherwise fall back to env/".".
    TL_PATHS
        .try_with(|p| p.clone())
        .unwrap_or_else(|_| Paths::new(env_default_root()))
}

// Keep your existing free functions, but have them consult the task-local.
pub fn core_toml() -> PathBuf {
    current_paths().core_toml()
}
pub fn admin_toml() -> PathBuf {
    current_paths().admin_toml()
}
pub fn install_json() -> PathBuf {
    current_paths().install_json()
}
pub fn secrets_dir() -> PathBuf {
    current_paths().secrets_dir()
}
pub fn secrets_libsql_dir() -> PathBuf {
    current_paths().secrets_libsql_dir()
}
pub fn secrets_ops_token() -> PathBuf {
    current_paths().secrets_ops_token()
}
pub fn secrets_content_token() -> PathBuf {
    current_paths().secrets_content_token()
}
pub fn data_dir() -> PathBuf {
    current_paths().data_dir()
}
pub fn data_ops_path() -> PathBuf {
    current_paths().data_ops_path()
}
pub fn data_content_path() -> PathBuf {
    current_paths().data_content_path()
}
pub fn schema_dir() -> PathBuf {
    current_paths().schema_dir()
}
pub fn schema_ops_dir() -> PathBuf {
    current_paths().schema_ops_dir()
}
