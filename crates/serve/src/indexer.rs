// crates/serve/src/indexer.rs

//! High-level content ingestion pipeline.
//!
//! This crate is **completely storage-agnostic**. It does not know about the filesystem,
//! Tantivy, indexed_json, or anything else.
//!
//! The edge layer injects three functions:
//!   1. `start_scan` — begin folder scan → emits PathBufs
//!   2. `index_front_matter` — persist indexed_json FM using served path
//!   3. `index_body` — persist HTML (or passthrough) into CAS/Tantivy using served path
//!
//! After indexing, the runtime resolvers (in edge + serve/resolver.rs) will search these
//! stores using additional functions injected separately.

use domain::doc::{BodyKind, Document, FmKind};
use futures::StreamExt;
use regex::Regex;
use serde_json::Value as Json;
use thiserror::Error;

use std::io;
use std::path::{Path, PathBuf};
use tokio::sync::mpsc;

// ---------------------------------------------------------------------------
// Folder Scan Configuration
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct FolderScanConfig {
    pub absolute: bool,
    pub recursive: bool,
    pub debounce_ms: u64,
    pub canonicalize_paths: bool,
    pub channel_capacity: usize,
    pub folder_re: Option<Regex>,
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

// ---------------------------------------------------------------------------
// Context Wrapper
// ---------------------------------------------------------------------------

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

// ---------------------------------------------------------------------------
// Error Types
// ---------------------------------------------------------------------------

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

// ---------------------------------------------------------------------------
// Injection Function Types (provided by edge layer)
// ---------------------------------------------------------------------------

pub type ScanStopFn = Box<dyn FnOnce() + Send + 'static>;

pub type StartFolderScanFn<ScanErr> = fn(
    root: &Path,
    cfg: &FolderScanConfig,
) -> Result<(mpsc::Receiver<PathBuf>, ScanStopFn), ScanErr>;

pub type IndexFrontMatterFn<FmErr> = fn(served_path: &Path, fm: &Json) -> Result<(), FmErr>;

pub type IndexBodyFn<BodyErr> =
    fn(served_path: &Path, html: &str, kind: BodyKind) -> Result<(), BodyErr>;

// ---------------------------------------------------------------------------
// Markdown + Body conversion helpers
// ---------------------------------------------------------------------------

fn default_markdown_options() -> comrak::Options<'static> {
    let mut opt = comrak::Options::default();
    opt.extension.strikethrough = true;
    opt.extension.table = true;
    opt.extension.autolink = true;
    opt.extension.tasklist = true;
    opt.extension.footnotes = true;
    opt
}

fn infer_body_kind(ext: Option<&str>) -> BodyKind {
    match ext.map(|s| s.to_ascii_lowercase()) {
        Some(ref e) if e == "md" || e == "markdown" || e == "mkd" || e == "mkdn" => {
            BodyKind::Markdown
        }
        Some(ref e) if e == "adoc" || e == "asciidoc" => BodyKind::AsciiDoc,
        Some(ref e) if e == "html" || e == "htm" || e == "xhtml" => BodyKind::Html,
        Some(ref e) if e == "rst" => BodyKind::ReStructuredText,
        Some(ref e) if e == "org" => BodyKind::OrgMode,
        _ => BodyKind::Plain,
    }
}

fn served_path_for_source(path: &Path) -> PathBuf {
    let kind = infer_body_kind(path.extension().and_then(|s| s.to_str()));
    match kind {
        BodyKind::Markdown
        | BodyKind::AsciiDoc
        | BodyKind::ReStructuredText
        | BodyKind::OrgMode => {
            let mut p = path.to_owned();
            p.set_extension("html");
            p
        }
        BodyKind::Html | BodyKind::Plain => path.to_owned(),
    }
}

