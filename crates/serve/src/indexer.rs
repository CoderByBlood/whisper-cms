// crates/serve/src/indexer.rs

// This version is crate-agnostic: no `crate::db`, no `crate::fs`, no `adapt::`.
//
// It assumes:
//   - `domain::doc::Document` already has its I/O strategy injected globally
//     (via the LazyLock-based open_bytes/open_utf8 you added earlier).
//   - The caller (in `edge`) provides:
//       * a folder-scan starter (filesystem + debouncing, etc.)
//       * a front-matter indexer (IndexedJson / whatever)
//       * a content indexer (Tantivy / whatever)
//
// All those concrete implementations live in `edge` and are *injected* here
// via function pointers / closures.

use domain::doc::{BodyKind, Document, FmKind};
use futures::StreamExt;
use regex::Regex;
use serde_json::Value as Json;
use thiserror::Error;

use std::io;
use std::path::{Path, PathBuf};
use tokio::sync::mpsc;

/// Configuration for `start_folder_scan`.
#[derive(Debug, Clone)]
pub struct FolderScanConfig {
    /// Emit absolute paths (true) or paths relative to `root` (false).
    pub absolute: bool,
    /// Recurse into subdirectories.
    pub recursive: bool,
    /// Debounce window in milliseconds for coalescing duplicate paths.
    pub debounce_ms: u64,
    /// Canonicalize paths before emission.
    pub canonicalize_paths: bool,
    /// Capacity of the bounded output channel (kept here so the
    /// `edge`-side scan starter can decide how to size its channel).
    pub channel_capacity: usize,
    /// Optional regex to **allow** folders. If set, a directory is traversed
    /// if it or **any ancestor under `root`** matches.
    pub folder_re: Option<Regex>,
    /// Optional regex to **allow** files by name (basename).
    pub file_re: Option<Regex>,
}

impl Default for FolderScanConfig {
    fn default() -> Self {
        Self {
            absolute: true,
            recursive: true,
            debounce_ms: 64,
            canonicalize_paths: true,
            channel_capacity: 1024,
            folder_re: None,
            file_re: None,
        }
    }
}

// ─────────────────────────────────────────────
// Per-document processing context.
// (Now completely decoupled from edge/adapt.)
// ─────────────────────────────────────────────

pub struct DocContext {
    pub document: Document,
}

impl std::fmt::Debug for DocContext {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DocContext")
            .field("document", &self.document)
            .finish()
    }
}

// ─────────────────────────────────────────────
// Error type (crate-agnostic)
// ─────────────────────────────────────────────

#[derive(Debug, Error)]
pub enum DocContextError {
    #[error("I/O error: {0}")]
    Io(#[from] io::Error),

    #[error("front matter parse error: {0}")]
    FrontMatter(String),

    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),

    #[error("front-matter index error: {0}")]
    FrontMatterIndex(String),

    #[error("content index error: {0}")]
    ContentIndex(String),

    #[error("AsciiDoc conversion error: {0}")]
    AsciiDoc(String),

    #[error("reStructuredText conversion error: {0}")]
    ReStructuredText(String),

    #[error("Org-mode conversion error: {0}")]
    Org(String),
}

// ─────────────────────────────────────────────
// Dependency-injected function shapes
// (implemented in `edge`, injected here)
// ─────────────────────────────────────────────

/// Stop callback returned by the folder-scan starter.
///
/// It only needs to be Send so we can move it across threads if necessary.
/// It does *not* need to be Sync.
pub type ScanStopFn = Box<dyn FnOnce() + Send + 'static>;

/// Function type for starting a folder scan, implemented in `edge`.
///
/// - `root`: root directory to scan
/// - `cfg`: configuration for how to scan (debounce, recursion, filters...)
/// - returns: `(receiver, stop_callback)`
pub type StartFolderScanFn<ScanErr> = fn(
    root: &Path,
    cfg: &FolderScanConfig,
) -> Result<(mpsc::Receiver<PathBuf>, ScanStopFn), ScanErr>;

/// Function type for indexing front matter, implemented in `edge`.
///
/// `served_path` is the "normalized" path (e.g. `/posts/hello.html`).
pub type IndexFrontMatterFn<FmErr> = fn(served_path: &Path, fm: &Json) -> Result<(), FmErr>;

