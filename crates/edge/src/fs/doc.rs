use crate::db::tantivy::{ContentIndex, ContentIndexError};
use adapt::mql::index::IndexRecord;
use domain::doc::{BodyKind, Document, FmKind};

use anyhow::Error as AnyError;
use comrak::{markdown_to_html, Options as MarkdownOptions};
use futures::StreamExt;
use gray_matter::engine::YAML;
use gray_matter::Matter;
use indexed_json::IndexedJson;
use orgize::Org;
use rst_parser::parse_only as parse_rst;
use rst_renderer::render_html as render_rst_html;
use serde_json::Value as Json;
use thiserror::Error;

use asciidocr::backends::htmls::render_htmlbook;
use asciidocr::parser::Parser;
use asciidocr::scanner::Scanner;

use std::fs;
use std::io;
use std::io::Cursor;
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// Per-document processing context.
pub struct DocContext {
    pub fm_index_dir: PathBuf,
    pub content_index: Arc<ContentIndex>,
    pub document: Document,
}

impl std::fmt::Debug for DocContext {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DocContext")
            .field("fm_index_dir", &self.fm_index_dir)
            .field("content_index", &"<skipped>")
            .field("document", &self.document)
            .finish()
    }
}

// ─────────────────────────────────────────────
// Error type
// ─────────────────────────────────────────────

#[derive(Debug, Error)]
pub enum DocContextError {
    #[error("I/O error: {0}")]
    Io(#[from] io::Error),

    #[error("front matter parse error: {0}")]
    FrontMatter(String),

    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),

