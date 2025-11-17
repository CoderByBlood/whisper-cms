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

    // `lookup_range` keeps the default `None` implementation for now.
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
// indexed_json-compatible backend (skeleton)
// ─────────────────────────────────────────────────────────────────────────────

/// Minimal adapter over an indexed JSON database.
///
/// You will implement this for your concrete `indexed_json` handle type,
/// e.g.:
///
/// ```ignore
/// struct MyIndexedJsonDb { /* your DB handle(s) here */ }
///
/// impl IndexedJsonApi for MyIndexedJsonDb {
///     type Id = u64;
///
///     fn all_ids(&self) -> Vec<Self::Id> { /* ... */ }
///     fn get_json(&self, id: Self::Id) -> Option<Json> { /* ... */ }
///     fn lookup_eq(&self, field: &str, value: &Json) -> Option<HashSet<Self::Id>> { /* ... */ }
///     fn lookup_in(&self, field: &str, values: &[Json]) -> Option<HashSet<Self::Id>> { /* ... */ }
///     fn lookup_range(
///         &self,
///         field: &str,
///         min: Option<&Json>,
///         max: Option<&Json>,
///     ) -> Option<HashSet<Self::Id>> {
///         // optional
///         None
///     }
/// }
/// ```
pub trait IndexedJsonApi {
    /// Document identifier type in the underlying DB.
    type Id: Copy + Eq + Hash;

    /// Return all document IDs.
    fn all_ids(&self) -> Vec<Self::Id>;

    /// Load a document as serde_json::Value.
    fn get_json(&self, id: Self::Id) -> Option<Json>;

    /// Index-backed equality lookup: field == value.
    fn lookup_eq(&self, field: &str, value: &Json) -> Option<HashSet<Self::Id>>;

    /// Index-backed membership lookup: field IN values.
    fn lookup_in(&self, field: &str, values: &[Json]) -> Option<HashSet<Self::Id>>;

    /// Optional: range lookup.
    fn lookup_range(
        &self,
        _field: &str,
        _min: Option<&Json>,
        _max: Option<&Json>,
    ) -> Option<HashSet<Self::Id>> {
        None
    }
}

/// JsonStore implementation backed by an IndexedJsonApi database.
///
/// `DB` is typically an `Arc<MyIndexedJsonDb>` so that clones are cheap.
#[derive(Debug, Clone)]
pub struct IndexedJsonStore<DB> {
    pub db: DB,
}

impl<DB> IndexedJsonStore<DB> {
    pub fn new(db: DB) -> Self {
        Self { db }
    }
}

impl<DB> JsonStore for IndexedJsonStore<DB>
where
    DB: IndexedJsonApi,
{
    type Id = DB::Id;

    fn all_ids(&self) -> Vec<Self::Id> {
        self.db.all_ids()
    }

    fn get(&self, id: Self::Id) -> Option<Json> {
        self.db.get_json(id)
    }
}

/// IndexBackend implementation backed by IndexedJsonApi.
///
/// This delegates directly to the underlying DB for indexed lookups, and
/// uses `IndexConfig` only to decide *whether* a field should be queried
/// via the index at all.
#[derive(Debug, Clone)]
pub struct IndexedJsonIndexBackend<DB> {
    pub db: DB,
    pub config: IndexConfig,
}

impl<DB> IndexedJsonIndexBackend<DB> {
    pub fn new(db: DB, config: IndexConfig) -> Self {
        Self { db, config }
    }

    pub fn index_config(&self) -> &IndexConfig {
        &self.config
    }
}

