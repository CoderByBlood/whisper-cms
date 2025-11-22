use anyhow::Result as AnyResult;
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use indexed_json::{Indexable as IjIndexable, IndexableField};
use serde::{Deserialize, Serialize};
use serde_json::Value as Json;
use smallvec::SmallVec;
use std::any::Any;
use std::cmp::Ordering;
use std::collections::HashSet;
use std::fmt::{Debug, Display, Formatter};
use std::hash::Hash;

// ─────────────────────────────────────────────────────────────────────────────
// Index configuration
// ─────────────────────────────────────────────────────────────────────────────

/// Configuration: which fields are indexed.
///
/// This is used by the MQL query planner to decide which predicates
/// can be answered by the index backend.
#[derive(Debug, Clone)]
pub struct IndexConfig {
    fields: HashSet<String>,
}

impl IndexConfig {
    /// Create a new IndexConfig from any iterable of field names.
    ///
    /// Example:
    /// ```ignore
    /// let cfg = IndexConfig::new([
    ///     "type",
    ///     "slug",
    ///     "publish.date",
    ///     "tax.tags",
    /// ]);
    /// ```
    pub fn new<I, S>(fields: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        Self {
            fields: fields.into_iter().map(Into::into).collect(),
        }
    }

    /// Returns true if this field has an index defined.
    pub fn is_indexed(&self, field: &str) -> bool {
        self.fields.contains(field)
    }