    #[error("indexed_json error: {0}")]
    IndexedJson(#[source] AnyError),

    #[error("Tantivy error: {0}")]
    ContentIndex(#[from] ContentIndexError),

    #[error("AsciiDoc conversion error: {0}")]
    AsciiDoc(String),

    #[error("reStructuredText conversion error: {0}")]
    ReStructuredText(String),

    #[error("Org-mode conversion error: {0}")]
    Org(String),
}

// ─────────────────────────────────────────────
// Comrak markdown options (GFM-ish defaults)
// ─────────────────────────────────────────────

fn default_markdown_options() -> MarkdownOptions<'static> {
    let mut options = MarkdownOptions::default();

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
pub async fn read_document_utf8(mut ctx: DocContext) -> Result<DocContext, DocContextError> {
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
// Stage 1 — front matter → IndexedJson
// ─────────────────────────────────────────────

/// Detect front matter (YAML → TOML → JSON), parse it, project into
/// `IndexRecord` and upsert into `IndexedJson`.
///
/// Also:
/// - sets `FmKind`
/// - sets `cached_body` to the content *after* front matter
///   (or the full file if no front matter is found).
pub async fn upsert_front_matter_db(ctx: DocContext) -> Result<DocContext, DocContextError> {
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

    // 2. TOML (only if YAML found nothing)
    if fm_json.is_none() {
        let trimmed = full.trim_start_matches('\u{feff}');

        if trimmed.starts_with("+++") {
            // remove leading delimiter
            let after = &trimmed[3..];
            let after = after
                .strip_prefix('\n')
                .or(after.strip_prefix("\r\n"))
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

    // Persist FM if we have it.
    if let Some(fm_json) = fm_json {
        let id = ctx.document.path.to_string_lossy().to_string();
        let record = IndexRecord::from_json_with_id(id, &fm_json);

        fs::create_dir_all(&ctx.fm_index_dir)?;

        let mut db = IndexedJson::<IndexRecord>::open(&ctx.fm_index_dir)
            .await
            .map_err(DocContextError::IndexedJson)?;

        db.append(&record)
            .await
            .map_err(DocContextError::IndexedJson)?;
        db.flush().await.map_err(DocContextError::IndexedJson)?;
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
// Helpers: AsciiDoc, RST, Org renderers
// ─────────────────────────────────────────────

fn render_asciidoc_with_asciidocr(src: &str, origin: &Path) -> Result<String, DocContextError> {
    let scanner = Scanner::new(src);
    let mut parser = Parser::new(origin.to_path_buf());

    let graph = parser
        .parse(scanner)
        .map_err(|e| DocContextError::AsciiDoc(e.to_string()))?;

    let html = render_htmlbook(&graph).map_err(|e| DocContextError::AsciiDoc(e.to_string()))?;

    Ok(html)
}

fn render_restructuredtext(src: &str) -> Result<String, DocContextError> {
    let doc = parse_rst(src).map_err(|e| DocContextError::ReStructuredText(e.to_string()))?;

    let mut buf: Vec<u8> = Vec::new();

    render_rst_html(&doc, &mut buf, true)
        .map_err(|e| DocContextError::ReStructuredText(e.to_string()))?;

    let html =
        String::from_utf8(buf).map_err(|e| DocContextError::ReStructuredText(e.to_string()))?;

    Ok(html)
}

fn render_org_to_html(src: &str) -> Result<String, DocContextError> {
    let org = Org::parse(src);
    let mut buf: Vec<u8> = Vec::new();

    org.write_html(&mut buf)
        .map_err(|e| DocContextError::Org(e.to_string()))?;

    let html = String::from_utf8(buf).map_err(|e| DocContextError::Org(e.to_string()))?;

    Ok(html)
}

// ─────────────────────────────────────────────
// Stage 2 — body → HTML → Tantivy
// ─────────────────────────────────────────────

/// Detect body kind, render to HTML, and upsert into Tantivy.
///
/// Uses:
/// - Markdown: `comrak` with a GFM-ish `Options`
/// - AsciiDoc: `asciidocr`
/// - ReStructuredText: `rst_parser` + `rst_renderer`
/// - OrgMode: `orgize`
/// - Html / Plain: passthrough
///
/// Uses `cached_body` if present, otherwise `cache`.
pub async fn upsert_body_db(ctx: DocContext) -> Result<DocContext, DocContextError> {
    let mut ctx = read_document_utf8(ctx).await?;

    // Detect body kind from extension if not already set.
    let ext_kind = ctx
        .document
        .path
        .extension()
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
        });

    let body_kind = ctx
        .document
        .body_kind
        .or(ext_kind)
        .unwrap_or(BodyKind::Plain);

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
            markdown_to_html(body_text, &options)
        }

        BodyKind::AsciiDoc => {
            let origin = ctx.document.path.parent().unwrap_or_else(|| Path::new("."));
            render_asciidoc_with_asciidocr(body_text, origin)?
        }

        BodyKind::ReStructuredText => render_restructuredtext(body_text)?,

        BodyKind::OrgMode => render_org_to_html(body_text)?,
    };

    // Index the rendered HTML into Tantivy using the context's index.
    let mut cursor = Cursor::new(html.into_bytes());
    ctx.content_index.add(&ctx.document.path, &mut cursor)?;

    // Attach body kind via builder.
    ctx.document = ctx.document.with_body_kind(body_kind);

    Ok(ctx)
}

#[cfg(test)]
mod tests {
    use super::*;
    use bytes::Bytes;
    use futures::stream::BoxStream;
    use futures::{stream, StreamExt};
    use std::fs;
    use std::io::Read;
    use std::path::{Path, PathBuf};
    use std::time::{SystemTime, UNIX_EPOCH};
    use tokio;

    // ─────────────────────────────────────────────
    // Helpers
    // ─────────────────────────────────────────────

    fn unique_temp_dir(prefix: &str) -> PathBuf {
        let mut base = std::env::temp_dir();
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        base.push(format!("{prefix}_{nanos}"));
        fs::create_dir_all(&base).unwrap();
        base
    }

    fn test_open_bytes(path: &PathBuf) -> BoxStream<'static, io::Result<Bytes>> {
        let result = fs::read(path).map(Bytes::from);
        stream::once(async move { result }).boxed()
    }

    fn test_open_utf8(path: &PathBuf) -> BoxStream<'static, io::Result<String>> {
        let result = fs::read_to_string(path);
        stream::once(async move { result }).boxed()
    }

    fn new_content_index() -> ContentIndex {
        let index_dir = unique_temp_dir("doc_ctx_content_index");

        // Tantivy requires at least 15_000_000 bytes per thread.
        const MIN_HEAP_BYTES: usize = 16 * 1024 * 1024; // 16 MiB

        ContentIndex::open_or_create(index_dir, MIN_HEAP_BYTES)
            .expect("failed to create test ContentIndex")
    }

    fn new_doc_with_content(ext: &str, contents: &str) -> Document {
        let dir = unique_temp_dir("doc_ctx_src");
        let path = dir.join(format!("doc_ctx.{ext}"));
        fs::write(&path, contents).expect("failed to write test file");

        Document::new(path, test_open_bytes, test_open_utf8)
    }

    fn new_doc_missing_file(ext: &str) -> Document {
        let dir = unique_temp_dir("doc_ctx_missing");
        let path = dir.join(format!("missing.{ext}"));
        // Intentionally do not create the file.
        Document::new(path, test_open_bytes, test_open_utf8)
    }

    fn new_ctx_with_doc(doc: Document) -> DocContext {
        let fm_index_dir = unique_temp_dir("doc_ctx_fm");
        let content_index = new_content_index();
        DocContext {
            fm_index_dir,
            content_index: Arc::new(content_index),
            document: doc,
        }
    }

    // ─────────────────────────────────────────────
    // read_document_utf8
    // ─────────────────────────────────────────────

    #[tokio::test]
    async fn read_document_utf8_populates_cache_on_success() {
        let doc = new_doc_with_content("md", "Hello, world!");
        let ctx = new_ctx_with_doc(doc);

        let ctx = read_document_utf8(ctx)
            .await
            .expect("read_document_utf8 failed");

        assert_eq!(ctx.document.cache.as_deref(), Some("Hello, world!"));
    }

    #[tokio::test]
    async fn read_document_utf8_propagates_io_error() {
        let doc = new_doc_missing_file("md");
        let ctx = new_ctx_with_doc(doc);

        let err = read_document_utf8(ctx).await.unwrap_err();
        assert!(matches!(err, DocContextError::Io(_)));
    }

    // ─────────────────────────────────────────────
    // upsert_front_matter_db
    // ─────────────────────────────────────────────

    #[tokio::test]
    async fn upsert_front_matter_db_detects_yaml_and_strips_body() {
        let contents = r#"---
slug: hello
title: "YAML Title"
---
Body text here
"#;
        let doc = new_doc_with_content("md", contents);
        let ctx = new_ctx_with_doc(doc);

        let ctx = upsert_front_matter_db(ctx)
            .await
            .expect("upsert_front_matter_db failed");

        // FmKind set to Yaml
        assert_eq!(ctx.document.fm_kind, Some(FmKind::Yaml));

        // Body should not contain front matter keys.
        let body = ctx
            .document
            .cached_body
            .as_deref()
            .expect("cached_body should be set");
        assert!(body.contains("Body text here"));
        assert!(!body.contains("slug: hello"));

        // FM index dir should exist and contain some files.
        assert!(ctx.fm_index_dir.exists());
        let mut entries = fs::read_dir(&ctx.fm_index_dir).expect("fm_index_dir should be readable");
        assert!(entries.next().is_some());
    }

    #[tokio::test]
    async fn upsert_front_matter_db_detects_toml_and_strips_body() {
        let contents = r#"+++
slug = "hello"
title = "TOML Title"
+++
TOML body here
"#;
        let doc = new_doc_with_content("md", contents);
        let ctx = new_ctx_with_doc(doc);

        let ctx = upsert_front_matter_db(ctx)
            .await
            .expect("upsert_front_matter_db failed");

        assert_eq!(ctx.document.fm_kind, Some(FmKind::Toml));

        let body = ctx
            .document
            .cached_body
            .as_deref()
            .expect("cached_body should be set");
        assert!(body.contains("TOML body here"));
        assert!(!body.contains("slug = \"hello\""));
    }

    #[tokio::test]
    async fn upsert_front_matter_db_detects_json_front_matter() {
        let contents = r#"{"slug":"hello","title":"JSON Title"}"#;
        let doc = new_doc_with_content("json", contents);
        let ctx = new_ctx_with_doc(doc);

        let ctx = upsert_front_matter_db(ctx)
            .await
            .expect("upsert_front_matter_db failed");

        assert_eq!(ctx.document.fm_kind, Some(FmKind::Json));

        // For pure JSON, we currently treat body as empty.
        let body = ctx
            .document
            .cached_body
            .as_deref()
            .expect("cached_body should be set");
        assert!(body.is_empty());

        // FM index dir should exist and contain some files.
        assert!(ctx.fm_index_dir.exists());
        let mut entries = fs::read_dir(&ctx.fm_index_dir).expect("fm_index_dir should be readable");
        assert!(entries.next().is_some());
    }

    #[tokio::test]
    async fn upsert_front_matter_db_no_front_matter_leaves_kind_none_and_body_full() {
        let contents = "Just a plain body with no front matter.";
        let doc = new_doc_with_content("txt", contents);
        let ctx = new_ctx_with_doc(doc);

        let ctx = upsert_front_matter_db(ctx)
            .await
            .expect("upsert_front_matter_db failed");

        assert_eq!(ctx.document.fm_kind, None);

        let body = ctx
            .document
            .cached_body
            .as_deref()
            .expect("cached_body should be set");
        assert_eq!(body, contents);

        // No FM => index directory should not have been created.
        assert!(ctx.fm_index_dir.exists());
        let count = fs::read_dir(&ctx.fm_index_dir).unwrap().count();
        assert_eq!(count, 0);
    }

    #[tokio::test]
    async fn upsert_front_matter_db_invalid_json_returns_error() {
        // Starts with '{' but is invalid JSON.
        let contents = "{ this is not valid json";
        let doc = new_doc_with_content("json", contents);
        let ctx = new_ctx_with_doc(doc);

        let err = upsert_front_matter_db(ctx).await.unwrap_err();
        assert!(matches!(err, DocContextError::FrontMatter(_)));
    }

    // ─────────────────────────────────────────────
    // upsert_body_db — Markdown / Html / Plain
    // ─────────────────────────────────────────────

    fn read_indexed_html(index: &ContentIndex, path: &Path) -> String {
        let mut cursor = index.get(path).expect("indexed doc should exist");
        let mut buf = String::new();
        cursor
            .read_to_string(&mut buf)
            .expect("read from index failed");
        buf
    }

    #[tokio::test]
    async fn upsert_body_db_renders_markdown_and_indexes_html() {
        let contents = r#"---
slug: md
---
# Title

Some *emphasis* here.
"#;
        let doc = new_doc_with_content("md", contents);
        let ctx = new_ctx_with_doc(doc);

        // First strip front matter, then render/index body.
        let ctx = upsert_front_matter_db(ctx).await.expect("fm upsert failed");
        let path = ctx.document.path.clone();
        let index = ctx.content_index.clone();

        let ctx = upsert_body_db(ctx).await.expect("body upsert failed");

        assert_eq!(ctx.document.body_kind, Some(BodyKind::Markdown));

        let html = read_indexed_html(&index, &path);
        assert!(html.contains("<em>emphasis</em>"));
        // Front matter should not leak into HTML.
        assert!(!html.contains("slug: md"));
    }

    #[tokio::test]
    async fn upsert_body_db_passes_through_html() {
        let contents = "<h1>Title</h1><p>Body</p>";
        let doc = new_doc_with_content("html", contents);
        let ctx = new_ctx_with_doc(doc);

        let ctx = upsert_front_matter_db(ctx).await.expect("fm upsert failed");
        let path = ctx.document.path.clone();
        let index = ctx.content_index.clone();

        let ctx = upsert_body_db(ctx).await.expect("body upsert failed");

        assert_eq!(ctx.document.body_kind, Some(BodyKind::Html));

        let html = read_indexed_html(&index, &path);
        assert!(html.contains("<h1>Title</h1>"));
        assert!(html.contains("<p>Body</p>"));
    }

    #[tokio::test]
    async fn upsert_body_db_treats_unknown_ext_as_plain() {
        let contents = "Just some plain text.";
        let doc = new_doc_with_content("txt", contents);
        let ctx = new_ctx_with_doc(doc);

        let ctx = upsert_front_matter_db(ctx).await.expect("fm upsert failed");
        let path = ctx.document.path.clone();
        let index = ctx.content_index.clone();

        let ctx = upsert_body_db(ctx).await.expect("body upsert failed");

        assert_eq!(ctx.document.body_kind, Some(BodyKind::Plain));

        let html = read_indexed_html(&index, &path);
        assert_eq!(html, contents);
    }

    // ─────────────────────────────────────────────
    // upsert_body_db — AsciiDoc / RST / OrgMode
    // ─────────────────────────────────────────────

    #[tokio::test]
    async fn upsert_body_db_handles_asciidoc() {
        let contents = r#"= AsciiDoc Title

AsciiDoc body text.
"#;
        let doc = new_doc_with_content("adoc", contents);
        let ctx = new_ctx_with_doc(doc);

        let ctx = upsert_front_matter_db(ctx).await.expect("fm upsert failed");
        let path = ctx.document.path.clone();
        let index = ctx.content_index.clone();

        let ctx = upsert_body_db(ctx).await.expect("body upsert failed");

        assert_eq!(ctx.document.body_kind, Some(BodyKind::AsciiDoc));

        let html = read_indexed_html(&index, &path);
        // We don't depend on exact HTML shape, but body text should be present.
        assert!(html.contains("AsciiDoc body text."));
    }

    #[tokio::test]
    async fn upsert_body_db_handles_restructuredtext() {
        let contents = r#"RST Title
=========

RST body text.
"#;
        let doc = new_doc_with_content("rst", contents);
        let ctx = new_ctx_with_doc(doc);

        let ctx = upsert_front_matter_db(ctx).await.expect("fm upsert failed");
        let path = ctx.document.path.clone();
        let index = ctx.content_index.clone();

        let ctx = upsert_body_db(ctx).await.expect("body upsert failed");

        assert_eq!(ctx.document.body_kind, Some(BodyKind::ReStructuredText));

        let html = read_indexed_html(&index, &path);
        assert!(html.contains("RST body text."));
    }

    #[tokio::test]
    async fn upsert_body_db_handles_org_mode() {
        let contents = r#"* Org Title

Org body text.
"#;
        let doc = new_doc_with_content("org", contents);
        let ctx = new_ctx_with_doc(doc);

        let ctx = upsert_front_matter_db(ctx).await.expect("fm upsert failed");
        let path = ctx.document.path.clone();
        let index = ctx.content_index.clone();

        let ctx = upsert_body_db(ctx).await.expect("body upsert failed");

        assert_eq!(ctx.document.body_kind, Some(BodyKind::OrgMode));

        let html = read_indexed_html(&index, &path);
        assert!(html.contains("Org body text."));
    }
}