/// Function type for indexing rendered HTML content, implemented in `edge`.
///
/// `kind` is the detected `BodyKind` (Markdown, Html, etc.) in case the
/// indexer wants to treat different kinds differently.
pub type IndexBodyFn<BodyErr> =
    fn(served_path: &Path, html: &str, kind: BodyKind) -> Result<(), BodyErr>;

// ─────────────────────────────────────────────
// Comrak markdown options (GFM-ish defaults)
// ─────────────────────────────────────────────

fn default_markdown_options() -> comrak::Options<'static> {
    let mut options = comrak::Options::default();

    // GitHub-ish extensions
    options.extension.strikethrough = true;
    options.extension.table = true;
    options.extension.autolink = true;
    options.extension.tasklist = true;
    options.extension.footnotes = true;

    options
}

// ─────────────────────────────────────────────
// Helper: read & cache full UTF-8 contents
// ─────────────────────────────────────────────

/// Ensure `ctx.document.cache` is populated with the full UTF-8 contents.
///
/// - If `cache` is already Some, returns unchanged.
/// - Otherwise, reads from `utf8_stream` and sets `cache` via `with_cache`.
async fn read_document_utf8(mut ctx: DocContext) -> Result<DocContext, DocContextError> {
    if ctx.document.cache.is_some() {
        return Ok(ctx);
    }

    let mut text = String::new();
    let mut stream = ctx.document.utf8_stream();

    while let Some(chunk) = stream.next().await {
        text.push_str(&chunk?);
    }

    ctx.document = ctx.document.with_cache(text);
    Ok(ctx)
}

// ─────────────────────────────────────────────
// Helpers: AsciiDoc, RST, Org renderers
// ─────────────────────────────────────────────

fn render_asciidoc_with_asciidocr(src: &str, origin: &Path) -> Result<String, DocContextError> {
    use asciidocr::backends::htmls::render_htmlbook;
    use asciidocr::parser::Parser;
    use asciidocr::scanner::Scanner;

    let scanner = Scanner::new(src);
    let mut parser = Parser::new(origin.to_path_buf());

    let graph = parser
        .parse(scanner)
        .map_err(|e| DocContextError::AsciiDoc(e.to_string()))?;

    let html = render_htmlbook(&graph).map_err(|e| DocContextError::AsciiDoc(e.to_string()))?;

    Ok(html)
}

fn render_restructuredtext(src: &str) -> Result<String, DocContextError> {
    use rst_parser::parse_only as parse_rst;
    use rst_renderer::render_html as render_rst_html;

    let doc = parse_rst(src).map_err(|e| DocContextError::ReStructuredText(e.to_string()))?;

    let mut buf: Vec<u8> = Vec::new();

    render_rst_html(&doc, &mut buf, true)
        .map_err(|e| DocContextError::ReStructuredText(e.to_string()))?;

    let html =
        String::from_utf8(buf).map_err(|e| DocContextError::ReStructuredText(e.to_string()))?;

    Ok(html)
}

fn render_org_to_html(src: &str) -> Result<String, DocContextError> {
    use orgize::Org;

    let org = Org::parse(src);
    let mut buf: Vec<u8> = Vec::new();

    org.write_html(&mut buf)
        .map_err(|e| DocContextError::Org(e.to_string()))?;

    let html = String::from_utf8(buf).map_err(|e| DocContextError::Org(e.to_string()))?;

    Ok(html)
}

/// Infer BodyKind from a file extension.
fn infer_body_kind_from_ext(path: &Path) -> BodyKind {
    path.extension()
        .and_then(|s| s.to_str())
        .map(|ext| match ext.to_ascii_lowercase().as_str() {
            // Markdown (with mkd, mkdn)
            "md" | "markdown" | "mkd" | "mkdn" => BodyKind::Markdown,
            // AsciiDoc
            "adoc" | "asciidoc" => BodyKind::AsciiDoc,
            // HTML-like (with xhtml)
            "html" | "htm" | "xhtml" => BodyKind::Html,
            // ReStructuredText
            "rst" => BodyKind::ReStructuredText,
            // Org
            "org" => BodyKind::OrgMode,
            // Fallback
            _ => BodyKind::Plain,
        })
        .unwrap_or(BodyKind::Plain)
}

