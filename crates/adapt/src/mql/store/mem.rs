use async_trait::async_trait;
use serde_json::Value as Json;
use std::collections::{HashMap, HashSet};
use std::fmt::Debug;

use crate::mql::{IndexBackend, IndexConfig, JsonStore};

// ─────────────────────────────────────────────────────────────────────────────
// In-memory JsonStore implementation
// ─────────────────────────────────────────────────────────────────────────────

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

#[async_trait]
impl JsonStore for InMemoryJsonStore {
    type Id = usize;

    async fn all_ids(&self) -> Vec<Self::Id> {
        (0..self.docs.len()).collect()
    }

    async fn get(&self, id: Self::Id) -> Option<Json> {
        self.docs.get(id).cloned()
    }
}

/// Convert a JSON value into an index key string.
///
/// For equality lookups we just use the JSON string representation; for a
/// more robust disk-backed integration, you might:
/// - normalize case,
/// - use typed encodings,
/// - or delegate to the underlying DB’s index key representation.
fn value_to_index_key(v: &Json) -> String {
    v.to_string()
}

// ─────────────────────────────────────────────────────────────────────────────
// In-memory IndexBackend implementation
// ─────────────────────────────────────────────────────────────────────────────

/// In-memory index backend.
///
/// For each indexed field, we build:
///   field -> (value string) -> set of IDs
///
/// NOTE: we index the JSON's string representation for equality/membership.
/// For a disk-backed integration, you’d replace this implementation with one
/// backed by its index structures.
#[derive(Debug, Clone)]
pub struct InMemoryIndexBackend {
    pub field_value_to_ids: HashMap<String, HashMap<String, HashSet<usize>>>,
}

