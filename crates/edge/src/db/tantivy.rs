use std::fs;
use std::io::{self, Cursor, Read};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use thiserror::Error;

use tantivy::collector::TopDocs;
use tantivy::directory::{error::OpenDirectoryError, MmapDirectory};
use tantivy::query::{QueryParser, QueryParserError, TermQuery};
use tantivy::schema::document::{OwnedValue, TantivyDocument};
use tantivy::schema::{Field, NamedFieldDocument, Schema, STORED, STRING, TEXT};
use tantivy::Document;
use tantivy::{doc, DocAddress, Index, IndexReader, IndexWriter, ReloadPolicy, Term};

/// How many results `search` returns by default.
const DEFAULT_SEARCH_LIMIT: usize = 50;

/// One search hit.
#[derive(Debug, Clone)]
pub struct SearchHit {
    pub path: PathBuf,
    pub score: f32,
}

#[derive(Debug, Error)]
pub enum ContentIndexError {
    #[error("I/O error: {0}")]
    Io(#[from] io::Error),

    #[error("Tantivy error: {0}")]
    Tantivy(#[from] tantivy::TantivyError),

    #[error("query parse error: {0}")]
    QueryParse(#[from] QueryParserError),

    #[error("failed to open directory: {0}")]
    OpenDirectory(#[from] OpenDirectoryError),

    #[error("writer lock was poisoned")]
    WriterPoisoned,

    #[error("document with path {0:?} not found")]
    NotFound(PathBuf),

    #[error("path field missing or not a string")]
    MissingPathField,

    #[error("content field missing or not a string")]
    MissingContentField,
}

pub type Result<T> = std::result::Result<T, ContentIndexError>;

/// Thin wrapper around Tantivy for storing & searching documents by path.
///
/// Schema:
/// - `path`: STRING | STORED
/// - `content`: TEXT | STORED  (raw bytes interpreted as UTF-8/UTF-8-lossy)
pub struct ContentIndex {
    index: Index,
    writer: Arc<Mutex<IndexWriter>>,
    reader: IndexReader,
    schema: Schema,
    path_field: Field,
    content_field: Field,
}

impl ContentIndex {
    /// Create or open an index at `index_dir`.
    ///
    /// `writer_heap_bytes` is the Tantivy writer memory budget.
    pub fn open_or_create<P: AsRef<Path>>(index_dir: P, writer_heap_bytes: usize) -> Result<Self> {
        let index_dir = index_dir.as_ref();
        fs::create_dir_all(index_dir)?;

        // Define schema: path + content.
        let mut schema_builder = Schema::builder();
        let path_field = schema_builder.add_text_field("path", STRING | STORED);
        let content_field = schema_builder.add_text_field("content", TEXT | STORED);
        let schema = schema_builder.build();

        // open_or_create wants a Directory, not a Path.
        let directory = MmapDirectory::open(index_dir)?;
        let index = Index::open_or_create(directory, schema.clone())?;

        let writer: IndexWriter = index.writer(writer_heap_bytes)?;
        let reader: IndexReader = index
            .reader_builder()
            .reload_policy(ReloadPolicy::OnCommitWithDelay)
            .try_into()?;

        Ok(Self {
            index,
            writer: Arc::new(Mutex::new(writer)),
            reader,
            schema,
            path_field,
            content_field,
        })
    }

    /// `add(path, stream)` – read all bytes from `body` and write them to the index under `path`.
    ///
    /// - Stores bytes as UTF-8 (lossy if needed) in `content`.
    /// - Indexes `content` for full-text search.
    pub fn add<R: Read>(&self, path: &Path, mut body: R) -> Result<()> {
        let mut buf = Vec::new();
        body.read_to_end(&mut buf)?;

        // Interpret as UTF-8, fall back to lossy if necessary.
        let content_str = match String::from_utf8(buf) {
            Ok(s) => s,
            Err(e) => String::from_utf8_lossy(e.as_bytes()).into_owned(),
        };

        let path_str = path.to_string_lossy().to_string();

        let mut writer = self
            .writer
            .lock()
            .map_err(|_| ContentIndexError::WriterPoisoned)?;

        let doc = doc!(
            self.path_field => path_str,
            self.content_field => content_str,
        );

        writer.add_document(doc)?;
        writer.commit()?; // flush + make new segment visible on disk

        // 🔑 IMPORTANT: ensure the IndexReader sees the latest commit immediately.
        // This is exactly what Tantivy suggests doing in tests when using
        // ReloadPolicy::OnCommitWithDelay.
        self.reader.reload()?; // uses ContentIndexError::Tantivy via `From`

        Ok(())
    }

    /// Helper: extract the first `OwnedValue::Str` for a given field name
    /// from a NamedFieldDocument, using a factory to construct the error.
    fn first_string_from_named<F>(
        named: &NamedFieldDocument,
        field_name: &str,
        missing_factory: F,
    ) -> Result<String>
    where
        F: Fn() -> ContentIndexError,
    {
        // NamedFieldDocument(BTreeMap<String, Vec<OwnedValue>>)
        let value = named
            .0
            .get(field_name)
            .and_then(|v| v.first())
            .ok_or_else(|| missing_factory())?;

        match value {
            OwnedValue::Str(s) => Ok(s.clone()),
            _ => Err(missing_factory()),
        }
    }

    /// `get(path) -> stream` – fetch stored content for `path` and return a `Read`able stream.
    pub fn get(&self, path: &Path) -> Result<Cursor<Vec<u8>>> {
        let path_str = path.to_string_lossy().to_string();
        let searcher = self.reader.searcher();

        // Exact term query on `path`.
        let term = Term::from_field_text(self.path_field, &path_str);
        let query = TermQuery::new(term, tantivy::schema::IndexRecordOption::Basic);

        let top_docs = searcher.search(&query, &TopDocs::with_limit(1))?;
        let (_, doc_address): (f32, DocAddress) = top_docs
            .into_iter()
            .next()
            .ok_or_else(|| ContentIndexError::NotFound(path.to_path_buf()))?;

        // Concrete document type: schema::document::TantivyDocument
        let retrieved: TantivyDocument = searcher.doc(doc_address)?;

        // Convert to NamedFieldDocument so we can extract fields by name.
        let named_doc: NamedFieldDocument = retrieved.to_named_doc(&self.schema);

        let content_str = Self::first_string_from_named(&named_doc, "content", || {
            ContentIndexError::MissingContentField
        })?;

        Ok(Cursor::new(content_str.into_bytes()))
    }

    /// `search(terms) -> Vec<SearchHit>` – full-text search over `content`.
    ///
    /// Returns up to `limit` hits with `path` + score.
    pub fn search(&self, query_str: &str, limit: usize) -> Result<Vec<SearchHit>> {
        let limit = if limit == 0 {
            DEFAULT_SEARCH_LIMIT
        } else {
            limit
        };

        let searcher = self.reader.searcher();

        let query_parser = QueryParser::for_index(&self.index, vec![self.content_field]);
        let query = query_parser.parse_query(query_str)?;

        let top_docs = searcher.search(&query, &TopDocs::with_limit(limit))?;

        let mut hits = Vec::with_capacity(top_docs.len());

        for (score, doc_address) in top_docs {
            let retrieved: TantivyDocument = searcher.doc(doc_address)?;
            let named_doc: NamedFieldDocument = retrieved.to_named_doc(&self.schema);

            let path_str = Self::first_string_from_named(&named_doc, "path", || {
                ContentIndexError::MissingPathField
            })?;

            hits.push(SearchHit {
                path: PathBuf::from(path_str),
                score,
            });
        }

        Ok(hits)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Cursor, Read};
    use tempfile::TempDir;

    /// Helper: create a fresh ContentIndex in a temp directory.
    fn create_temp_index() -> (TempDir, ContentIndex) {
        let tmp = TempDir::new().expect("create temp dir");
        let index_path = tmp.path().join("index");
        let index =
            ContentIndex::open_or_create(index_path, 50_000_000).expect("create ContentIndex");
        (tmp, index)
    }

    #[test]
    fn add_and_get_roundtrip() {
        let (_tmp, index) = create_temp_index();

        let path = PathBuf::from("/posts/hello.html");
        let content = "<html><body>Hello, world!</body></html>";

        // add
        index
            .add(&path, Cursor::new(content.as_bytes()))
            .expect("add should succeed");

        // get
        let mut reader = index.get(&path).expect("get should succeed");
        let mut buf = String::new();
        reader
            .read_to_string(&mut buf)
            .expect("read should succeed");

        assert_eq!(buf, content);
    }

    #[test]
    fn get_missing_path_returns_not_found() {
        let (_tmp, index) = create_temp_index();

        let missing = PathBuf::from("/does/not/exist.html");
        let err = index.get(&missing).unwrap_err();

        match err {
            ContentIndexError::NotFound(p) => {
                assert_eq!(p, missing);
            }
            other => panic!("expected NotFound error, got {other:?}"),
        }
    }

    #[test]
    fn search_finds_matching_docs() {
        let (_tmp, index) = create_temp_index();

        // Three docs:
        //  - doc1 + doc3 contain "rust"
        //  - doc2 does not
        let d1_path = PathBuf::from("/posts/rust-intro.html");
        let d2_path = PathBuf::from("/posts/other-topic.html");
        let d3_path = PathBuf::from("/posts/rust-advanced.html");

        index
            .add(
                &d1_path,
                Cursor::new(b"<html>Rust is great for systems programming.</html>"),
            )
            .expect("add d1");
        index
            .add(
                &d2_path,
                Cursor::new(b"<html>This document talks about gardening.</html>"),
            )
            .expect("add d2");
        index
            .add(
                &d3_path,
                Cursor::new(b"<html>Advanced Rust patterns for CMS engines.</html>"),
            )
            .expect("add d3");

        let hits = index.search("rust", 10).expect("search should succeed");

        let paths: Vec<PathBuf> = hits.into_iter().map(|h| h.path).collect();

        assert!(paths.contains(&d1_path), "results should contain d1");
        assert!(paths.contains(&d3_path), "results should contain d3");
        assert!(
            !paths.contains(&d2_path),
            "results should not contain d2, which doesn't mention rust"
        );
    }

    #[test]
    fn search_with_no_matches_returns_empty_vec() {
        let (_tmp, index) = create_temp_index();

        let d1_path = PathBuf::from("/posts/one.html");
        let d2_path = PathBuf::from("/posts/two.html");

        index
            .add(&d1_path, Cursor::new(b"<html>This is about cats.</html>"))
            .expect("add d1");
        index
            .add(&d2_path, Cursor::new(b"<html>This is about dogs.</html>"))
            .expect("add d2");

        let hits = index.search("quantum field theory", 10).expect("search");

        assert!(
            hits.is_empty(),
            "expected no hits for a query not present in any doc"
        );
    }

    #[test]
    fn search_limit_zero_uses_default_non_zero_limit() {
        let (_tmp, index) = create_temp_index();

        // Add several docs that all match the same term.
        for i in 0..5 {
            let path = PathBuf::from(format!("/posts/doc-{i}.html"));
            let body = format!("<html>Common term rust appears here {i}.</html>");
            index
                .add(&path, Cursor::new(body.into_bytes()))
                .expect("add");
        }

        // limit = 0 should be treated as DEFAULT_SEARCH_LIMIT internally;
        // we can't see the constant, but we can assert that:
        //  - we get at least one result
        //  - we never get more docs than we inserted
        let hits = index.search("rust", 0).expect("search");

        assert!(
            !hits.is_empty(),
            "search with limit=0 should still return some results"
        );
        assert!(
            hits.len() <= 5,
            "should not return more docs than were inserted"
        );
    }

    #[test]
    fn add_non_utf8_body_is_lossy_but_not_panicking() {
        let (_tmp, index) = create_temp_index();

        let path = PathBuf::from("/posts/binary-ish.html");
        // Some invalid UTF-8 sequence.
        let bytes = vec![0xff, 0xfe, 0xfd, b'H', b'i'];

        index
            .add(&path, Cursor::new(bytes))
            .expect("add should succeed even with invalid utf-8");

        let mut reader = index.get(&path).expect("get should succeed");
        let mut buf = String::new();
        reader
            .read_to_string(&mut buf)
            .expect("read should succeed");

        // We don't assert exact content (because of lossy conversion),
        // but we at least ensure something came back and it's non-empty.
        assert!(
            !buf.is_empty(),
            "content should not be empty after lossy utf-8 conversion"
        );
    }
}
