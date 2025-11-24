// crates/edge/src/db/resolver.rs

use domain::content::ContentKind;
use domain::content::ResolvedContent;
use gray_matter::engine::YAML;
use gray_matter::Matter;
use http::{HeaderMap, Method};
use serde_json::{json, Map as JsonMap, Value as Json};
use serve::ctx::http::{ContextError, RequestContext};
use serve::resolver::ContentResolver;
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::LazyLock;
use tracing::debug;

/// Filesystem-based content resolver.
///
/// Responsibilities:
/// - Map request paths like `/`, `/about`, `/about.html` to concrete files
///   under `root`.
/// - Derive a `ContentKind` from extension.
/// - Parse front matter (YAML / TOML / JSON) from the resolved file.
/// - On any failure, fall back to empty front matter and a "no body" path.
#[derive(Clone, Debug)]
pub struct FsContentResolver {
    root: PathBuf,
}

impl FsContentResolver {
    pub fn new<P: AsRef<Path>>(root: P) -> Self {
        Self {
            root: root.as_ref().to_path_buf(),
        }
    }

    /// Given a request path like "/", "/about", "/about.html", normalize it to
    /// a concrete on-disk file path under `root` and detect `ContentKind`.
    ///
    /// Resolution order:
    ///   1. Treat `path` as-is relative to root (if non-empty).
    ///   2. If no extension, try appending ".html".
    ///   3. If empty or ends with "/", try appending "index.html".
    #[tracing::instrument(skip_all)]
    fn normalize_path(&self, path: &str) -> Option<(PathBuf, ContentKind)> {
        let trimmed = path.trim_start_matches('/');

        let mut candidates: Vec<(PathBuf, Option<ContentKind>)> = Vec::new();

        // 1: exact relative path
        if !trimmed.is_empty() {
            candidates.push((self.root.join(trimmed), None));
        }

        // 2: no extension → ".html"
        if !trimmed.is_empty() && !trimmed.contains('.') {
            candidates.push((
                self.root.join(format!("{trimmed}.html")),
                Some(ContentKind::Html),
            ));
        }

        // 3: "/" or ends with "/" → "index.html"
        if trimmed.is_empty() || path.ends_with('/') {
            let base = if trimmed.is_empty() {
                "index.html".to_string()
            } else {
                format!("{trimmed}index.html")
            };
            candidates.push((self.root.join(base), Some(ContentKind::Html)));
        }

        for (candidate, kind_hint) in candidates {
            if candidate.is_file() {
                let ck =
                    kind_hint.unwrap_or_else(|| Self::detect_content_kind_from_ext(&candidate));
                debug!("Resolved path '{}' → {:?}", path, candidate);
                return Some((candidate, ck));
            }
        }

        None
    }

    #[tracing::instrument(skip_all)]
    fn detect_content_kind_from_ext(path: &Path) -> ContentKind {
        if let Some(ext) = path.extension().and_then(|s| s.to_str()) {
            match ext.to_ascii_lowercase().as_str() {
                "html" | "htm" => ContentKind::Html,
                "json" => ContentKind::Json,
                _ => ContentKind::Asset,
            }
        } else {
            ContentKind::Asset
        }
    }

    /// Parse front matter from a file using the same rules as the doc pipeline:
    /// 1. YAML (via gray_matter)
    /// 2. TOML front matter delimited by +++
    /// 3. Pure JSON file starting with '{'
    ///
    /// Returns Some(front_matter_json) or None if no FM is found or parsing fails.
    #[tracing::instrument(skip_all)]
    fn load_front_matter(&self, path: &Path) -> Option<Json> {
        let full = match fs::read_to_string(path) {
            Ok(s) => s,
            Err(e) => {
                debug!("Failed to read {:?} for front matter: {}", path, e);
                return None;
            }
        };

        let mut fm_json: Option<Json> = None;

        // 1. YAML via gray_matter
        {
            let matter: Matter<YAML> = Matter::new();
            if let Ok(parsed) = matter.parse::<Json>(&full) {
                if let Some(data) = parsed.data {
                    fm_json = Some(data);
                }
            }
        }

        // 2. TOML front matter (only if YAML found nothing)
        if fm_json.is_none() {
            let trimmed = full.trim_start_matches('\u{feff}');

            if trimmed.starts_with("+++") {
                // remove leading delimiter
                let after = &trimmed[3..];
                let after = after
                    .strip_prefix('\n')
                    .or_else(|| after.strip_prefix("\r\n"))
                    .unwrap_or(after);

                // find closing delimiter
                if let Some(end_idx) = after.find("\n+++") {
                    let fm_src = &after[..end_idx];
                    match toml::from_str::<toml::Value>(fm_src) {
                        Ok(toml_val) => {
                            if let Ok(json) = serde_json::to_value(toml_val) {
                                fm_json = Some(json);
                            }
                        }
                        Err(e) => {
                            debug!("TOML front matter parse error in {:?}: {}", path, e);
                        }
                    }
                }
            }
        }

        // 3. Pure JSON file (only if YAML/TOML found nothing)
        if fm_json.is_none() {
            let trimmed = full.trim_start_matches('\u{feff}').trim_start();
            if trimmed.starts_with('{') {
                match serde_json::from_str::<Json>(trimmed) {
                    Ok(value) => {
                        fm_json = Some(value);
                    }
                    Err(e) => {
                        debug!("JSON front matter parse error in {:?}: {}", path, e);
                    }
                }
            }
        }

        fm_json
    }