/// Map a source filesystem path to the “served” path:
///
/// - If the body is a *translated* type (Markdown, AsciiDoc, RST, Org),
///   we normalize to a `.html` extension.
/// - For HTML and Plain text, we keep the original extension.
///
/// This path is intended to be used as:
///   - the `id` for front-matter in your key-value index
///   - the key for content index (Tantivy or otherwise)
///   - the HTTP-visible “content path” in resolution.
fn serving_path_for_source(path: &Path) -> PathBuf {
    let kind = infer_body_kind_from_ext(path);
    match kind {
        BodyKind::Markdown
        | BodyKind::AsciiDoc
        | BodyKind::ReStructuredText
        | BodyKind::OrgMode => {
            let mut p = path.to_path_buf();
            p.set_extension("html");
            p
        }
        BodyKind::Html | BodyKind::Plain => path.to_path_buf(),
    }
}

// ─────────────────────────────────────────────
// Stage 1 — front matter → index callback
// ─────────────────────────────────────────────

/// Detect front matter (YAML → TOML → JSON), parse it, and invoke the
/// injected front-matter indexer.
///
/// Also:
/// - sets `FmKind`
/// - sets `cached_body` to the content *after* front matter
///   (or the full file if no front matter is found).
pub async fn upsert_front_matter_db<FmErr, FmIndexFn>(
    ctx: DocContext,
    index_front_matter: FmIndexFn,
) -> Result<DocContext, DocContextError>
where
    FmErr: std::fmt::Display,
    FmIndexFn: Fn(&Path, &Json) -> Result<(), FmErr>,
{
    use gray_matter::engine::YAML;
    use gray_matter::Matter;

    let mut ctx = read_document_utf8(ctx).await?;

    let full = ctx
        .document
        .cache
        .as_ref()
        .map(String::as_str)
        .unwrap_or("");

    let mut fm_json: Option<Json> = None;
    let mut fm_kind: Option<FmKind> = None;
    let mut body: Option<String> = None;

    // 1. YAML
    {
        let matter: Matter<YAML> = Matter::new();
        if let Ok(parsed) = matter.parse::<Json>(full) {
            if let Some(data) = parsed.data {
                fm_json = Some(data);
                fm_kind = Some(FmKind::Yaml);
                body = Some(parsed.content);
            }
        }
    }

    // 2. TOML (only if YAML found nothing) — uses `toml::from_str` instead of
    // gray_matter's TOML engine because that was too strict in your setup.
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
                        let json = serde_json::to_value(toml_val)
                            .map_err(|e| DocContextError::FrontMatter(e.to_string()))?;
                        fm_kind = Some(FmKind::Toml);
                        fm_json = Some(json);
                        body = Some(after[end_idx + 4..].trim_start().to_owned());
                    }
                    Err(e) => return Err(DocContextError::FrontMatter(e.to_string())),
                }
            }
        }
    }

    // 3. JSON front matter (only if YAML/TOML found nothing)
    if fm_json.is_none() {
        let trimmed = full.trim_start_matches('\u{feff}').trim_start();
        if trimmed.starts_with('{') {
            match serde_json::from_str::<Json>(trimmed) {
                Ok(value) => {
                    fm_json = Some(value);
                    fm_kind = Some(FmKind::Json);
                    // For "pure JSON front matter", treat body as empty for now.
                    body = Some(String::new());
                }
                Err(e) => return Err(DocContextError::FrontMatter(e.to_string())),
            }
        }
    }

    // If no FM was detected, we still want body = full file.
    if body.is_none() {
        body = Some(full.to_owned());
    }

    // Persist FM if we have it — using the *served* path as the id.
    if let Some(fm_json) = fm_json {
        let served_path = serving_path_for_source(&ctx.document.path);

        index_front_matter(&served_path, &fm_json)
            .map_err(|e| DocContextError::FrontMatterIndex(e.to_string()))?;
    }

    // Update FmKind if we detected one.
    if let Some(kind) = fm_kind {
        ctx.document = ctx.document.with_fm_kind(kind);
    }

    // Cache body-only text.
    if let Some(body_text) = body {
        ctx.document = ctx.document.with_body(body_text);
    }

    Ok(ctx)
}

// ─────────────────────────────────────────────
// Stage 2 — body → HTML → content index callback
// ─────────────────────────────────────────────

