// crates/edge/src/fs/ext.rs

use adapt::runtime::error::RuntimeError;
use adapt::runtime::plugin::PluginSpec;
use adapt::runtime::theme::ThemeSpec;
use serde::Deserialize;
use std::fs;
use std::path::{Path, PathBuf};

/// A mapping from a mount path (URL prefix) to a theme id,
/// plus the template root directory for that theme.
///
/// `template_root` is always `<theme_dir>/templates`
/// (or `<assets_dir>/templates` if you decide that later).
#[derive(Debug, Clone)]
pub struct ThemeBinding {
    pub mount_path: String,
    pub theme_id: String,
    pub template_root: PathBuf,
}

impl ThemeBinding {
    pub fn new(mount: impl Into<String>, theme: impl Into<String>, template_root: PathBuf) -> Self {
        Self {
            mount_path: mount.into(),
            theme_id: theme.into(),
            template_root,
        }
    }
}

/// A plugin discovered on disk.
#[derive(Debug, Clone)]
pub struct DiscoveredPlugin {
    pub dir: PathBuf,
    pub spec: PluginSpec,
}

/// A theme discovered on disk.
///
/// - `mount_path` is the URL mount (e.g. "/") from theme.toml
/// - `dir` is the theme root directory on disk
/// - `assets_dir` (if present) is `<dir>/assets`
/// - `spec` is the runtime ThemeSpec (id, name, mount_path, source)
#[derive(Debug, Clone)]
pub struct DiscoveredTheme {
    pub mount_path: String,
    pub dir: PathBuf,
    pub assets_dir: Option<PathBuf>,
    pub spec: ThemeSpec,
}

// ─────────────────────────────────────────────────────────────────────────────
// Manifest structs
// ─────────────────────────────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
struct PluginManifest {
    pub id: Option<String>,
    pub name: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ThemeManifest {
    pub mount: String,
    pub id: Option<String>,
    pub name: Option<String>,
}

// ─────────────────────────────────────────────────────────────────────────────
// Plugin discovery
// ─────────────────────────────────────────────────────────────────────────────

pub fn discover_plugins(root: impl AsRef<Path>) -> Result<Vec<DiscoveredPlugin>, RuntimeError> {
    let root = root.as_ref();

    if !root.exists() {
        return Ok(Vec::new());
    }

    let mut out = Vec::new();

    for entry in fs::read_dir(root)
        .map_err(|e| RuntimeError::Other(format!("failed to read plugin root {:?}: {e}", root)))?
    {
        let entry = entry
            .map_err(|e| RuntimeError::Other(format!("failed to read plugin dir entry: {e}")))?;

        let path = entry.path();
        if !path.is_dir() {
            continue;
        }

        let manifest_path = path.join("plugin.toml");
        if !manifest_path.exists() {
            continue; // not a plugin dir
        }

        let manifest_src = fs::read_to_string(&manifest_path).map_err(|e| {
            RuntimeError::Other(format!(
                "failed reading plugin manifest {:?}: {e}",
                manifest_path
            ))
        })?;

        let manifest: PluginManifest = toml::from_str(&manifest_src).map_err(|e| {
            RuntimeError::Other(format!(
                "failed parsing plugin manifest {:?}: {e}",
                manifest_path
            ))
        })?;

        let dir_name = path
            .file_name()
            .and_then(|x| x.to_str())
            .unwrap_or("unknown-plugin")
            .to_string();

        let id = manifest.id.unwrap_or_else(|| dir_name.clone());
        let name = manifest.name.unwrap_or_else(|| id.clone());
        let main = "plugin.js".to_string();
        let main_path = path.join(&main);

        let js_src = fs::read_to_string(&main_path).map_err(|e| {
            RuntimeError::Other(format!(
                "failed reading plugin JS file {:?}: {e}",
                main_path
            ))
        })?;

        let spec = PluginSpec {
            id,
            name,
            source: js_src,
        };

        out.push(DiscoveredPlugin { dir: path, spec });
    }

    Ok(out)
}

// ─────────────────────────────────────────────────────────────────────────────
// Theme discovery
// ─────────────────────────────────────────────────────────────────────────────

pub fn discover_themes(root: impl AsRef<Path>) -> Result<Vec<DiscoveredTheme>, RuntimeError> {
    let root = root.as_ref();

    if !root.exists() {
        return Ok(Vec::new());
    }

    let mut out = Vec::new();

    for entry in fs::read_dir(root)
        .map_err(|e| RuntimeError::Other(format!("failed to read themes root {:?}: {e}", root)))?
    {
        let entry = entry
            .map_err(|e| RuntimeError::Other(format!("failed to read theme dir entry: {e}")))?;

        let path = entry.path();
        if !path.is_dir() {
            continue;
        }

        let manifest_path = path.join("theme.toml");
        if !manifest_path.exists() {
            continue; // not a theme
        }

        let manifest_src = fs::read_to_string(&manifest_path).map_err(|e| {
            RuntimeError::Other(format!(
                "failed reading theme manifest {:?}: {e}",
                manifest_path
            ))
        })?;

        let manifest: ThemeManifest = toml::from_str(&manifest_src).map_err(|e| {
            RuntimeError::Other(format!(
                "failed parsing theme manifest {:?}: {e}",
                manifest_path
            ))
        })?;

        let dir_name = path
            .file_name()
            .and_then(|x| x.to_str())
            .unwrap_or("unknown-theme")
            .to_string();

        let id = manifest.id.unwrap_or_else(|| dir_name.clone());
        let name = manifest.name.unwrap_or_else(|| id.clone());
        let main = "theme.js".to_string();
        let main_path = path.join(&main);

        let js_src = fs::read_to_string(&main_path).map_err(|e| {
            RuntimeError::Other(format!("failed reading theme JS file {:?}: {e}", main_path))
        })?;

        let assets_dir = match path.join("assets/") {
            p if p.exists() => Some(p),
            _ => None,
        };

        let spec = ThemeSpec {
            id,
            name,
            mount_path: manifest.mount.clone(),
            source: js_src,
        };

        out.push(DiscoveredTheme {
            mount_path: manifest.mount,
            dir: path,
            assets_dir,
            spec,
        });
    }

    Ok(out)
}

// ─────────────────────────────────────────────────────────────────────────────
// Conversions
// ─────────────────────────────────────────────────────────────────────────────

impl From<&DiscoveredTheme> for ThemeBinding {
    fn from(t: &DiscoveredTheme) -> Self {
        // Template root: `<theme_dir>/templates`
        let template_root = t.dir.join("templates");

        ThemeBinding {
            mount_path: t.mount_path.clone(),
            theme_id: t.spec.id.clone(),
            template_root,
        }
    }
}
