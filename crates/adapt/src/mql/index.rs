use serde_json::Value as Json;
use std::collections::{HashMap, HashSet};

/// Configuration: which fields are indexed.
#[derive(Debug, Clone)]
pub struct IndexConfig {
    pub indexed_fields: Vec<String>,
}

impl IndexConfig {
    pub fn new(indexed_fields: Vec<String>) -> Self {
        Self { indexed_fields }
    }

    pub fn is_indexed(&self, field: &str) -> bool {
        self.indexed_fields.iter().any(|f| f == field)
    }
}

/// Abstraction over a JSON document store.
pub trait JsonStore {
    /// Document ID type; here we use u64.
    type Id: Copy + Eq + std::hash::Hash;

    /// Get all document IDs.
    fn all_ids(&self) -> Vec<Self::Id>;

    /// Get a document by ID.
    fn get(&self, id: Self::Id) -> Option<&Json>;
}

/// Simple in-memory JsonStore for testing.
pub struct InMemoryJsonStore {
    docs: Vec<Json>,
}

impl InMemoryJsonStore {
    pub fn new(docs: Vec<Json>) -> Self {
        Self { docs }
    }
}

impl JsonStore for InMemoryJsonStore {
    type Id = usize;

    fn all_ids(&self) -> Vec<Self::Id> {
        (0..self.docs.len()).collect()
    }

    fn get(&self, id: Self::Id) -> Option<&Json> {
        self.docs.get(id)
    }
}

/// Index backend API: eq / in / range lookups on indexed fields.
pub trait IndexBackend {
    type Id: Copy + Eq + std::hash::Hash;

    /// Lookup IDs for field == value.
    fn lookup_eq(&self, field: &str, value: &Json) -> Option<HashSet<Self::Id>>;

    /// Lookup IDs for field IN values (union).
    fn lookup_in(&self, field: &str, values: &[Json]) -> Option<HashSet<Self::Id>>;

    /// Range lookups (optional for v1).
    fn lookup_range(
        &self,
        _field: &str,
        _min: Option<&Json>,
        _max: Option<&Json>,
    ) -> Option<HashSet<Self::Id>> {
        None
    }
}

/// Simple in-memory index backend for testing / reference.
///
/// For each indexed field, we build:
///   field -> (value string) -> set of IDs
///
/// NOTE: we index the JSON's string representation for equality/membership.
/// For real use with `indexed_json`, you'd replace this implementation with
/// one backed by its index structures.
pub struct InMemoryIndexBackend {
    pub field_value_to_ids: HashMap<String, HashMap<String, HashSet<usize>>>,
}

impl InMemoryIndexBackend {
    pub fn build(config: &IndexConfig, store: &InMemoryJsonStore) -> Self {
        use super::eval::get_field_value;

        let mut field_value_to_ids: HashMap<String, HashMap<String, HashSet<usize>>> =
            HashMap::new();

        for (id, doc) in store.docs.iter().enumerate() {
            for field in &config.indexed_fields {
                if let Some(value) = get_field_value(doc, field) {
                    let key = value_to_index_key(value);
                    let field_map = field_value_to_ids.entry(field.clone()).or_default();
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