impl<DB> IndexBackend for IndexedJsonIndexBackend<DB>
where
    DB: IndexedJsonApi,
{
    type Id = DB::Id;

    fn lookup_eq(&self, field: &str, value: &Json) -> Option<HashSet<Self::Id>> {
        if !self.config.is_indexed(field) {
            return None;
        }
        self.db.lookup_eq(field, value)
    }

    fn lookup_in(&self, field: &str, values: &[Json]) -> Option<HashSet<Self::Id>> {
        if !self.config.is_indexed(field) {
            return None;
        }
        self.db.lookup_in(field, values)
    }

    fn lookup_range(
        &self,
        field: &str,
        min: Option<&Json>,
        max: Option<&Json>,
    ) -> Option<HashSet<Self::Id>> {
        if !self.config.is_indexed(field) {
            return None;
        }
        self.db.lookup_range(field, min, max)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{json, Value as Json};
    use std::cell::RefCell;

    // ─────────────────────────────────────────────────────────────────────
    // IndexConfig tests
    // ─────────────────────────────────────────────────────────────────────

    #[test]
    fn indexconfig_new_and_is_indexed() {
        let cfg = IndexConfig::new(["kind", "slug", "front_matter.date", "front_matter.tags"]);

        assert!(cfg.is_indexed("kind"));
        assert!(cfg.is_indexed("front_matter.date"));
        assert!(!cfg.is_indexed("draft"));
        assert!(!cfg.is_indexed("nonexistent"));
    }

    #[test]
    fn indexconfig_fields_iterates_all_fields() {
        let cfg = IndexConfig::new(["a", "b", "c"]);
        let mut fields: Vec<_> = cfg.fields().collect();
        fields.sort(); // HashSet iteration order is undefined

        assert_eq!(fields, vec!["a", "b", "c"]);
    }

    #[test]
    fn indexconfig_empty_has_no_indexed_fields() {
        let cfg = IndexConfig::new(Vec::<&str>::new());
        assert!(!cfg.is_indexed("anything"));
        assert_eq!(cfg.fields().count(), 0);
    }

    // ─────────────────────────────────────────────────────────────────────
    // InMemoryJsonStore tests
    // ─────────────────────────────────────────────────────────────────────

    #[test]
    fn inmemoryjsonstore_len_and_is_empty() {
        let empty = InMemoryJsonStore::new(Vec::new());
        assert_eq!(empty.len(), 0);
        assert!(empty.is_empty());

        let store = InMemoryJsonStore::new(vec![json!({ "id": 1 }), json!({ "id": 2 })]);
        assert_eq!(store.len(), 2);
        assert!(!store.is_empty());
    }

    #[test]
    fn inmemoryjsonstore_all_ids_and_get() {
        let store = InMemoryJsonStore::new(vec![
            json!({ "id": 10 }),
            json!({ "id": 11 }),
            json!({ "id": 12 }),
        ]);

        let ids = store.all_ids();
        assert_eq!(ids, vec![0, 1, 2]);

        let doc0 = store.get(0).expect("doc 0 must exist");
        let doc1 = store.get(1).expect("doc 1 must exist");
        let doc2 = store.get(2).expect("doc 2 must exist");
        assert_eq!(doc0["id"], json!(10));
        assert_eq!(doc1["id"], json!(11));
        assert_eq!(doc2["id"], json!(12));

        // Out of bounds
        assert!(store.get(3).is_none());
        assert!(store.get(999).is_none());
    }

    #[test]
    fn inmemoryjsonstore_get_returns_owned_json() {
        let store = InMemoryJsonStore::new(vec![json!({ "value": 1 })]);

        let mut doc = store.get(0).expect("doc must exist");
        assert_eq!(doc["value"], json!(1));

        // Modify the returned JSON and ensure store's internal data is unaffected.
        doc["value"] = json!(999);

        let original = store.get(0).expect("doc must exist again");
        assert_eq!(
            original["value"],
            json!(1),
            "store must not be mutated by changes to returned Json"
        );
    }

    // ─────────────────────────────────────────────────────────────────────
    // value_to_index_key tests
    // ─────────────────────────────────────────────────────────────────────

    #[test]
    fn value_to_index_key_uses_json_string_representation() {
        // Strings
        assert_eq!(value_to_index_key(&json!("post")), "\"post\"");

        // Numbers
        assert_eq!(value_to_index_key(&json!(42)), "42");

        // Bool
        assert_eq!(value_to_index_key(&json!(true)), "true");

        // Null
        assert_eq!(value_to_index_key(&Json::Null), "null");

        // Arrays
        assert_eq!(value_to_index_key(&json!(["a", "b"])), "[\"a\",\"b\"]");

        // Objects (ordering may differ but serde_json has stable ordering for
        // construction via json! macro with literal keys).
        assert_eq!(value_to_index_key(&json!({ "k": "v" })), "{\"k\":\"v\"}");
    }

    // ─────────────────────────────────────────────────────────────────────
    // InMemoryIndexBackend::build and lookups
    // ─────────────────────────────────────────────────────────────────────

    #[test]
    fn inmemoryindexbackend_build_creates_index_entries() {
        // Three docs with different tag arrays.
        let store = InMemoryJsonStore::new(vec![
            json!({ "kind": "post", "front_matter": { "tags": ["rust", "wasm"] } }),
            json!({ "kind": "page", "front_matter": { "tags": ["rust"] } }),
            json!({ "kind": "post", "front_matter": { "tags": ["other"] } }),
        ]);

        let config = IndexConfig::new(["kind", "front_matter.tags"]);
        let backend = InMemoryIndexBackend::build(&config, &store);

        // kind == "post" should map to IDs {0, 2}
        let key_post = value_to_index_key(&json!("post"));
        let kind_map = backend
            .field_value_to_ids
            .get("kind")
            .expect("kind field must be indexed");
        let ids_for_post = kind_map
            .get(&key_post)
            .expect("there must be an entry for kind=post");
        let mut ids_vec: Vec<_> = ids_for_post.iter().copied().collect();
        ids_vec.sort();
        assert_eq!(ids_vec, vec![0, 2]);

        // kind == "page" should map to ID {1}
        let key_page = value_to_index_key(&json!("page"));
        let ids_for_page = kind_map
            .get(&key_page)
            .expect("there must be an entry for kind=page");
        let mut ids_vec: Vec<_> = ids_for_page.iter().copied().collect();
        ids_vec.sort();
        assert_eq!(ids_vec, vec![1]);

        // front_matter.tags are indexed as full arrays, not individual elements.

        let tags_map = backend
            .field_value_to_ids
            .get("front_matter.tags")
            .expect("tags field must be indexed");

        // Doc 0: ["rust", "wasm"]
        let key_rust_wasm = value_to_index_key(&json!(["rust", "wasm"]));
        let ids_for_rust_wasm = tags_map
            .get(&key_rust_wasm)
            .expect("there must be an entry for tags=[\"rust\",\"wasm\"]");
        let mut ids_vec: Vec<_> = ids_for_rust_wasm.iter().copied().collect();
        ids_vec.sort();
        assert_eq!(ids_vec, vec![0]);

        // Doc 1: ["rust"]
        let key_rust = value_to_index_key(&json!(["rust"]));
        let ids_for_rust = tags_map
            .get(&key_rust)
            .expect("there must be an entry for tags=[\"rust\"]");
        let mut ids_vec: Vec<_> = ids_for_rust.iter().copied().collect();
        ids_vec.sort();
        assert_eq!(ids_vec, vec![1]);

        // Doc 2: ["other"]
        let key_other = value_to_index_key(&json!(["other"]));
        let ids_for_other = tags_map
            .get(&key_other)
            .expect("there must be an entry for tags=[\"other\"]");
        let mut ids_vec: Vec<_> = ids_for_other.iter().copied().collect();
        ids_vec.sort();
        assert_eq!(ids_vec, vec![2]);
    }

    #[test]
    fn inmemoryindexbackend_lookup_eq_hits_and_misses() {
        let store = InMemoryJsonStore::new(vec![
            json!({ "kind": "post" }),
            json!({ "kind": "page" }),
            json!({ "kind": "post" }),
        ]);

        let config = IndexConfig::new(["kind"]);
        let backend = InMemoryIndexBackend::build(&config, &store);

        // Hit: kind == "post" -> {0, 2}
        let ids = backend
            .lookup_eq("kind", &json!("post"))
            .expect("kind=post should be indexed");
        let mut ids_vec: Vec<_> = ids.into_iter().collect();
        ids_vec.sort();
        assert_eq!(ids_vec, vec![0, 2]);

        // Hit: kind == "page" -> {1}
        let ids = backend
            .lookup_eq("kind", &json!("page"))
            .expect("kind=page should be indexed");
        let mut ids_vec: Vec<_> = ids.into_iter().collect();
        ids_vec.sort();
        assert_eq!(ids_vec, vec![1]);

        // Miss: kind == "unknown" -> None
        let ids = backend.lookup_eq("kind", &json!("unknown"));
        assert!(ids.is_none(), "unknown kind should not be present");
    }

    #[test]
    fn inmemoryindexbackend_lookup_in_unions_results_and_returns_none_when_empty() {
        let store = InMemoryJsonStore::new(vec![
            json!({ "kind": "post" }),
            json!({ "kind": "page" }),
            json!({ "kind": "draft" }),
            json!({ "kind": "post" }),
        ]);

        let config = IndexConfig::new(["kind"]);
        let backend = InMemoryIndexBackend::build(&config, &store);

        // Union of kind IN ["post", "page"] -> IDs {0,1,3}
        let ids = backend
            .lookup_in("kind", &[json!("post"), json!("page")])
            .expect("kind in [post,page] should have hits");
        let mut ids_vec: Vec<_> = ids.into_iter().collect();
        ids_vec.sort();
        assert_eq!(ids_vec, vec![0, 1, 3]);

        // IN with no matches -> None
        let ids = backend.lookup_in("kind", &[json!("unknown")]);
        assert!(ids.is_none(), "no matches should return None");
    }

    #[test]
    fn inmemoryindexbackend_lookup_range_default_is_none() {
        let backend = InMemoryIndexBackend {
            field_value_to_ids: HashMap::new(),
        };

        let res = backend.lookup_range("kind", Some(&json!(1)), Some(&json!(10)));
        assert!(res.is_none());
    }

    // ─────────────────────────────────────────────────────────────────────
    // IndexedJsonStore & IndexedJsonIndexBackend tests with a TestDb
    // ─────────────────────────────────────────────────────────────────────

    #[derive(Debug, Clone)]
    struct TestDb {
        ids: Vec<u64>,
        docs: HashMap<u64, Json>,
        eq_calls: RefCell<Vec<(String, String)>>,
        in_calls: RefCell<Vec<(String, Vec<String>)>>,
        range_calls: RefCell<Vec<(String, Option<String>, Option<String>)>>,
    }

    impl TestDb {
        fn new() -> Self {
            Self {
                ids: Vec::new(),
                docs: HashMap::new(),
                eq_calls: RefCell::new(Vec::new()),
                in_calls: RefCell::new(Vec::new()),
                range_calls: RefCell::new(Vec::new()),
            }
        }

        fn insert(&mut self, id: u64, doc: Json) {
            self.ids.push(id);
            self.docs.insert(id, doc);
        }
    }

    impl IndexedJsonApi for TestDb {
        type Id = u64;

        fn all_ids(&self) -> Vec<Self::Id> {
            self.ids.clone()
        }

        fn get_json(&self, id: Self::Id) -> Option<Json> {
            self.docs.get(&id).cloned()
        }

        fn lookup_eq(&self, field: &str, value: &Json) -> Option<HashSet<Self::Id>> {
            let key = value_to_index_key(value);
            self.eq_calls
                .borrow_mut()
                .push((field.to_string(), key.clone()));

            // For tests, we hardcode some behavior:
            // - field "kind", value "post" => all ids whose doc.kind == "post"
            if field == "kind" && value == &json!("post") {
                let mut out = HashSet::new();
                for (id, doc) in &self.docs {
                    if doc.get("kind") == Some(&json!("post")) {
                        out.insert(*id);
                    }
                }
                if out.is_empty() {
                    None
                } else {
                    Some(out)
                }
            } else {
                None
            }
        }

        fn lookup_in(&self, field: &str, values: &[Json]) -> Option<HashSet<Self::Id>> {
            let keys: Vec<String> = values.iter().map(|v| value_to_index_key(v)).collect();
            self.in_calls
                .borrow_mut()
                .push((field.to_string(), keys.clone()));

            // For tests: kind IN values -> union of kind == each value
            if field == "kind" {
                let mut out = HashSet::new();
                for v in values {
                    if v == &json!("post") || v == &json!("page") {
                        for (id, doc) in &self.docs {
                            if doc.get("kind") == Some(v) {
                                out.insert(*id);
                            }
                        }
                    }
                }
                if out.is_empty() {
                    None
                } else {
                    Some(out)
                }
            } else {
                None
            }
        }

        fn lookup_range(
            &self,
            field: &str,
            min: Option<&Json>,
            max: Option<&Json>,
        ) -> Option<HashSet<Self::Id>> {
            let min_s = min.map(value_to_index_key);
            let max_s = max.map(value_to_index_key);
            self.range_calls
                .borrow_mut()
                .push((field.to_string(), min_s, max_s));

            // For tests we do not support ranges -> always None
            None
        }
    }

    #[test]
    fn indexedjsonstore_delegates_to_db_for_all_ids_and_get() {
        let mut db = TestDb::new();
        db.insert(1, json!({ "kind": "post" }));
        db.insert(2, json!({ "kind": "page" }));

        let store = IndexedJsonStore::new(db.clone());

        let mut ids = store.all_ids();
        ids.sort();
        assert_eq!(ids, vec![1, 2]);

        let doc1 = store.get(1).expect("doc 1 must exist");
        let doc2 = store.get(2).expect("doc 2 must exist");
        assert_eq!(doc1["kind"], json!("post"));
        assert_eq!(doc2["kind"], json!("page"));
        assert!(store.get(999).is_none());
    }

    #[test]
    fn indexedjsonindexbackend_respects_indexconfig_for_lookup_eq_and_in() {
        let mut db = TestDb::new();
        db.insert(1, json!({ "kind": "post" }));
        db.insert(2, json!({ "kind": "page" }));

        let config = IndexConfig::new(["kind"]);
        let backend = IndexedJsonIndexBackend::new(db.clone(), config.clone());

        // kind is indexed -> backend should delegate to db.lookup_eq
        let ids = backend
            .lookup_eq("kind", &json!("post"))
            .expect("kind=post should be answered");
        assert!(ids.contains(&1));
        assert!(!ids.contains(&2));

        // Non-indexed field even if DB *could* answer it should be gated.
        // Here DB will return None for any other field anyway, but we
        // explicitly test that the backend checks IndexConfig:
        let ids = backend.lookup_eq("draft", &json!(false));
        assert!(ids.is_none());

        // kind IN ["post","page"] -> union via db.lookup_in
        let ids = backend
            .lookup_in("kind", &[json!("post"), json!("page")])
            .expect("kind in [post,page] should be answered");
        assert!(ids.contains(&1));
        assert!(ids.contains(&2));
    }

    #[test]
    fn indexedjsonindexbackend_lookup_range_default_is_none() {
        let db = TestDb::new();
        let config = IndexConfig::new(["kind"]);
        let backend = IndexedJsonIndexBackend::new(db.clone(), config);

        let res = backend.lookup_range("kind", Some(&json!(1)), Some(&json!(10)));
        assert!(res.is_none());
    }
}