impl InMemoryIndexBackend {
    /// Build an in-memory index for the given config and store.
    pub async fn build(config: &IndexConfig, store: &InMemoryJsonStore) -> Self {
        use crate::mql::eval::get_field_value;

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

#[async_trait]
impl IndexBackend for InMemoryIndexBackend {
    type Id = usize;

    async fn lookup_eq(&self, field: &str, value: &Json) -> Option<HashSet<Self::Id>> {
        let key = value_to_index_key(value);
        let field_map = self.field_value_to_ids.get(field)?;
        field_map.get(&key).cloned()
    }

    async fn lookup_in(&self, field: &str, values: &[Json]) -> Option<HashSet<Self::Id>> {
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

    async fn lookup_range(
        &self,
        _field: &str,
        _min: Option<&Json>,
        _max: Option<&Json>,
    ) -> Option<HashSet<Self::Id>> {
        // range queries not supported by in-memory backend (yet)
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use tokio;

    use crate::mql::IndexConfig;

    // ─────────────────────────────────────────────────────────────
    // InMemoryJsonStore tests
    // ─────────────────────────────────────────────────────────────

    #[test]
    fn inmemory_store_len_and_is_empty_reflect_docs() {
        let empty_store = InMemoryJsonStore::new(vec![]);
        assert_eq!(empty_store.len(), 0);
        assert!(empty_store.is_empty());

        let store = InMemoryJsonStore::new(vec![json!({"slug": "a"}), json!({"slug": "b"})]);
        assert_eq!(store.len(), 2);
        assert!(!store.is_empty());
    }

    #[tokio::test]
    async fn inmemory_store_all_ids_on_empty_and_non_empty() {
        let empty_store = InMemoryJsonStore::new(vec![]);
        let ids = empty_store.all_ids().await;
        assert!(ids.is_empty());

        let store = InMemoryJsonStore::new(vec![
            json!({"slug": "a"}),
            json!({"slug": "b"}),
            json!({"slug": "c"}),
        ]);
        let ids = store.all_ids().await;
        assert_eq!(ids, vec![0, 1, 2]);
    }

    #[tokio::test]
    async fn inmemory_store_get_in_range_and_out_of_range() {
        let store = InMemoryJsonStore::new(vec![json!({"slug": "a"}), json!({"slug": "b"})]);

        // In range
        let doc0 = store.get(0).await.expect("id 0 should exist");
        assert_eq!(doc0["slug"], "a");

        let doc1 = store.get(1).await.expect("id 1 should exist");
        assert_eq!(doc1["slug"], "b");

        // Out of range
        assert!(store.get(2).await.is_none());
        assert!(store.get(999).await.is_none());
    }

    // ─────────────────────────────────────────────────────────────
    // value_to_index_key tests (indirect key normalization)
    // ─────────────────────────────────────────────────────────────

    #[test]
    fn value_to_index_key_uses_json_string_representation() {
        // Scalars
        assert_eq!(super::value_to_index_key(&json!("text")), "\"text\"");
        assert_eq!(super::value_to_index_key(&json!(42)), "42");
        assert_eq!(super::value_to_index_key(&json!(true)), "true");

        // Arrays and objects are stringified JSON
        assert_eq!(super::value_to_index_key(&json!([1, 2, 3])), "[1,2,3]");
        assert_eq!(
            super::value_to_index_key(&json!({"a": 1, "b": 2})),
            r#"{"a":1,"b":2}"#
        );
    }

    // ─────────────────────────────────────────────────────────────
    // InMemoryIndexBackend::build tests
    // ─────────────────────────────────────────────────────────────

    #[tokio::test]
    async fn build_creates_index_for_configured_fields_only() {
        let docs = vec![
            json!({ "type": "post", "slug": "a" }),
            json!({ "type": "post", "slug": "b" }),
            json!({ "type": "page", "slug": "c" }),
        ];

        let store = InMemoryJsonStore::new(docs);
        // Only index "type" and "slug", not "nonexistent"
        let config = IndexConfig::new(["type", "slug"]);

        let backend = InMemoryIndexBackend::build(&config, &store).await;

        // "type" should have two distinct keys: "post" and "page"
        let type_map = backend
            .field_value_to_ids
            .get("type")
            .expect("type field should be indexed");

        assert_eq!(type_map.len(), 2);
        let post_key = super::value_to_index_key(&json!("post"));
        let page_key = super::value_to_index_key(&json!("page"));
        assert!(type_map.contains_key(&post_key));
        assert!(type_map.contains_key(&page_key));

        // "slug" should map "a", "b", "c" to single ids each
        let slug_map = backend
            .field_value_to_ids
            .get("slug")
            .expect("slug field should be indexed");
        assert_eq!(slug_map.len(), 3);

        let a_key = super::value_to_index_key(&json!("a"));
        let b_key = super::value_to_index_key(&json!("b"));
        let c_key = super::value_to_index_key(&json!("c"));

        assert_eq!(
            slug_map
                .get(&a_key)
                .unwrap()
                .iter()
                .copied()
                .collect::<Vec<_>>(),
            vec![0]
        );
        assert_eq!(
            slug_map
                .get(&b_key)
                .unwrap()
                .iter()
                .copied()
                .collect::<Vec<_>>(),
            vec![1]
        );
        assert_eq!(
            slug_map
                .get(&c_key)
                .unwrap()
                .iter()
                .copied()
                .collect::<Vec<_>>(),
            vec![2]
        );

        // A field that isn't in the config should not be indexed
        assert!(backend.field_value_to_ids.get("draft").is_none());
    }

    #[tokio::test]
    async fn build_handles_multiple_docs_with_same_value() {
        let docs = vec![
            json!({ "type": "post", "slug": "a" }),
            json!({ "type": "post", "slug": "b" }),
            json!({ "type": "post", "slug": "c" }),
        ];

        let store = InMemoryJsonStore::new(docs);
        let config = IndexConfig::new(["type"]); // only index "type"

        let backend = InMemoryIndexBackend::build(&config, &store).await;

        let type_map = backend
            .field_value_to_ids
            .get("type")
            .expect("type field should be indexed");

        let post_key = super::value_to_index_key(&json!("post"));
        let ids = type_map
            .get(&post_key)
            .expect("post key should exist in type index");

        let mut ids_vec: Vec<_> = ids.iter().copied().collect();
        ids_vec.sort_unstable();
        assert_eq!(ids_vec, vec![0, 1, 2]);
    }

    // ─────────────────────────────────────────────────────────────
    // IndexBackend impl: lookup_eq / lookup_in / lookup_range
    // ─────────────────────────────────────────────────────────────

    #[tokio::test]
    async fn lookup_eq_finds_matching_ids() {
        let docs = vec![
            json!({ "type": "post", "slug": "a" }),
            json!({ "type": "post", "slug": "b" }),
            json!({ "type": "page", "slug": "c" }),
        ];

        let store = InMemoryJsonStore::new(docs);
        let config = IndexConfig::new(["type", "slug"]);

        let backend = InMemoryIndexBackend::build(&config, &store).await;

        // Positive: type == "post"
        let posts = backend.lookup_eq("type", &json!("post")).await.unwrap();
        let mut ids: Vec<_> = posts.into_iter().collect();
        ids.sort_unstable();
        assert_eq!(ids, vec![0, 1]);

        // Positive: slug == "c"
        let c_ids = backend.lookup_eq("slug", &json!("c")).await.unwrap();
        let ids: Vec<_> = c_ids.into_iter().collect();
        assert_eq!(ids, vec![2]);

        // Negative: type == "missing"
        assert!(backend.lookup_eq("type", &json!("missing")).await.is_none());

        // Negative: unknown field
        assert!(backend
            .lookup_eq("nonexistent_field", &json!("whatever"))
            .await
            .is_none());
    }

    #[tokio::test]
    async fn lookup_in_unions_across_values() {
        let docs = vec![
            json!({ "slug": "a" }),
            json!({ "slug": "b" }),
            json!({ "slug": "c" }),
            json!({ "slug": "d" }),
        ];

        let store = InMemoryJsonStore::new(docs);
        let config = IndexConfig::new(["slug"]);

        let backend = InMemoryIndexBackend::build(&config, &store).await;

        // IN ["a", "c", "x"] should return ids 0 and 2
        let result = backend
            .lookup_in("slug", &[json!("a"), json!("c"), json!("x")])
            .await
            .unwrap();
        let mut ids: Vec<_> = result.into_iter().collect();
        ids.sort_unstable();
        assert_eq!(ids, vec![0, 2]);

        // IN with overlapping sets (["a","b"], ["b","c"] equivalent)
        let result2 = backend
            .lookup_in("slug", &[json!("b"), json!("c")])
            .await
            .unwrap();
        let mut ids2: Vec<_> = result2.into_iter().collect();
        ids2.sort_unstable();
        assert_eq!(ids2, vec![1, 2]);

        // IN with no hits should return None
        assert!(backend
            .lookup_in("slug", &[json!("x"), json!("y")])
            .await
            .is_none());

        // Unknown field => None
        assert!(backend
            .lookup_in("nonexistent_field", &[json!("a")])
            .await
            .is_none());
    }

    #[tokio::test]
    async fn lookup_range_is_not_supported() {
        let docs = vec![json!({ "order": 1 }), json!({ "order": 2 })];

        let store = InMemoryJsonStore::new(docs);
        let config = IndexConfig::new(["order"]);
        let backend = InMemoryIndexBackend::build(&config, &store).await;

        // No matter what we pass, this backend does not support ranges.
        assert!(backend
            .lookup_range("order", Some(&json!(1)), Some(&json!(2)))
            .await
            .is_none());
        assert!(backend.lookup_range("order", None, None).await.is_none());
        assert!(backend
            .lookup_range("nonexistent_field", Some(&json!(1)), Some(&json!(2)))
            .await
            .is_none());
    }
}