// ---------------------------------------------------------------------------
// Stage 1 — Front Matter Upsert
// ---------------------------------------------------------------------------

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

    let mut fm_json = None;
    let mut fm_kind = None;
    let mut body = None;

    // YAML FM
    if fm_json.is_none() {
        let matter: Matter<YAML> = Matter::new();
        if let Ok(parsed) = matter.parse::<Json>(full) {
            if let Some(data) = parsed.data {
                fm_json = Some(data);
                fm_kind = Some(FmKind::Yaml);
                body = Some(parsed.content);
            }
        }
    }

    // TOML FM
    if fm_json.is_none() {
        let trimmed = full.trim_start_matches('\u{feff}');
        if trimmed.starts_with("+++") {
            let after = trimmed.trim_start_matches('+').trim_start_matches("\n");
            if let Some(end_idx) = after.find("\n+++") {
                let fm_src = &after[..end_idx];
                match toml::from_str::<toml::Value>(fm_src) {
                    Ok(toml_val) => {
                        fm_json = Some(
                            serde_json::to_value(toml_val)
                                .map_err(|e| DocContextError::FrontMatter(e.to_string()))?,
                        );
                        fm_kind = Some(FmKind::Toml);
                        body = Some(after[end_idx + 4..].trim_start().to_owned());
                    }
                    Err(e) => return Err(DocContextError::FrontMatter(e.to_string())),
                }
            }
        }
    }

    // JSON FM
    if fm_json.is_none() {
        let trimmed = full.trim();
        if trimmed.starts_with('{') {
            match serde_json::from_str::<Json>(trimmed) {
                Ok(v) => {
                    fm_json = Some(v);
                    fm_kind = Some(FmKind::Json);
                    body = Some(String::new());
                }
                Err(e) => return Err(DocContextError::FrontMatter(e.to_string())),
            }
        }
    }

    // If body not extracted, set full file as body
    if body.is_none() {
        body = Some(full.to_owned());
    }

    if let Some(data) = fm_json {
        let served = served_path_for_source(&ctx.document.path);
        index_front_matter(&served, &data)
            .map_err(|e| DocContextError::FrontMatterIndex(e.to_string()))?;
    }

    if let Some(kind) = fm_kind {
        ctx.document = ctx.document.with_fm_kind(kind);
    }

    if let Some(b) = body {
        ctx.document = ctx.document.with_body(b);
    }

    Ok(ctx)
}

// ---------------------------------------------------------------------------
// Stage 2 — Body Rendering + Content Indexing
// ---------------------------------------------------------------------------

fn render_asciidoc(src: &str, origin: &Path) -> Result<String, DocContextError> {
    use asciidocr::backends::htmls::render_htmlbook;
    use asciidocr::parser::Parser;
    use asciidocr::scanner::Scanner;

    let scanner = Scanner::new(src);
    let mut parser = Parser::new(origin.to_path_buf());
    let graph = parser
        .parse(scanner)
        .map_err(|e| DocContextError::AsciiDoc(e.to_string()))?;
    Ok(render_htmlbook(&graph).map_err(|e| DocContextError::AsciiDoc(e.to_string()))?)
}

fn render_rst(src: &str) -> Result<String, DocContextError> {
    use rst_parser::parse_only;
    use rst_renderer::render_html;

    let doc = parse_only(src).map_err(|e| DocContextError::ReStructuredText(e.to_string()))?;
    let mut buf = Vec::new();
    render_html(&doc, &mut buf, true)
        .map_err(|e| DocContextError::ReStructuredText(e.to_string()))?;
    let html =
        String::from_utf8(buf).map_err(|e| DocContextError::ReStructuredText(e.to_string()))?;
    Ok(html)
}

fn render_org(src: &str) -> Result<String, DocContextError> {
    use orgize::Org;
    let org = Org::parse(src);
    let mut buf = Vec::new();
    org.write_html(&mut buf)
        .map_err(|e| DocContextError::Org(e.to_string()))?;
    Ok(String::from_utf8(buf).map_err(|e| DocContextError::Org(e.to_string()))?)
}

pub async fn upsert_body_db<BodyErr, BodyIndexFn>(
    ctx: DocContext,
    index_body: BodyIndexFn,
) -> Result<DocContext, DocContextError>
where
    BodyErr: std::fmt::Display,
    BodyIndexFn: Fn(&Path, &str, BodyKind) -> Result<(), BodyErr>,
{
    let mut ctx = read_document_utf8(ctx).await?;

    let ext = ctx.document.path.extension().and_then(|s| s.to_str());
    let kind = infer_body_kind(ext);

    let body_text = ctx
        .document
        .cached_body
        .as_deref()
        .or_else(|| ctx.document.cache.as_deref())
        .unwrap_or("");

    let html = match kind {
        BodyKind::Html | BodyKind::Plain => body_text.to_owned(),
        BodyKind::Markdown => comrak::markdown_to_html(body_text, &default_markdown_options()),
        BodyKind::AsciiDoc => {
            let origin = ctx.document.path.parent().unwrap_or_else(|| Path::new("."));
            render_asciidoc(body_text, origin)?
        }
        BodyKind::ReStructuredText => render_rst(body_text)?,
        BodyKind::OrgMode => render_org(body_text)?,
    };

    let served = served_path_for_source(&ctx.document.path);

    index_body(&served, &html, kind).map_err(|e| DocContextError::ContentIndex(e.to_string()))?;

    ctx.document = ctx.document.with_body_kind(kind);
    Ok(ctx)
}

// ---------------------------------------------------------------------------
// High-Level Pipeline
// ---------------------------------------------------------------------------

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

        let processed = async {
            let ctx = upsert_front_matter_db::<FmErr, _>(ctx, index_front_matter).await?;
            let ctx = upsert_body_db::<BodyErr, _>(ctx, index_body).await?;
            Ok::<_, DocContextError>(ctx)
        }
        .await;

        match processed {
            Ok(done) => docs.push(done.document),
            Err(err) => errors.push((path, err)),
        }
    }

    stop();
    Ok((docs, errors))
}
