// crates/adapt/src/mql/index.rs
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
