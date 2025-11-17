use serde_json::Value as Json;
use std::collections::{HashMap, HashSet};
use std::hash::Hash;

/// Configuration: which fields are indexed.
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
    ///     "kind",
    ///     "slug",
    ///     "front_matter.date",
    ///     "front_matter.tags",
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

/// Abstraction over a JSON document store (in-memory or disk-backed).
///
/// The store is responsible for:
/// - enumerating all document IDs
/// - returning an owned JSON document for a given ID
///
/// Returning owned `Json` (rather than `&Json`) keeps this usable for both
/// in-memory and disk-backed implementations (e.g. `indexed_json`).
pub trait JsonStore {
    /// Document ID type (e.g. usize for in-memory, u64 for a DB).
    type Id: Copy + Eq + Hash;

    /// Get all document IDs.
    fn all_ids(&self) -> Vec<Self::Id>;

    /// Get a document by ID (owned JSON).
    fn get(&self, id: Self::Id) -> Option<Json>;
}

/// Simple in-memory store of JSON documents.
///
/// This is primarily a reference implementation and is also useful for tests.
#[derive(Debug, Clone)]
pub struct InMemoryJsonStore {
    pub docs: Vec<Json>,
}

impl InMemoryJsonStore {
    pub fn new(docs: Vec<Json>) -> Self {
        Self { docs }
    }

    pub fn len(&self) -> usize {
        self.docs.len()
    }

    pub fn is_empty(&self) -> bool {
        self.docs.is_empty()
    }
}

impl JsonStore for InMemoryJsonStore {
    type Id = usize;

    fn all_ids(&self) -> Vec<Self::Id> {
        (0..self.docs.len()).collect()
    }

    fn get(&self, id: Self::Id) -> Option<Json> {
        self.docs.get(id).cloned()
    }
}

/// Index backend API: equality / membership / range lookups on indexed fields.
///
/// Implementations are free to ignore any part of this contract by returning
/// `None` (meaning "I don't have an index for this predicate").
pub trait IndexBackend {
    type Id: Copy + Eq + Hash;

    /// Lookup IDs for `field == value`.
    ///
    /// Returns:
    /// - `Some(HashSet<Id>)` if the field is indexed and the index can answer
    ///   this predicate.
    /// - `None` if the field is not indexed or this backend cannot answer it.
    fn lookup_eq(&self, field: &str, value: &Json) -> Option<HashSet<Self::Id>>;

    /// Lookup IDs for `field IN values` (union).
    ///
    /// Returns:
    /// - `Some(HashSet<Id>)` if the field is indexed and the index can answer
    ///   this predicate.
    /// - `None` if the field is not indexed or this backend cannot answer it.
    fn lookup_in(&self, field: &str, values: &[Json]) -> Option<HashSet<Self::Id>>;

    /// Range lookups (optional).
    ///
    /// Returns:
    /// - `Some(HashSet<Id>)` if the backend can handle the range query.
    /// - `None` if not supported / not indexed.
    fn lookup_range(
        &self,
        _field: &str,
        _min: Option<&Json>,
        _max: Option<&Json>,
    ) -> Option<HashSet<Self::Id>> {
        None
    }
}

/// In-memory index backend.
///
/// For each indexed field, we build:
///   field -> (value string) -> set of IDs
///
/// NOTE: we index the JSON's string representation for equality/membership.
/// For a real `indexed_json` integration, you’d replace this implementation
/// with one backed by its index structures.
#[derive(Debug, Clone)]
pub struct InMemoryIndexBackend {
    pub field_value_to_ids: HashMap<String, HashMap<String, HashSet<usize>>>,
}

impl InMemoryIndexBackend {
    /// Build an in-memory index for the given config and store.
    pub fn build(config: &IndexConfig, store: &InMemoryJsonStore) -> Self {
        use super::eval::get_field_value;

        let mut field_value_to_ids: HashMap<String, HashMap<String, HashSet<usize>>> =
            HashMap::new();

        for (id, doc) in store.docs.iter().enumerate() {
            for field in config.fields() {
                if let Some(value) = get_field_value(doc, field) {
                    let key = value_to_index_key(value);
                    let field_map = field_value_to_ids.entry(field.to_string()).or_default();
                    let ids = field_map.entry(key).or_default();
                    ids.insert(id);
                }
            }
        }

        Self { field_value_to_ids }
    }
}

impl IndexBackend for InMemoryIndexBackend {
    type Id = usize;

    fn lookup_eq(&self, field: &str, value: &Json) -> Option<HashSet<Self::Id>> {
        let key = value_to_index_key(value);
        let field_map = self.field_value_to_ids.get(field)?;
        field_map.get(&key).cloned()
    }

    fn lookup_in(&self, field: &str, values: &[Json]) -> Option<HashSet<Self::Id>> {
        let field_map = self.field_value_to_ids.get(field)?;
        let mut acc: HashSet<Self::Id> = HashSet::new();
        for v in values {
            let key = value_to_index_key(v);
            if let Some(ids) = field_map.get(&key) {
                acc.extend(ids.iter().copied());
            }
        }
        if acc.is_empty() {
            None
        } else {
            Some(acc)
        }
    }
}

/// Convert a JSON value into an index key string.
///
/// For equality lookups we just use the JSON string representation; for a
/// more robust indexed_json integration, you'd use whatever its index key
/// representation is.
fn value_to_index_key(v: &Json) -> String {
    v.to_string()
}