    /// Iterate over all indexed fields.
    pub fn fields(&self) -> impl Iterator<Item = &str> {
        self.fields.iter().map(|s| s.as_str())
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Async JsonStore abstraction
// ─────────────────────────────────────────────────────────────────────────────

/// Async abstraction over a JSON document store (in-memory or disk-backed).
///
/// The store is responsible for:
/// - enumerating all document IDs
/// - returning an owned JSON document for a given ID
///
/// Returning owned `Json` (rather than `&Json`) keeps this usable for both
/// in-memory and disk-backed implementations (e.g. `indexed_json`).
#[async_trait]
pub trait JsonStore {
    /// Document ID type (e.g. usize for in-memory, `IndexedId` for indexed_json).
    type Id: Copy + Eq + Hash + Send + Sync + 'static;

    /// Get all document IDs.
    async fn all_ids(&self) -> Vec<Self::Id>;

    /// Get a document by ID (owned JSON).
    async fn get(&self, id: Self::Id) -> Option<Json>;
}

// ─────────────────────────────────────────────────────────────────────────────
// Async IndexBackend abstraction (used by QueryPlanner)
// ─────────────────────────────────────────────────────────────────────────────

/// Async index backend API: equality / membership / range lookups on fields.
///
/// Implementations are free to ignore any part of this contract by returning
/// `None` (meaning "I don't have an index for this predicate") and letting
/// the planner fall back to a broader scan.
#[async_trait]
pub trait IndexBackend {
    type Id: Copy + Eq + Hash + Send + Sync + 'static;

    /// Lookup IDs for `field == value`.
    ///
    /// Returns:
    /// - `Some(HashSet<Id>)` if the field is indexed and the index can answer
    ///   this predicate.
    /// - `None` if the field is not indexed or this backend cannot answer it.
    async fn lookup_eq(&self, field: &str, value: &Json) -> Option<HashSet<Self::Id>>;

    /// Lookup IDs for `field IN values` (union).
    ///
    /// Returns:
    /// - `Some(HashSet<Id>)` if the field is indexed and the index can answer
    ///   this predicate.
    /// - `None` if the field is not indexed or this backend cannot answer it.
    async fn lookup_in(&self, field: &str, values: &[Json]) -> Option<HashSet<Self::Id>>;

    /// Range lookups (optional).
    ///
    /// Returns:
    /// - `Some(HashSet<Id>)` if the backend can handle the range query.
    /// - `None` if not supported / not indexed.
    async fn lookup_range(
        &self,
        _field: &str,
        _min: Option<&Json>,
        _max: Option<&Json>,
    ) -> Option<HashSet<Self::Id>> {
        None
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// IndexRecord: strongly-typed index projection of front matter
// ─────────────────────────────────────────────────────────────────────────────

/// Strongly-typed projection of the front matter that we index.
///
/// This mirrors the “WordPress-ish” shape we agreed on.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct IndexRecord {
    pub id: String,
    #[serde(rename = "type")]
    pub kind: Option<String>,
    pub slug: Option<String>,
    pub parent: Option<String>,

    #[serde(default)]
    pub content: ContentFields,
    #[serde(default)]
    pub publish: PublishFields,
    #[serde(default)]
    pub nav: NavFields,
    #[serde(default)]
    pub tax: TaxFields,
    #[serde(default)]
    pub i18n: I18nFields,
    #[serde(default)]
    pub author: AuthorFields,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ContentFields {
    pub title: Option<String>,
    pub section: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct PublishFields {
    pub status: Option<String>,
    /// ISO-8601 publish date
    pub date: Option<String>,
    /// ISO-8601 modified date
    pub modified: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct NavFields {
    pub menu_order: Option<i64>,
    pub menu_visible: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct TaxFields {
    #[serde(default)]
    pub categories: Vec<String>,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default)]
    pub series: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct I18nFields {
    pub lang: Option<String>,
    pub canonical_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct AuthorFields {
    pub author: Option<String>,
    #[serde(default)]
    pub co_authors: Vec<String>,
}

impl IndexRecord {
    /// Build an IndexRecord from a raw document JSON and explicit id.
    ///
    /// Missing fields simply become `None` / `Vec::new()`.
    pub fn from_json_with_id(id: String, doc: &Json) -> Self {
        use super::eval::get_field_value;

        fn as_string(v: Option<&Json>) -> Option<String> {
            v.and_then(|j| j.as_str().map(|s| s.to_owned()))
        }

        fn as_i64(v: Option<&Json>) -> Option<i64> {
            v.and_then(|j| j.as_i64())
        }

        fn as_bool(v: Option<&Json>) -> Option<bool> {
            v.and_then(|j| j.as_bool())
        }

        fn as_string_vec(v: Option<&Json>) -> Vec<String> {
            match v {
                Some(Json::Array(arr)) => arr
                    .iter()
                    .filter_map(|item| item.as_str().map(|s| s.to_owned()))
                    .collect(),
                Some(Json::String(s)) => vec![s.clone()],
                _ => Vec::new(),
            }
        }

        let kind = as_string(get_field_value(doc, "type").or_else(|| get_field_value(doc, "kind")));
        let slug = as_string(get_field_value(doc, "slug"));
        let parent = as_string(get_field_value(doc, "parent"));

        let content = ContentFields {
            title: as_string(get_field_value(doc, "content.title")),
            section: as_string(get_field_value(doc, "content.section")),
        };

        let publish = PublishFields {
            status: as_string(get_field_value(doc, "publish.status")),
            date: as_string(get_field_value(doc, "publish.date")),
            modified: as_string(get_field_value(doc, "publish.modified")),
        };

        let nav = NavFields {
            menu_order: as_i64(get_field_value(doc, "nav.menu_order")),
            menu_visible: as_bool(get_field_value(doc, "nav.menu_visible")),
        };

        let tax = TaxFields {
            categories: as_string_vec(get_field_value(doc, "tax.categories")),
            tags: as_string_vec(get_field_value(doc, "tax.tags")),
            series: as_string_vec(get_field_value(doc, "tax.series")),
        };

        let i18n = I18nFields {
            lang: as_string(get_field_value(doc, "i18n.lang")),
            canonical_id: as_string(get_field_value(doc, "i18n.canonical_id")),
        };

        let author = AuthorFields {
            author: as_string(get_field_value(doc, "author.author")),
            co_authors: as_string_vec(get_field_value(doc, "author.co_authors")),
        };

        IndexRecord {
            id,
            kind,
            slug,
            parent,
            content,
            publish,
            nav,
            tax,
            i18n,
            author,
        }
    }
}

// Convenience conversions from (id, Json) tuples.
impl From<(&str, &Json)> for IndexRecord {
    fn from((id, doc): (&str, &Json)) -> Self {
        IndexRecord::from_json_with_id(id.to_owned(), doc)
    }
}

impl From<(String, &Json)> for IndexRecord {
    fn from((id, doc): (String, &Json)) -> Self {
        IndexRecord::from_json_with_id(id, doc)
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// impl indexed_json::Indexable for IndexRecord
// ─────────────────────────────────────────────────────────────────────────────

impl IjIndexable for IndexRecord {
    type Iter = Vec<Box<dyn IndexableField>>;

    fn index(&self) -> Self::Iter {
        let mut out: Vec<Box<dyn IndexableField>> = Vec::new();

        // Root fields
        out.push(Box::new(StringField::new("id", self.id.clone())));

        if let Some(kind) = &self.kind {
            out.push(Box::new(StringField::new("type", kind.clone())));
        }
        if let Some(slug) = &self.slug {
            out.push(Box::new(StringField::new("slug", slug.clone())));
        }
        if let Some(parent) = &self.parent {
            out.push(Box::new(StringField::new("parent", parent.clone())));
        }

        // content.*
        if let Some(title) = &self.content.title {
            out.push(Box::new(StringField::new("content.title", title.clone())));
        }
        if let Some(section) = &self.content.section {
            out.push(Box::new(StringField::new(
                "content.section",
                section.clone(),
            )));
        }

        // publish.*
        if let Some(status) = &self.publish.status {
            out.push(Box::new(StringField::new("publish.status", status.clone())));
        }
        if let Some(date) = &self.publish.date {
            out.push(Box::new(StringField::new("publish.date", date.clone())));
        }
        if let Some(modified) = &self.publish.modified {
            out.push(Box::new(StringField::new(
                "publish.modified",
                modified.clone(),
            )));
        }

        // nav.*
        if let Some(order) = self.nav.menu_order {
            out.push(Box::new(I64Field::new("nav.menu_order", order)));
        }
        if let Some(visible) = self.nav.menu_visible {
            out.push(Box::new(BoolField::new("nav.menu_visible", visible)));
        }

        // tax.* (multi-valued: one index entry per category/tag/series item)
        for cat in &self.tax.categories {
            out.push(Box::new(StringField::new("tax.categories", cat.clone())));
        }
        for tag in &self.tax.tags {
            out.push(Box::new(StringField::new("tax.tags", tag.clone())));
        }
        for series in &self.tax.series {
            out.push(Box::new(StringField::new("tax.series", series.clone())));
        }

        // i18n.*
        if let Some(lang) = &self.i18n.lang {
            out.push(Box::new(StringField::new("i18n.lang", lang.clone())));
        }
        if let Some(cid) = &self.i18n.canonical_id {
            out.push(Box::new(StringField::new("i18n.canonical_id", cid.clone())));
        }

        // author.*
        if let Some(author) = &self.author.author {
            out.push(Box::new(StringField::new("author.author", author.clone())));
        }
        for co in &self.author.co_authors {
            out.push(Box::new(StringField::new("author.co_authors", co.clone())));
        }

        out
    }

    fn timestamp(&self) -> DateTime<Utc> {
        // Prefer publish.date if present and parseable; otherwise now().
        if let Some(date_str) = &self.publish.date {
            if let Ok(dt) = DateTime::parse_from_rfc3339(date_str) {
                return dt.with_timezone(&Utc);
            }
        }
        Utc::now()
    }

    fn dyn_partial_cmp(&self, i: &dyn IndexableField) -> Option<Ordering> {
        match i.key() {
            // Root
            "id" => {
                let f = i.as_any().downcast_ref::<StringField>()?;
                Some(self.id.cmp(&f.value))
            }
            "type" => {
                let f = i.as_any().downcast_ref::<StringField>()?;
                self.kind.as_ref().map(|v| v.cmp(&f.value))
            }
            "slug" => {
                let f = i.as_any().downcast_ref::<StringField>()?;
                self.slug.as_ref().map(|v| v.cmp(&f.value))
            }
            "parent" => {
                let f = i.as_any().downcast_ref::<StringField>()?;
                self.parent.as_ref().map(|v| v.cmp(&f.value))
            }

            // content.*
            "content.title" => {
                let f = i.as_any().downcast_ref::<StringField>()?;
                self.content.title.as_ref().map(|v| v.cmp(&f.value))
            }
            "content.section" => {
                let f = i.as_any().downcast_ref::<StringField>()?;
                self.content.section.as_ref().map(|v| v.cmp(&f.value))
            }

            // publish.*
            "publish.status" => {
                let f = i.as_any().downcast_ref::<StringField>()?;
                self.publish.status.as_ref().map(|v| v.cmp(&f.value))
            }
            "publish.date" => {
                let f = i.as_any().downcast_ref::<StringField>()?;
                self.publish.date.as_ref().map(|v| v.cmp(&f.value))
            }
            "publish.modified" => {
                let f = i.as_any().downcast_ref::<StringField>()?;
                self.publish.modified.as_ref().map(|v| v.cmp(&f.value))
            }

            // nav.*
            "nav.menu_order" => {
                let f = i.as_any().downcast_ref::<I64Field>()?;
                self.nav.menu_order.map(|v| v.cmp(&f.value))
            }
            "nav.menu_visible" => {
                let f = i.as_any().downcast_ref::<BoolField>()?;
                self.nav.menu_visible.map(|v| v.cmp(&f.value))
            }

            // tax.*: treat "contains" as Equal
            "tax.categories" => {
                let f = i.as_any().downcast_ref::<StringField>()?;
                if self.tax.categories.iter().any(|c| c == &f.value) {
                    Some(Ordering::Equal)
                } else {
                    None
                }
            }
            "tax.tags" => {
                let f = i.as_any().downcast_ref::<StringField>()?;
                if self.tax.tags.iter().any(|t| t == &f.value) {
                    Some(Ordering::Equal)
                } else {
                    None
                }
            }
            "tax.series" => {
                let f = i.as_any().downcast_ref::<StringField>()?;
                if self.tax.series.iter().any(|s| s == &f.value) {
                    Some(Ordering::Equal)
                } else {
                    None
                }
            }

            // i18n.*
            "i18n.lang" => {
                let f = i.as_any().downcast_ref::<StringField>()?;
                self.i18n.lang.as_ref().map(|v| v.cmp(&f.value))
            }
            "i18n.canonical_id" => {
                let f = i.as_any().downcast_ref::<StringField>()?;
                self.i18n.canonical_id.as_ref().map(|v| v.cmp(&f.value))
            }

            // author.*
            "author.author" => {
                let f = i.as_any().downcast_ref::<StringField>()?;
                self.author.author.as_ref().map(|v| v.cmp(&f.value))
            }
            "author.co_authors" => {
                let f = i.as_any().downcast_ref::<StringField>()?;
                if self.author.co_authors.iter().any(|c| c == &f.value) {
                    Some(Ordering::Equal)
                } else {
                    None
                }
            }

            _ => None,
        }
    }
}
// ─────────────────────────────────────────────────────────────────────────────
// IndexableField implementations for IndexRecord fields
// ─────────────────────────────────────────────────────────────────────────────

#[derive(Debug)]
pub struct StringField {
    pub key: &'static str,
    pub value: String,
}

impl StringField {
    pub fn new(key: &'static str, value: String) -> Self {
        Self { key, value }
    }
}

impl Display for StringField {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.value)
    }
}

impl IndexableField for StringField {
    fn key(&self) -> &'static str {
        self.key
    }

    fn byte_compareable(&self) -> bool {
        true // UTF-8 lexicographic matches string ordering
    }

    fn encode(&self, buf: &mut SmallVec<[u8; 128]>) -> AnyResult<()> {
        buf.clear();
        buf.extend_from_slice(self.value.as_bytes());
        Ok(())
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

#[derive(Debug)]
pub struct I64Field {
    pub key: &'static str,
    pub value: i64,
}

impl I64Field {
    pub fn new(key: &'static str, value: i64) -> Self {
        Self { key, value }
    }
}

impl Display for I64Field {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.value)
    }
}

impl IndexableField for I64Field {
    fn key(&self) -> &'static str {
        self.key
    }

    fn byte_compareable(&self) -> bool {
        true // big-endian encoding preserves numeric order
    }

    fn encode(&self, buf: &mut SmallVec<[u8; 128]>) -> AnyResult<()> {
        buf.clear();
        buf.extend_from_slice(&self.value.to_be_bytes());
        Ok(())
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

#[derive(Debug)]
pub struct BoolField {
    pub key: &'static str,
    pub value: bool,
}

impl BoolField {
    pub fn new(key: &'static str, value: bool) -> Self {
        Self { key, value }
    }
}

impl Display for BoolField {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.value)
    }
}

impl IndexableField for BoolField {
    fn key(&self) -> &'static str {
        self.key
    }

    fn byte_compareable(&self) -> bool {
        true // 0 < 1 works fine
    }

    fn encode(&self, buf: &mut SmallVec<[u8; 128]>) -> AnyResult<()> {
        buf.clear();
        buf.push(if self.value { 1 } else { 0 });
        Ok(())
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use serde_json::json;
    use std::collections::HashSet;

    // ─────────────────────────────────────────────────────────────
    // IndexConfig tests
    // ─────────────────────────────────────────────────────────────

    #[test]
    fn index_config_basic_usage() {
        let cfg = IndexConfig::new(["type", "slug", "publish.date"]);

        assert!(cfg.is_indexed("type"));
        assert!(cfg.is_indexed("slug"));
        assert!(cfg.is_indexed("publish.date"));

        assert!(!cfg.is_indexed("unknown"));
        assert!(!cfg.is_indexed("publish.modified"));
    }

    #[test]
    fn index_config_deduplicates_fields() {
        let cfg = IndexConfig::new(["slug", "slug", "publish.date", "publish.date"]);

        // Collect fields into a set to confirm uniqueness.
        let fields: HashSet<&str> = cfg.fields().collect();

        assert_eq!(fields.len(), 2);
        assert!(fields.contains("slug"));
        assert!(fields.contains("publish.date"));
    }

    #[test]
    fn index_config_fields_iterator_matches_inserted_fields() {
        let cfg = IndexConfig::new(vec!["type", "slug", "tax.tags"]);

        let mut fields: Vec<&str> = cfg.fields().collect();
        fields.sort();

        assert_eq!(fields, vec!["slug", "tax.tags", "type"]);
    }

    // ─────────────────────────────────────────────────────────────
    // IndexRecord::from_json_with_id tests (full + partial)
    // ─────────────────────────────────────────────────────────────

    #[test]
    fn index_record_from_json_with_full_front_matter() {
        let doc = json!({
            "type": "post",
            "slug": "hello-world",
            "parent": "root",

            "content": {
                "title": "Hello World",
                "section": "blog"
            },
            "publish": {
                "status": "publish",
                "date": "2024-01-01T12:34:56Z",
                "modified": "2024-01-02T10:00:00Z"
            },
            "nav": {
                "menu_order": 10,
                "menu_visible": true
            },
            "tax": {
                "categories": ["rust", "web"],
                "tags": ["hello", "world"],
                "series": ["intro"]
            },
            "i18n": {
                "lang": "en",
                "canonical_id": "post-001"
            },
            "author": {
                "author": "Alice",
                "co_authors": ["Bob", "Carol"]
            }
        });

        let rec = IndexRecord::from_json_with_id("doc-1".to_string(), &doc);

        // Root fields
        assert_eq!(rec.id, "doc-1");
        assert_eq!(rec.kind.as_deref(), Some("post"));
        assert_eq!(rec.slug.as_deref(), Some("hello-world"));
        assert_eq!(rec.parent.as_deref(), Some("root"));

        // Content
        assert_eq!(rec.content.title.as_deref(), Some("Hello World"));
        assert_eq!(rec.content.section.as_deref(), Some("blog"));

        // Publish
        assert_eq!(rec.publish.status.as_deref(), Some("publish"));
        assert_eq!(rec.publish.date.as_deref(), Some("2024-01-01T12:34:56Z"));
        assert_eq!(
            rec.publish.modified.as_deref(),
            Some("2024-01-02T10:00:00Z")
        );

        // Nav
        assert_eq!(rec.nav.menu_order, Some(10));
        assert_eq!(rec.nav.menu_visible, Some(true));

        // Tax
        assert_eq!(
            rec.tax.categories,
            vec!["rust".to_string(), "web".to_string()]
        );
        assert_eq!(rec.tax.tags, vec!["hello".to_string(), "world".to_string()]);
        assert_eq!(rec.tax.series, vec!["intro".to_string()]);

        // I18n
        assert_eq!(rec.i18n.lang.as_deref(), Some("en"));
        assert_eq!(rec.i18n.canonical_id.as_deref(), Some("post-001"));

        // Author
        assert_eq!(rec.author.author.as_deref(), Some("Alice"));
        assert_eq!(
            rec.author.co_authors,
            vec!["Bob".to_string(), "Carol".to_string()]
        );
    }

    #[test]
    fn index_record_handles_missing_fields_with_defaults() {
        // Completely empty doc: everything except id should be defaulted.
        let doc = json!({});

        let rec = IndexRecord::from_json_with_id("empty".to_string(), &doc);

        assert_eq!(rec.id, "empty");

        // All Option fields should be None, Vec fields empty.
        assert_eq!(rec.kind, None);
        assert_eq!(rec.slug, None);
        assert_eq!(rec.parent, None);

        assert_eq!(rec.content.title, None);
        assert_eq!(rec.content.section, None);

        assert_eq!(rec.publish.status, None);
        assert_eq!(rec.publish.date, None);
        assert_eq!(rec.publish.modified, None);

        assert_eq!(rec.nav.menu_order, None);
        assert_eq!(rec.nav.menu_visible, None);

        assert!(rec.tax.categories.is_empty());
        assert!(rec.tax.tags.is_empty());
        assert!(rec.tax.series.is_empty());

        assert_eq!(rec.i18n.lang, None);
        assert_eq!(rec.i18n.canonical_id, None);

        assert_eq!(rec.author.author, None);
        assert!(rec.author.co_authors.is_empty());
    }

    #[test]
    fn index_record_type_falls_back_to_kind_when_type_missing() {
        let doc = json!({
            "kind": "page",
            "slug": "about"
        });

        let rec = IndexRecord::from_json_with_id("k1".to_string(), &doc);

        // "type" missing, so we should pick up "kind".
        assert_eq!(rec.kind.as_deref(), Some("page"));
        assert_eq!(rec.slug.as_deref(), Some("about"));
    }

    #[test]
    fn index_record_type_prefers_type_over_kind_when_both_present() {
        let doc = json!({
            "type": "post",
            "kind": "page",
            "slug": "confusing"
        });

        let rec = IndexRecord::from_json_with_id("k2".to_string(), &doc);

        // We expect "type" to win over "kind" if both exist.
        assert_eq!(rec.kind.as_deref(), Some("post"));
        assert_eq!(rec.slug.as_deref(), Some("confusing"));
    }

    #[test]
    fn index_record_arrays_and_scalars_for_tax_and_author_are_normalized() {
        let doc = json!({
            "tax": {
                "categories": "single-cat",
                "tags": ["one", "two", 3],
                "series": ["series1"]
            },
            "author": {
                "author": "Alice",
                "co_authors": "Bob"
            }
        });

        let rec = IndexRecord::from_json_with_id("norm".to_string(), &doc);

        // categories: scalar string -> ["single-cat"]
        assert_eq!(rec.tax.categories, vec!["single-cat".to_string()]);

        // tags: mixed array -> only string elements kept
        assert_eq!(rec.tax.tags, vec!["one".to_string(), "two".to_string()]);

        // series: array of strings unchanged
        assert_eq!(rec.tax.series, vec!["series1".to_string()]);

        // author.author is scalar
        assert_eq!(rec.author.author.as_deref(), Some("Alice"));

        // co_authors: scalar string -> ["Bob"]
        assert_eq!(rec.author.co_authors, vec!["Bob".to_string()]);
    }

    #[test]
    fn index_record_ignores_non_string_non_array_tax_values() {
        let doc = json!({
            "tax": {
                "categories": 123,
                "tags": { "not": "an array" },
                "series": null
            }
        });

        let rec = IndexRecord::from_json_with_id("bad-tax".to_string(), &doc);

        // All should become empty vecs when types are unsupported.
        assert!(rec.tax.categories.is_empty());
        assert!(rec.tax.tags.is_empty());
        assert!(rec.tax.series.is_empty());
    }

    #[test]
    fn index_record_i18n_and_nav_parsing_with_wrong_types() {
        let doc = json!({
            "nav": {
                "menu_order": "not-a-number",
                "menu_visible": "not-a-bool"
            },
            "i18n": {
                "lang": 123,
                "canonical_id": false
            }
        });

        let rec = IndexRecord::from_json_with_id("weird-nav-i18n".to_string(), &doc);

        // menu_order: wrong type -> None
        assert_eq!(rec.nav.menu_order, None);
        // menu_visible: wrong type -> None
        assert_eq!(rec.nav.menu_visible, None);

        // lang: wrong type -> None
        assert_eq!(rec.i18n.lang, None);
        // canonical_id: wrong type -> None
        assert_eq!(rec.i18n.canonical_id, None);
    }

    // ─────────────────────────────────────────────────────────────
    // From<(&str, &Json)> and From<(String, &Json)> tests
    // ─────────────────────────────────────────────────────────────

    #[test]
    fn from_str_and_string_tuple_constructors_work() {
        let doc = json!({ "type": "post", "slug": "tuple" });

        let rec1: IndexRecord = ("id-str", &doc).into();
        assert_eq!(rec1.id, "id-str");
        assert_eq!(rec1.kind.as_deref(), Some("post"));
        assert_eq!(rec1.slug.as_deref(), Some("tuple"));

        let rec2: IndexRecord = ("id-owned".to_string(), &doc).into();
        assert_eq!(rec2.id, "id-owned");
        assert_eq!(rec2.kind.as_deref(), Some("post"));
        assert_eq!(rec2.slug.as_deref(), Some("tuple"));
    }

    // ─────────────────────────────────────────────────────────────
    // JsonStore + IndexBackend minimal sanity tests
    // ─────────────────────────────────────────────────────────────

    #[derive(Debug)]
    struct TestStore {
        docs: Vec<Json>,
    }

    #[async_trait]
    impl JsonStore for TestStore {
        type Id = usize;

        async fn all_ids(&self) -> Vec<Self::Id> {
            (0..self.docs.len()).collect()
        }

        async fn get(&self, id: Self::Id) -> Option<Json> {
            self.docs.get(id).cloned()
        }
    }

    #[derive(Debug)]
    struct TestBackend {
        // very small fake index: field -> Json -> ids
        eq_index:
            std::collections::HashMap<String, std::collections::HashMap<Json, HashSet<usize>>>,
    }

    impl TestBackend {
        fn new() -> Self {
            Self {
                eq_index: std::collections::HashMap::new(),
            }
        }

        fn insert_eq(&mut self, field: &str, value: Json, id: usize) {
            let field_map = self.eq_index.entry(field.to_string()).or_default();
            let ids = field_map.entry(value).or_default();
            ids.insert(id);
        }
    }

    #[async_trait]
    impl IndexBackend for TestBackend {
        type Id = usize;

        async fn lookup_eq(&self, field: &str, value: &Json) -> Option<HashSet<Self::Id>> {
            let field_map = self.eq_index.get(field)?;
            field_map.get(value).cloned()
        }

        async fn lookup_in(&self, field: &str, values: &[Json]) -> Option<HashSet<Self::Id>> {
            let field_map = self.eq_index.get(field)?;
            let mut acc = HashSet::new();
            for v in values {
                if let Some(ids) = field_map.get(v) {
                    acc.extend(ids.iter().copied());
                }
            }
            if acc.is_empty() {
                None
            } else {
                Some(acc)
            }
        }

        async fn lookup_range(
            &self,
            _field: &str,
            _min: Option<&Json>,
            _max: Option<&Json>,
        ) -> Option<HashSet<Self::Id>> {
            // Test backend does not support range.
            None
        }
    }

    #[tokio::test]
    async fn teststore_and_testbackend_basic_behavior() {
        let docs = vec![
            json!({ "type": "post", "slug": "a" }),
            json!({ "type": "post", "slug": "b" }),
            json!({ "type": "page", "slug": "c" }),
        ];

        let store = TestStore { docs };

        // JsonStore::all_ids
        let ids = store.all_ids().await;
        assert_eq!(ids, vec![0, 1, 2]);

        // JsonStore::get
        assert_eq!(store.get(1).await.unwrap()["slug"].as_str(), Some("b"));
        assert!(store.get(999).await.is_none());

        // IndexBackend: index on "type"
        let mut backend = TestBackend::new();
        backend.insert_eq("type", json!("post"), 0);
        backend.insert_eq("type", json!("post"), 1);
        backend.insert_eq("type", json!("page"), 2);

        // lookup_eq positive
        let posts = backend.lookup_eq("type", &json!("post")).await.unwrap();
        assert_eq!(posts.len(), 2);
        assert!(posts.contains(&0));
        assert!(posts.contains(&1));

        // lookup_eq negative
        assert!(backend.lookup_eq("type", &json!("missing")).await.is_none());

        // lookup_in union
        let mixed = backend
            .lookup_in("type", &[json!("post"), json!("page")])
            .await
            .unwrap();
        assert_eq!(mixed.len(), 3);

        // lookup_in with no matches
        assert!(backend
            .lookup_in("type", &[json!("does-not-exist")])
            .await
            .is_none());

        // lookup_range not supported
        assert!(backend.lookup_range("type", None, None).await.is_none());
    }
}