/// Detect body kind, render to HTML, and invoke the injected content-indexer.
///
/// Uses:
/// - Markdown: `comrak` with a GFM-ish `Options`
/// - AsciiDoc: `asciidocr`
/// - ReStructuredText: `rst_parser` + `rst_renderer`
/// - OrgMode: `orgize`
/// - Html / Plain: passthrough
///
/// Uses `cached_body` if present, otherwise `cache`.
pub async fn upsert_body_db<BodyErr, BodyIndexFn>(
    ctx: DocContext,
    index_body: BodyIndexFn,
) -> Result<DocContext, DocContextError>
where
    BodyErr: std::fmt::Display,
    BodyIndexFn: Fn(&Path, &str, BodyKind) -> Result<(), BodyErr>,
{
    let mut ctx = read_document_utf8(ctx).await?;

    // Detect body kind from explicit setting or extension.
    let body_kind = ctx
        .document
        .body_kind
        .unwrap_or_else(|| infer_body_kind_from_ext(&ctx.document.path));

    // Prefer cached_body (body only), fall back to full cache.
    let body_text = ctx
        .document
        .cached_body
        .as_deref()
        .or_else(|| ctx.document.cache.as_deref())
        .unwrap_or("");

    let html = match body_kind {
        BodyKind::Html | BodyKind::Plain => body_text.to_owned(),

        BodyKind::Markdown => {
            let options = default_markdown_options();
            comrak::markdown_to_html(body_text, &options)
        }

        BodyKind::AsciiDoc => {
            let origin = ctx.document.path.parent().unwrap_or_else(|| Path::new("."));
            render_asciidoc_with_asciidocr(body_text, origin)?
        }

        BodyKind::ReStructuredText => render_restructuredtext(body_text)?,

        BodyKind::OrgMode => render_org_to_html(body_text)?,
    };

    // Index the rendered HTML using the *served* path as key.
    let served_path = serving_path_for_source(&ctx.document.path);

    index_body(&served_path, &html, body_kind)
        .map_err(|e| DocContextError::ContentIndex(e.to_string()))?;

    // Attach body kind via builder.
    ctx.document = ctx.document.with_body_kind(body_kind);

    Ok(ctx)
}

// ─────────────────────────────────────────────
// High-level pipeline: scan + process
// ─────────────────────────────────────────────

/// High-level pipeline:
/// - starts a folder scan via injected `start_scan`
/// - receives debounced `PathBuf`s from a bounded channel
/// - turns each into a `Document`
/// - runs `upsert_front_matter_db` and `upsert_body_db`
/// - collects all `Document`s and per-file `DocContextError`s
///
/// A single error does *not* stop the pipeline; it is recorded and
/// processing continues with the next path.
///
/// The only "hard" error is failing to *start* the scan, which is
/// reported as `Err(scan_err)`.
pub async fn scan_and_process_docs<ScanErr, FmErr, BodyErr>(
    root: &Path,
    scan_cfg: FolderScanConfig,
    start_scan: StartFolderScanFn<ScanErr>,
    index_front_matter: IndexFrontMatterFn<FmErr>,
    index_body: IndexBodyFn<BodyErr>,
) -> Result<(Vec<Document>, Vec<(PathBuf, DocContextError)>), ScanErr>
where
    ScanErr: std::fmt::Display,
    FmErr: std::fmt::Display,
    BodyErr: std::fmt::Display,
{
    let (mut rx, stop) = start_scan(root, &scan_cfg)?;

    let mut docs = Vec::new();
    let mut errors = Vec::new();

    while let Some(path) = rx.recv().await {
        let document = Document::new(path.clone());
        let ctx = DocContext { document };

        let result = async {
            let ctx = upsert_front_matter_db::<FmErr, _>(ctx, index_front_matter).await?;
            let ctx = upsert_body_db::<BodyErr, _>(ctx, index_body).await?;
            Ok::<_, DocContextError>(ctx)
        }
        .await;

        match result {
            Ok(ctx_done) => {
                docs.push(ctx_done.document);
            }
            Err(e) => {
                errors.push((path, e));
            }
        }
    }

    // Ensure scan tasks are stopped (idempotent if they already finished).
    stop();

    Ok((docs, errors))
}
