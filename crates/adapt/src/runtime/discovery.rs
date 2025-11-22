use std::fs;
use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::runtime::error::RuntimeError;
use crate::runtime::plugin::PluginSpec;
use crate::runtime::theme::ThemeSpec;

/// A mapping from a mount path (URL prefix) to a theme name.
///
/// The binding is resolved later in `bootstrap::RuntimeSet`.
#[derive(Debug, Clone)]
pub struct ThemeBinding {
    pub mount_path: String,
    pub theme_name: String,
}

impl ThemeBinding {
    pub fn new(mount: impl Into<String>, theme: impl Into<String>) -> Self {
        Self {
            mount_path: mount.into(),
            theme_name: theme.into(),
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
#[derive(Debug, Clone)]
pub struct DiscoveredTheme {
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
    pub main: Option<String>, // defaults to "plugin.js"
}

#[derive(Debug, Deserialize)]
struct ThemeManifest {
    pub id: Option<String>,
    pub name: Option<String>,
    pub main: Option<String>,       // defaults to "theme.js"
    pub assets_dir: Option<String>, // optional
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
        let main = manifest.main.unwrap_or_else(|| "plugin.js".to_string());
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
        let main = manifest.main.unwrap_or_else(|| "theme.js".to_string());
        let main_path = path.join(&main);

        let js_src = fs::read_to_string(&main_path).map_err(|e| {
            RuntimeError::Other(format!("failed reading theme JS file {:?}: {e}", main_path))
        })?;

        let assets_dir = manifest.assets_dir.map(|rel| path.join(rel));

        let spec = ThemeSpec {
            id,
            name,
            source: js_src,
        };

        out.push(DiscoveredTheme {
            dir: path,
            assets_dir,
            spec,
        });
    }

    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::PathBuf;
    use uuid::Uuid;

    // Small helper to create a unique temp directory for each test.
    fn temp_dir(label: &str) -> PathBuf {
        let mut dir = std::env::temp_dir();
        dir.push(format!("whispercms_discovery_{label}_{}", Uuid::new_v4()));
        fs::create_dir_all(&dir).expect("create temp dir");
        dir
    }

    // ─────────────────────────────────────────────────────────────────────
    // ThemeBinding tests
    // ─────────────────────────────────────────────────────────────────────

    #[test]
    fn theme_binding_new_sets_fields() {
        let binding = ThemeBinding::new("/blog", "my-theme");

        assert_eq!(binding.mount_path, "/blog");
        assert_eq!(binding.theme_name, "my-theme");
    }

    #[test]
    fn theme_binding_new_accepts_owned_and_borrowed() {
        let mount = String::from("/docs");
        let theme = String::from("docs-theme");

        let binding = ThemeBinding::new(mount.clone(), theme.clone());

        assert_eq!(binding.mount_path, mount);
        assert_eq!(binding.theme_name, theme);
    }

    // ─────────────────────────────────────────────────────────────────────
    // discover_plugins tests
    // ─────────────────────────────────────────────────────────────────────

    #[test]
    fn discover_plugins_nonexistent_root_returns_empty_vec() {
        let mut root = std::env::temp_dir();
        root.push(format!(
            "whispercms_discovery_nonexistent_{}",
            Uuid::new_v4()
        ));
        // Ensure it does NOT exist
        if root.exists() {
            fs::remove_dir_all(&root).unwrap();
        }

        let result = discover_plugins(&root).expect("should not error on nonexistent root");
        assert!(
            result.is_empty(),
            "nonexistent root should yield empty plugin list"
        );
    }

    #[test]
    fn discover_plugins_empty_root_returns_empty_vec() {
        let root = temp_dir("plugins_empty_root");

        let result = discover_plugins(&root).expect("empty root should not error");
        assert!(
            result.is_empty(),
            "empty plugin root should yield empty list"
        );
    }

    #[test]
    fn discover_plugins_file_instead_of_dir_produces_error() {
        let root = temp_dir("plugins_file_root");
        let file_path = root.join("not_a_dir.txt");
        fs::write(&file_path, "hello").expect("write dummy file");

        let result = discover_plugins(&file_path);

        match result {
            Err(RuntimeError::Other(msg)) => {
                assert!(
                    msg.contains("failed to read plugin root"),
                    "expected error message to mention 'failed to read plugin root', got: {msg}"
                );
            }
            other => panic!(
                "expected RuntimeError::Other for file root, got: {:?}",
                other
            ),
        }
    }

    #[test]
    fn discover_plugins_skips_entries_without_manifest() {
        let root = temp_dir("plugins_skip_no_manifest");

        // A directory without plugin.toml
        let no_manifest_dir = root.join("not_a_plugin");
        fs::create_dir_all(&no_manifest_dir).expect("create no_manifest_dir");

        // A plain file in the root
        let file = root.join("README.txt");
        fs::write(&file, "not relevant").expect("write file");

        let result = discover_plugins(&root).expect("discover_plugins should succeed");
        assert!(
            result.is_empty(),
            "directories without plugin.toml should be skipped"
        );
    }

    #[test]
    fn discover_plugins_invalid_manifest_toml_returns_error() {
        let root = temp_dir("plugins_invalid_manifest");
        let plugin_dir = root.join("plugin1");
        fs::create_dir_all(&plugin_dir).expect("create plugin dir");

        let manifest_path = plugin_dir.join("plugin.toml");
        fs::write(&manifest_path, "this is not valid toml = ==").expect("write invalid toml");

        let result = discover_plugins(&root);

        match result {
            Err(RuntimeError::Other(msg)) => {
                assert!(
                    msg.contains("failed parsing plugin manifest"),
                    "expected parsing error message, got: {msg}"
                );
            }
            other => panic!(
                "expected RuntimeError::Other from invalid manifest, got: {:?}",
                other
            ),
        }
    }

    #[test]
    fn discover_plugins_missing_js_file_returns_error() {
        let root = temp_dir("plugins_missing_js");
        let plugin_dir = root.join("plugin1");
        fs::create_dir_all(&plugin_dir).expect("create plugin dir");

        let manifest_path = plugin_dir.join("plugin.toml");
        // Valid TOML, but main points to non-existent file.
        fs::write(
            &manifest_path,
            r#"
                id = "p1"
                name = "Plugin One"
                main = "missing.js"
            "#,
        )
        .expect("write manifest");

        let result = discover_plugins(&root);

        match result {
            Err(RuntimeError::Other(msg)) => {
                assert!(
                    msg.contains("failed reading plugin JS file"),
                    "expected JS read error message, got: {msg}"
                );
            }
            other => panic!(
                "expected RuntimeError::Other from missing JS, got: {:?}",
                other
            ),
        }
    }

    #[test]
    fn discover_plugins_uses_defaults_when_manifest_fields_missing() {
        let root = temp_dir("plugins_defaults");
        let plugin_dir = root.join("my_plugin");
        fs::create_dir_all(&plugin_dir).expect("create plugin dir");

        let manifest_path = plugin_dir.join("plugin.toml");
        // Empty TOML is valid; all fields will be None.
        fs::write(&manifest_path, "").expect("write empty manifest");

        let js_path = plugin_dir.join("plugin.js");
        let js_source = "console.log('hello plugin');";
        fs::write(&js_path, js_source).expect("write plugin.js");

        let result = discover_plugins(&root).expect("discover_plugins should succeed");
        assert_eq!(result.len(), 1, "should discover exactly one plugin");

        let discovered = &result[0];
        assert_eq!(discovered.dir, plugin_dir);

        // When id and name are missing, id defaults to dir name, name defaults to id.
        let dir_name = plugin_dir
            .file_name()
            .unwrap()
            .to_str()
            .unwrap()
            .to_string();
        assert_eq!(discovered.spec.id, dir_name);
        assert_eq!(discovered.spec.name, discovered.spec.id);
        assert_eq!(discovered.spec.source, js_source);
    }

    #[test]
    fn discover_plugins_manifest_overrides_id_name_and_main() {
        let root = temp_dir("plugins_manifest_overrides");
        let plugin_dir = root.join("my_plugin");
        fs::create_dir_all(&plugin_dir).expect("create plugin dir");

        let manifest_path = plugin_dir.join("plugin.toml");
        fs::write(
            &manifest_path,
            r#"
                id = "plugin-123"
                name = "My Plugin"
                main = "src/main.js"
            "#,
        )
        .expect("write manifest");

        let src_dir = plugin_dir.join("src");
        fs::create_dir_all(&src_dir).expect("create src dir");
        let js_path = src_dir.join("main.js");
        let js_source = "console.log('main');";
        fs::write(&js_path, js_source).expect("write main.js");

        let result = discover_plugins(&root).expect("discover_plugins should succeed");
        assert_eq!(result.len(), 1, "should discover exactly one plugin");

        let discovered = &result[0];
        assert_eq!(discovered.spec.id, "plugin-123");
        assert_eq!(discovered.spec.name, "My Plugin");
        assert_eq!(discovered.spec.source, js_source);
    }

    #[test]
    fn discover_plugins_multiple_plugins_are_discovered() {
        let root = temp_dir("plugins_multiple");

        // Plugin A
        let dir_a = root.join("plugin_a");
        fs::create_dir_all(&dir_a).expect("create plugin_a");
        fs::write(dir_a.join("plugin.toml"), "").expect("write manifest A");
        fs::write(dir_a.join("plugin.js"), "console.log('A');").expect("write js A");

        // Plugin B
        let dir_b = root.join("plugin_b");
        fs::create_dir_all(&dir_b).expect("create plugin_b");
        fs::write(
            dir_b.join("plugin.toml"),
            r#"
                id = "plugin-b-id"
                name = "Plugin B"
            "#,
        )
        .expect("write manifest B");
        fs::write(dir_b.join("plugin.js"), "console.log('B');").expect("write js B");

        let result = discover_plugins(&root).expect("discover_plugins should succeed");

        assert_eq!(result.len(), 2, "should discover two plugins");

        // Order is not guaranteed; just assert that both sources are present.
        let mut sources: Vec<_> = result.iter().map(|p| p.spec.source.as_str()).collect();
        sources.sort(); // "console.log('A');" < "console.log('B');"

        assert_eq!(
            sources,
            vec!["console.log('A');", "console.log('B');"],
            "discovered sources should contain A and B"
        );
    }

    // ─────────────────────────────────────────────────────────────────────
    // discover_themes tests
    // ─────────────────────────────────────────────────────────────────────

    #[test]
    fn discover_themes_nonexistent_root_returns_empty_vec() {
        let mut root = std::env::temp_dir();
        root.push(format!(
            "whispercms_discovery_themes_nonexistent_{}",
            Uuid::new_v4()
        ));
        if root.exists() {
            fs::remove_dir_all(&root).unwrap();
        }

        let result = discover_themes(&root).expect("should not error on nonexistent root");
        assert!(
            result.is_empty(),
            "nonexistent themes root should yield empty list"
        );
    }

    #[test]
    fn discover_themes_empty_root_returns_empty_vec() {
        let root = temp_dir("themes_empty_root");

        let result = discover_themes(&root).expect("empty themes root should not error");
        assert!(
            result.is_empty(),
            "empty themes root should yield empty list"
        );
    }

    #[test]
    fn discover_themes_file_instead_of_dir_produces_error() {
        let root = temp_dir("themes_file_root");
        let file_path = root.join("not_a_dir.txt");
        fs::write(&file_path, "hello").expect("write dummy file");

        let result = discover_themes(&file_path);

        match result {
            Err(RuntimeError::Other(msg)) => {
                assert!(
                    msg.contains("failed to read themes root"),
                    "expected error message to mention 'failed to read themes root', got: {msg}"
                );
            }
            other => panic!(
                "expected RuntimeError::Other for file root, got: {:?}",
                other
            ),
        }
    }

    #[test]
    fn discover_themes_skips_entries_without_manifest() {
        let root = temp_dir("themes_skip_no_manifest");

        let no_manifest_dir = root.join("not_a_theme");
        fs::create_dir_all(&no_manifest_dir).expect("create not_a_theme");

        let file = root.join("README.txt");
        fs::write(&file, "not relevant").expect("write file");

        let result = discover_themes(&root).expect("discover_themes should succeed");
        assert!(
            result.is_empty(),
            "directories without theme.toml should be skipped"
        );
    }

    #[test]
    fn discover_themes_invalid_manifest_toml_returns_error() {
        let root = temp_dir("themes_invalid_manifest");
        let theme_dir = root.join("theme1");
        fs::create_dir_all(&theme_dir).expect("create theme dir");

        let manifest_path = theme_dir.join("theme.toml");
        fs::write(&manifest_path, "this is not valid toml = ==").expect("write invalid toml");

        let result = discover_themes(&root);

        match result {
            Err(RuntimeError::Other(msg)) => {
                assert!(
                    msg.contains("failed parsing theme manifest"),
                    "expected parsing error message, got: {msg}"
                );
            }
            other => panic!(
                "expected RuntimeError::Other from invalid manifest, got: {:?}",
                other
            ),
        }
    }

    #[test]
    fn discover_themes_missing_js_file_returns_error() {
        let root = temp_dir("themes_missing_js");
        let theme_dir = root.join("theme1");
        fs::create_dir_all(&theme_dir).expect("create theme dir");

        let manifest_path = theme_dir.join("theme.toml");
        fs::write(
            &manifest_path,
            r#"
                id = "t1"
                name = "Theme One"
                main = "missing.js"
            "#,
        )
        .expect("write manifest");

        let result = discover_themes(&root);

        match result {
            Err(RuntimeError::Other(msg)) => {
                assert!(
                    msg.contains("failed reading theme JS file"),
                    "expected JS read error message, got: {msg}"
                );
            }
            other => panic!(
                "expected RuntimeError::Other from missing JS, got: {:?}",
                other
            ),
        }
    }

    #[test]
    fn discover_themes_uses_defaults_when_manifest_fields_missing() {
        let root = temp_dir("themes_defaults");
        let theme_dir = root.join("my_theme");
        fs::create_dir_all(&theme_dir).expect("create theme dir");

        let manifest_path = theme_dir.join("theme.toml");
        fs::write(&manifest_path, "").expect("write empty manifest");

        let js_path = theme_dir.join("theme.js");
        let js_source = "console.log('theme');";
        fs::write(&js_path, js_source).expect("write theme.js");

        let result = discover_themes(&root).expect("discover_themes should succeed");
        assert_eq!(result.len(), 1, "should discover exactly one theme");

        let discovered = &result[0];
        assert_eq!(discovered.dir, theme_dir);

        let dir_name = theme_dir.file_name().unwrap().to_str().unwrap().to_string();
        assert_eq!(discovered.spec.id, dir_name);
        assert_eq!(discovered.spec.name, discovered.spec.id);
        assert_eq!(discovered.spec.source, js_source);
        assert!(
            discovered.assets_dir.is_none(),
            "assets_dir should be None when not specified"
        );
    }

    #[test]
    fn discover_themes_manifest_overrides_id_name_main_and_assets_dir() {
        let root = temp_dir("themes_manifest_overrides");
        let theme_dir = root.join("my_theme");
        fs::create_dir_all(&theme_dir).expect("create theme dir");

        let manifest_path = theme_dir.join("theme.toml");
        fs::write(
            &manifest_path,
            r#"
                id = "theme-123"
                name = "My Theme"
                main = "src/theme_main.js"
                assets_dir = "public"
            "#,
        )
        .expect("write manifest");

        let src_dir = theme_dir.join("src");
        fs::create_dir_all(&src_dir).expect("create src dir");
        let js_path = src_dir.join("theme_main.js");
        let js_source = "console.log('theme main');";
        fs::write(&js_path, js_source).expect("write theme_main.js");

        let assets_dir = theme_dir.join("public");
        fs::create_dir_all(&assets_dir).expect("create assets dir");

        let result = discover_themes(&root).expect("discover_themes should succeed");
        assert_eq!(result.len(), 1, "should discover exactly one theme");

        let discovered = &result[0];
        assert_eq!(discovered.spec.id, "theme-123");
        assert_eq!(discovered.spec.name, "My Theme");
        assert_eq!(discovered.spec.source, js_source);
        assert_eq!(
            discovered.assets_dir.as_ref().unwrap(),
            &assets_dir,
            "assets_dir should be joined relative to the theme dir"
        );
    }

    #[test]
    fn discover_themes_multiple_themes_are_discovered() {
        let root = temp_dir("themes_multiple");

        // Theme A
        let dir_a = root.join("theme_a");
        fs::create_dir_all(&dir_a).expect("create theme_a");
        fs::write(dir_a.join("theme.toml"), "").expect("write manifest A");
        fs::write(dir_a.join("theme.js"), "console.log('A');").expect("write js A");

        // Theme B
        let dir_b = root.join("theme_b");
        fs::create_dir_all(&dir_b).expect("create theme_b");
        fs::write(
            dir_b.join("theme.toml"),
            r#"
                id = "theme-b-id"
                name = "Theme B"
            "#,
        )
        .expect("write manifest B");
        fs::write(dir_b.join("theme.js"), "console.log('B');").expect("write js B");

        let result = discover_themes(&root).expect("discover_themes should succeed");

        assert_eq!(result.len(), 2, "should discover two themes");

        let mut sources: Vec<_> = result.iter().map(|t| t.spec.source.as_str()).collect();
        sources.sort(); // "console.log('A');" < "console.log('B');"

        assert_eq!(
            sources,
            vec!["console.log('A');", "console.log('B');"],
            "discovered theme sources should contain A and B"
        );
    }
}