    /// Internal helper used by both the trait impl and any direct calls.
    pub fn resolve_fs(
        &self,
        path: &str,
        _method: &Method,
    ) -> Result<ResolvedContent, ContextError> {
        // Step 2–4: resolve to a concrete file, if possible.
        if let Some((body_path, content_kind)) = self.normalize_path(path) {
            let front_matter = self
                .load_front_matter(&body_path)
                .unwrap_or_else(|| json!({}));

            Ok(ResolvedContent {
                content_kind,
                front_matter,
                body_path,
            })
        } else {
            // Step 5: no match – still run plugins/themes with empty FM.
            Ok(ResolvedContent {
                content_kind: ContentKind::Asset,
                front_matter: json!({}),
                body_path: PathBuf::new(),
            })
        }
    }
}

// ─────────────────────────────────────────────
// Implement the adapt-side ContentResolver trait
// ─────────────────────────────────────────────

impl ContentResolver for FsContentResolver {
    #[tracing::instrument(skip_all)]
    fn resolve(&self, path: &str, method: &Method) -> Result<ResolvedContent, ContextError> {
        self.resolve_fs(path, method)
    }
}

// ─────────────────────────────────────────────
// (Optional) If you're *also* doing function-pointer
// injection into adapt, that wiring would live here,
// but it doesn't affect this trait error.
// ─────────────────────────────────────────────

static GLOBAL_RESOLVER: LazyLock<std::sync::Mutex<Option<FsContentResolver>>> =
    LazyLock::new(|| std::sync::Mutex::new(None));

pub fn init_global_fs_resolver(resolver: FsContentResolver) {
    *GLOBAL_RESOLVER.lock().unwrap() = Some(resolver);
}

pub fn edge_resolve(path: &str, method: &Method) -> Result<ResolvedContent, ContextError> {
    let guard = GLOBAL_RESOLVER.lock().unwrap();
    let resolver = guard
        .as_ref()
        .expect("init_global_fs_resolver must be called before edge_resolve");
    resolver.resolve_fs(path, method)
}

pub fn edge_build_request_context(
    path: String,
    method: Method,
    headers: HeaderMap,
    query_params: HashMap<String, String>,
    resolved: ResolvedContent,
) -> RequestContext {
    // Normalize headers
    let mut hdr_obj = JsonMap::new();
    for (name, value) in headers.iter() {
        let key = canonicalize_header_name(name.as_str());
        hdr_obj.insert(key, json!(value.to_str().unwrap_or("")));
    }

    // Serialize params
    let mut qp_obj = JsonMap::new();
    for (k, v) in query_params {
        qp_obj.insert(k, Json::String(v));
    }

    RequestContext::builder()
        .path(Json::String(path))
        .method(Json::String(method.to_string()))
        .headers(Json::Object(hdr_obj))
        .params(Json::Object(qp_obj))
        .content_meta(resolved.front_matter)
        .theme_config(json!({}))
        .plugin_configs(HashMap::new())
        .build()
}

fn canonicalize_header_name(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    let mut upper_next = true;

    for ch in raw.chars() {
        if ch == '-' {
            out.push('-');
            upper_next = true;
        } else if upper_next {
            out.extend(ch.to_uppercase());
            upper_next = false;
        } else {
            out.extend(ch.to_lowercase());
        }
    }

    out
}
