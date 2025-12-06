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

use crate::Error;
use async_trait::async_trait;
use domain::doc::{BodyKind, Document, FmKind};
use regex::Regex;
use serde_json::Value as Json;
use std::path::{Path, PathBuf};
use std::sync::Arc;
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
// Injection Function Types (provided by edge layer)
// ---------------------------------------------------------------------------

pub type ScanStopFn = Box<dyn FnOnce() + Send + 'static>;

#[async_trait]
pub trait ContentManager {
    async fn scan_file(&self, path: &Path) -> Result<Arc<str>, Error>;

    async fn scan_folder(
        &self,
        root: &Path,
        cfg: &FolderScanConfig,
    ) -> Result<(mpsc::Receiver<PathBuf>, ScanStopFn), Error>;

    async fn index_front_matter(&self, served_path: &Path, fm: &Json) -> Result<(), Error>;

    async fn index_body(&self, served_path: &Path, html: &str, kind: BodyKind)
        -> Result<(), Error>;

    async fn lookup_slug(&self, slug: &str) -> Result<Option<Json>, Error>;
    async fn lookup_served(&self, served: &str) -> Result<Option<Json>, Error>;
    async fn lookup_body(&self, key: &str) -> Result<Option<Arc<str>>, Error>;
}

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

async fn read_document_utf8(
    mut ctx: DocContext,
    scan_indexer: &impl ContentManager,
) -> Result<DocContext, Error> {
    if ctx.document.cache.is_some() {
        return Ok(ctx);
    }

    let text = scan_indexer.scan_file(&ctx.document.path).await?;

    ctx.document = ctx.document.with_cache(text);
    Ok(ctx)
}

pub async fn upsert_front_matter_db(
    ctx: DocContext,
    scan_indexer: &impl ContentManager,
) -> Result<DocContext, Error> {
    use gray_matter::engine::YAML;
    use gray_matter::Matter;

    let mut ctx = read_document_utf8(ctx, scan_indexer).await?;
    let full = ctx.document.cache.clone().unwrap_or(Arc::from(""));

    let mut fm_json = None;
    let mut fm_kind = None;
    let mut body = None;

    // YAML FM
    if fm_json.is_none() {
        let matter: Matter<YAML> = Matter::new();
        if let Ok(parsed) = matter.parse::<Json>(&full) {
            if let Some(data) = parsed.data {
                fm_json = Some(data);
                fm_kind = Some(FmKind::Yaml);
                body = Some(Arc::from(parsed.content.as_str()));
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
                                .map_err(|e| Error::FrontMatter(e.to_string()))?,
                        );
                        fm_kind = Some(FmKind::Toml);
                        body = Some(Arc::from(after[end_idx + 4..].trim_start()));
                    }
                    Err(e) => return Err(Error::FrontMatter(e.to_string())),
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
                    body = Some(Arc::from(""));
                }
                Err(e) => return Err(Error::FrontMatter(e.to_string())),
            }
        }
    }

    // If body not extracted, set full file as body
    if body.is_none() {
        body = Some(full.to_owned());
    }

    if let Some(data) = fm_json {
        let served = served_path_for_source(&ctx.document.path);
        scan_indexer
            .index_front_matter(&served, &data)
            .await
            .map_err(|e| Error::FrontMatterIndex(e.to_string()))?;
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

fn render_asciidoc(src: &str, origin: &Path) -> Result<String, Error> {
    use asciidocr::backends::htmls::render_htmlbook;
    use asciidocr::parser::Parser;
    use asciidocr::scanner::Scanner;

    let scanner = Scanner::new(src);
    let mut parser = Parser::new(origin.to_path_buf());
    let graph = parser
        .parse(scanner)
        .map_err(|e| Error::AsciiDoc(e.to_string()))?;
    Ok(render_htmlbook(&graph).map_err(|e| Error::AsciiDoc(e.to_string()))?)
}

fn render_rst(src: &str) -> Result<String, Error> {
    use rst_parser::parse_only;
    use rst_renderer::render_html;

    let doc = parse_only(src).map_err(|e| Error::ReStructuredText(e.to_string()))?;
    let mut buf = Vec::new();
    render_html(&doc, &mut buf, true).map_err(|e| Error::ReStructuredText(e.to_string()))?;
    let html = String::from_utf8(buf).map_err(|e| Error::ReStructuredText(e.to_string()))?;
    Ok(html)
}

fn render_org(src: &str) -> Result<String, Error> {
    use orgize::Org;
    let org = Org::parse(src);
    let mut buf = Vec::new();
    org.write_html(&mut buf)
        .map_err(|e| Error::Org(e.to_string()))?;
    Ok(String::from_utf8(buf).map_err(|e| Error::Org(e.to_string()))?)
}

pub async fn upsert_body_db(
    ctx: DocContext,
    scan_indexer: &impl ContentManager,
) -> Result<DocContext, Error> {
    let mut ctx = read_document_utf8(ctx, scan_indexer).await?;

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

    scan_indexer
        .index_body(&served, &html, kind)
        .await
        .map_err(|e| Error::ContentIndex(e.to_string()))?;

    ctx.document = ctx.document.with_body_kind(kind);
    Ok(ctx)
}

// ---------------------------------------------------------------------------
// High-Level Pipeline
// ---------------------------------------------------------------------------

pub async fn scan_and_process_docs(
    root: &Path,
    scan_cfg: FolderScanConfig,
    scan_indexer: impl ContentManager,
) -> Result<(Vec<Document>, Vec<(PathBuf, Error)>), Error> {
    let (mut rx, stop) = scan_indexer.scan_folder(root, &scan_cfg).await?;

    let mut docs = Vec::new();
    let mut errors = Vec::new();

    while let Some(path) = rx.recv().await {
        let document = Document::new(path.clone());
        let ctx = DocContext { document };

        let processed = async {
            let ctx = upsert_front_matter_db(ctx, &scan_indexer).await?;
            let ctx = upsert_body_db(ctx, &scan_indexer).await?;
            Ok::<_, Error>(ctx)
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
