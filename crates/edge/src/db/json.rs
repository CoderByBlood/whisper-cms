use adapt::mql::index::{BoolField, I64Field, IndexRecord, StringField};
use adapt::mql::{IndexBackend, IndexConfig, JsonStore};
use async_trait::async_trait;
use chrono::Datelike;
use indexed_json::{IndexEntry, IndexableField, IndexedJson, Query};
use serde_json::Value as Json;
use std::collections::HashSet;
use std::fmt::Debug;
use std::hash::{Hash, Hasher};
use std::sync::Arc;
use tokio::sync::Mutex;

// ─────────────────────────────────────────────────────────────────────────────
// indexed_json-backed JsonStore and IndexBackend
// ─────────────────────────────────────────────────────────────────────────────

type SharedIndexedJson = Arc<Mutex<IndexedJson<IndexRecord>>>;

/// Because `IndexEntry` itself doesn’t implement `Hash`, we wrap it in a
/// newtype so we can satisfy `Id: Hash` bounds in our traits.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct IndexedId(pub IndexEntry);

impl Hash for IndexedId {
    fn hash<H: Hasher>(&self, state: &mut H) {
        // Use public Datelike methods on NaiveDate
        self.0.file.year().hash(state);
        self.0.file.ordinal().hash(state);
        self.0.offset.hash(state);
    }
}

/// JsonStore implementation backed by `indexed_json::IndexedJson<IndexRecord>`.
///
/// This treats `IndexedJson` as the durable store of `IndexRecord` values
/// and exposes them as `serde_json::Value` to the rest of the MQL system.
#[derive(Clone)]
pub struct IndexedJsonStore {
    pub db: SharedIndexedJson,
}

impl IndexedJsonStore {
    pub fn new(db: SharedIndexedJson) -> Self {
        Self { db }
    }
}

#[async_trait]
impl JsonStore for IndexedJsonStore {
    type Id = IndexedId;

    async fn all_ids(&self) -> Vec<Self::Id> {
        let mut ids = Vec::new();
        let mut guard = self.db.lock().await;

        if let Some(first) = guard.first() {
            let mut cur = first;
            loop {
                match guard.get(cur).await {
                    Ok(Some((next, _rec))) => {
                        // Only push if we actually got a record
                        ids.push(IndexedId(cur));
                        cur = next;
                    }
                    Ok(None) => break,
                    Err(_) => break,
                }
            }
        }

        ids
    }

    async fn get(&self, id: Self::Id) -> Option<Json> {
        let mut guard = self.db.lock().await;
        match guard.get(id.0).await {
            Ok(Some((_next, rec))) => serde_json::to_value(rec).ok(),
            _ => None,
        }
    }
}

/// IndexBackend implementation backed by `IndexedJson<IndexRecord>`.
///
/// This builds `indexed_json::Query` values from simple `(field, Json)`
/// constraints and delegates to the underlying archive index.
#[derive(Clone)]
pub struct IndexedJsonIndexBackend {
    pub db: SharedIndexedJson,
    pub config: IndexConfig,
}

impl IndexedJsonIndexBackend {
    pub fn new(db: SharedIndexedJson, config: IndexConfig) -> Self {
        Self { db, config }
    }

    pub fn index_config(&self) -> &IndexConfig {
        &self.config
    }

    fn make_field(
        &self,
        field: &str,
        value: &Json,
    ) -> Option<Arc<dyn IndexableField + Send + Sync>> {
        // Map MQL field path -> static key & typed value.
        macro_rules! string_field {
            ($key:expr) => {{
                let s = value
                    .as_str()
                    .map(|v| v.to_owned())
                    .unwrap_or_else(|| value.to_string());
                Some(Arc::new(StringField::new($key, s)) as Arc<dyn IndexableField + Send + Sync>)
            }};
        }

        macro_rules! i64_field {
            ($key:expr) => {{
                let n = if let Some(i) = value.as_i64() {
                    i
                } else if let Some(u) = value.as_u64() {
                    u as i64
                } else {
                    return None;
                };
                Some(Arc::new(I64Field::new($key, n)) as Arc<dyn IndexableField + Send + Sync>)
            }};
        }

        macro_rules! bool_field {
            ($key:expr) => {{
                let b = value.as_bool()?;
                Some(Arc::new(BoolField::new($key, b)) as Arc<dyn IndexableField + Send + Sync>)
            }};
        }

        match field {
            // Root
            "id" => string_field!("id"),
            "type" => string_field!("type"),
            "slug" => string_field!("slug"),
            "parent" => string_field!("parent"),

            // content.*
            "content.title" => string_field!("content.title"),
            "content.section" => string_field!("content.section"),

            // publish.*
            "publish.status" => string_field!("publish.status"),
            "publish.date" => string_field!("publish.date"),
            "publish.modified" => string_field!("publish.modified"),

            // nav.*
            "nav.menu_order" => i64_field!("nav.menu_order"),
            "nav.menu_visible" => bool_field!("nav.menu_visible"),

            // tax.* — single value at a time; IndexRecord indexes each element separately.
            "tax.categories" => string_field!("tax.categories"),
            "tax.tags" => string_field!("tax.tags"),
            "tax.series" => string_field!("tax.series"),

            // i18n.*
            "i18n.lang" => string_field!("i18n.lang"),
            "i18n.canonical_id" => string_field!("i18n.canonical_id"),

            // author.*
            "author.author" => string_field!("author.author"),
            "author.co_authors" => string_field!("author.co_authors"),

            _ => None,
        }
    }

    async fn run_query(&self, q: &Query) -> Option<HashSet<IndexedId>> {
        let guard = self.db.lock().await;
        let set = guard.query(q).ok()?;

        // `Set<IndexEntry>`'s iterator yields &IndexEntry
        let out: HashSet<IndexedId> = set
            .into_iter()
            .map(|e: &IndexEntry| IndexedId(*e))
            .collect();
        Some(out)
    }
}

#[async_trait]
impl IndexBackend for IndexedJsonIndexBackend {
    type Id = IndexedId;

    async fn lookup_eq(&self, field: &str, value: &Json) -> Option<HashSet<Self::Id>> {
        if !self.config.is_indexed(field) {
            return None;
        }
        let f = self.make_field(field, value)?;
        let q = Query::Eq(f);
        self.run_query(&q).await
    }

    async fn lookup_in(&self, field: &str, values: &[Json]) -> Option<HashSet<Self::Id>> {
        if !self.config.is_indexed(field) {
            return None;
        }

        let mut clauses = Vec::new();
        for v in values {
            if let Some(f) = self.make_field(field, v) {
                clauses.push(Query::Eq(f));
            }
        }
        if clauses.is_empty() {
            return None;
        }

        let q = if clauses.len() == 1 {
            clauses.remove(0)
        } else {
            Query::Or(clauses)
        };

        self.run_query(&q).await
    }

    async fn lookup_range(
        &self,
        field: &str,
        min: Option<&Json>,
        max: Option<&Json>,
    ) -> Option<HashSet<Self::Id>> {
        if !self.config.is_indexed(field) {
            return None;
        }

        let mut parts = Vec::new();
        if let Some(min_v) = min {
            if let Some(f) = self.make_field(field, min_v) {
                parts.push(Query::Gte(f));
            }
        }
        if let Some(max_v) = max {
            if let Some(f) = self.make_field(field, max_v) {
                parts.push(Query::Lte(f));
            }
        }

        let q = match parts.len() {
            0 => return None,
            1 => parts.remove(0),
            _ => Query::And(parts),
        };

        self.run_query(&q).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use adapt::mql::index::IndexRecord;
    use adapt::mql::IndexConfig;
    use chrono::{NaiveDate, Timelike};
    use indexed_json::Indexable;
    use serde_json::json;
    use smallvec::SmallVec;
    use std::cmp::Ordering;
    use std::collections::HashMap;
    use std::collections::HashSet;
    use std::hash::{Hash, Hasher};
    use std::time::{SystemTime, UNIX_EPOCH};
    use tokio;

    // ─────────────────────────────────────────────────────────────
    // Helpers
    // ─────────────────────────────────────────────────────────────

    fn unique_temp_dir() -> std::path::PathBuf {
        let mut base = std::env::temp_dir();
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        base.push(format!("indexed_json_tests_{}", nanos));
        std::fs::create_dir_all(&base).unwrap();
        base
    }

    async fn new_db_with_records(records: Vec<IndexRecord>) -> SharedIndexedJson {
        let base = unique_temp_dir();
        let mut db = IndexedJson::<IndexRecord>::open(&base).await.unwrap();
        for rec in &records {
            db.append(rec).await.unwrap();
        }
        db.flush().await.unwrap();
        Arc::new(Mutex::new(db))
    }

    // ─────────────────────────────────────────────────────────────
    // IndexedId hash / equality
    // ─────────────────────────────────────────────────────────────

    #[test]
    fn indexed_id_hash_and_eq_behaves_sensibly() {
        let d1 = NaiveDate::from_ymd_opt(2024, 1, 1).unwrap();
        let d2 = NaiveDate::from_ymd_opt(2024, 1, 2).unwrap();

        let e1 = IndexEntry {
            file: d1,
            offset: 10,
        };
        let e1_dup = IndexEntry {
            file: d1,
            offset: 10,
        };
        let e2 = IndexEntry {
            file: d2,
            offset: 10,
        };

        let id1 = IndexedId(e1);
        let id1b = IndexedId(e1_dup);
        let id2 = IndexedId(e2);

        // equality
        assert_eq!(id1, id1b);
        assert_ne!(id1, id2);

        // hashing: inserting duplicates yields size 1, distinct yields 2
        let mut set = HashSet::new();
        set.insert(id1);
        set.insert(id1b);
        set.insert(id2);
        assert_eq!(set.len(), 2);

        // sanity: hashing is stable for same value
        fn hash_val<T: Hash>(v: &T) -> u64 {
            use std::collections::hash_map::DefaultHasher;
            let mut h = DefaultHasher::new();
            v.hash(&mut h);
            h.finish()
        }

        assert_eq!(hash_val(&id1), hash_val(&id1b));
    }

    // ─────────────────────────────────────────────────────────────
    // StringField / I64Field / BoolField
    // ─────────────────────────────────────────────────────────────

    #[test]
    fn stringfield_basic_behaviour() {
        let f = StringField::new("slug", "hello".to_string());
        assert_eq!(f.key, "slug");
        assert_eq!(f.key(), "slug");
        assert!(f.byte_compareable());

        let mut buf: SmallVec<[u8; 128]> = SmallVec::new();
        f.encode(&mut buf).unwrap();
        assert_eq!(&buf[..], b"hello");

        // as_any downcast
        let any = f.as_any();
        let down = any.downcast_ref::<StringField>().unwrap();
        assert_eq!(down.value, "hello");

        // Display
        assert_eq!(format!("{f}"), "hello");
    }

    #[test]
    fn i64field_basic_behaviour_and_big_endian_encoding() {
        let f = I64Field::new("nav.menu_order", 0x0102_0304_0506_0708);
        assert_eq!(f.key, "nav.menu_order");
        assert!(f.byte_compareable());

        let mut buf: SmallVec<[u8; 128]> = SmallVec::new();
        f.encode(&mut buf).unwrap();

        // Big-endian encoding preserves numeric order
        assert_eq!(buf.len(), 8);
        let mut expected = Vec::new();
        expected.extend_from_slice(&0x0102_0304_0506_0708_i64.to_be_bytes());
        assert_eq!(&buf[..], &expected[..]);

        // Display
        assert_eq!(format!("{f}"), "72623859790382856"); // decimal representation
    }

    #[test]
    fn boolfield_basic_behaviour_and_encoding() {
        let f_true = BoolField::new("nav.menu_visible", true);
        let f_false = BoolField::new("nav.menu_visible", false);

        assert!(f_true.byte_compareable());
        assert!(f_false.byte_compareable());

        let mut buf: SmallVec<[u8; 128]> = SmallVec::new();
        f_true.encode(&mut buf).unwrap();
        assert_eq!(&buf[..], [1]);

        buf.clear();
        f_false.encode(&mut buf).unwrap();
        assert_eq!(&buf[..], [0]);

        assert_eq!(format!("{f_true}"), "true");
        assert_eq!(format!("{f_false}"), "false");
    }

    // ─────────────────────────────────────────────────────────────
    // IndexRecord: Indexable (index, timestamp, dyn_partial_cmp)
    // ─────────────────────────────────────────────────────────────

    #[test]
    fn indexrecord_index_emits_expected_fields() {
        let mut rec = IndexRecord::default();
        rec.id = "1".to_string();
        rec.kind = Some("post".to_string());
        rec.slug = Some("hello".to_string());
        rec.publish.status = Some("publish".to_string());
        rec.nav.menu_order = Some(10);
        rec.nav.menu_visible = Some(true);
        rec.tax.tags = vec!["rust".to_string(), "wasm".to_string()];
        rec.author.author = Some("Alice".to_string());
        rec.author.co_authors = vec!["Bob".to_string(), "Carol".to_string()];

        let fields = rec.index();

        // Collect into map: key -> Vec<String> representation for easier checking
        let mut by_key: HashMap<&str, Vec<String>> = HashMap::new();
        for f in &fields {
            let val = format!("{f}");
            by_key.entry(f.key()).or_default().push(val);
        }

        assert_eq!(by_key.get("id").unwrap(), &vec!["1".to_string()]);
        assert_eq!(by_key.get("type").unwrap(), &vec!["post".to_string()]);
        assert_eq!(by_key.get("slug").unwrap(), &vec!["hello".to_string()]);
        assert_eq!(
            by_key.get("publish.status").unwrap(),
            &vec!["publish".to_string()]
        );
        assert_eq!(
            by_key.get("nav.menu_order").unwrap(),
            &vec!["10".to_string()]
        );
        assert_eq!(
            by_key.get("nav.menu_visible").unwrap(),
            &vec!["true".to_string()]
        );

        let tags = by_key.get("tax.tags").unwrap();
        assert!(tags.contains(&"rust".to_string()));
        assert!(tags.contains(&"wasm".to_string()));
        assert_eq!(tags.len(), 2);

        let co_authors = by_key.get("author.co_authors").unwrap();
        assert!(co_authors.contains(&"Bob".to_string()));
        assert!(co_authors.contains(&"Carol".to_string()));
        assert_eq!(co_authors.len(), 2);
    }

    #[test]
    fn indexrecord_timestamp_prefers_valid_publish_date() {
        let mut rec = IndexRecord::default();
        rec.publish.date = Some("2000-01-02T03:04:05Z".to_string());

        let ts = rec.timestamp();
        assert_eq!(ts.year(), 2000);
        assert_eq!(ts.month(), 1);
        assert_eq!(ts.day(), 2);
        assert_eq!(ts.hour(), 3);
        assert_eq!(ts.minute(), 4);
        assert_eq!(ts.second(), 5);
    }

    #[test]
    fn indexrecord_timestamp_falls_back_on_invalid_or_missing_date() {
        let mut rec = IndexRecord::default();
        rec.publish.date = Some("not-a-date".to_string());

        let ts1 = rec.timestamp();
        // Just sanity: it's some time after 1970
        assert!(ts1.year() >= 1970);

        let rec2 = IndexRecord::default();
        let ts2 = rec2.timestamp();
        assert!(ts2.year() >= 1970);
    }

    #[test]
    fn dyn_partial_cmp_for_scalar_fields() {
        let mut rec = IndexRecord::default();
        rec.id = "1".into();
        rec.kind = Some("post".into());
        rec.slug = Some("hello".into());

        // Equal
        let f_slug_eq = StringField::new("slug", "hello".into());
        assert_eq!(rec.dyn_partial_cmp(&f_slug_eq), Some(Ordering::Equal));

        // Less/Greater
        let f_slug_gt = StringField::new("slug", "world".into());
        assert_eq!(rec.dyn_partial_cmp(&f_slug_gt), Some("hello".cmp("world")));

        // Missing field => None
        let mut rec2 = IndexRecord::default();
        rec2.slug = None;
        assert_eq!(rec2.dyn_partial_cmp(&f_slug_eq), None);
    }

    #[test]
    fn dyn_partial_cmp_for_array_fields_uses_contains_semantics() {
        let mut rec = IndexRecord::default();
        rec.tax.tags = vec!["rust".into(), "wasm".into()];
        rec.author.co_authors = vec!["Bob".into(), "Carol".into()];

        let f_tag_rust = StringField::new("tax.tags", "rust".into());
        let f_tag_go = StringField::new("tax.tags", "go".into());

        assert_eq!(rec.dyn_partial_cmp(&f_tag_rust), Some(Ordering::Equal));
        assert_eq!(rec.dyn_partial_cmp(&f_tag_go), None);

        let f_co_bob = StringField::new("author.co_authors", "Bob".into());
        let f_co_dave = StringField::new("author.co_authors", "Dave".into());

        assert_eq!(rec.dyn_partial_cmp(&f_co_bob), Some(Ordering::Equal));
        assert_eq!(rec.dyn_partial_cmp(&f_co_dave), None);
    }

    // ─────────────────────────────────────────────────────────────
    // IndexedJsonStore tests
    // ─────────────────────────────────────────────────────────────

    #[tokio::test]
    async fn indexedjsonstore_all_ids_and_get_on_empty_db() {
        let base = unique_temp_dir();
        let db = IndexedJson::<IndexRecord>::open(&base).await.unwrap();
        let shared = Arc::new(Mutex::new(db));
        let store = IndexedJsonStore::new(shared);

        let ids = store.all_ids().await;
        assert!(ids.is_empty());

        assert!(store
            .get(IndexedId(IndexEntry {
                file: NaiveDate::from_ymd_opt(2024, 1, 1).unwrap(),
                offset: 0
            }))
            .await
            .is_none());
    }

    #[tokio::test]
    async fn indexedjsonstore_all_ids_and_get_with_records() {
        let mut r1 = IndexRecord::default();
        r1.id = "1".into();
        r1.slug = Some("a".into());
        r1.kind = Some("post".into());

        let mut r2 = IndexRecord::default();
        r2.id = "2".into();
        r2.slug = Some("b".into());
        r2.kind = Some("page".into());

        let shared = new_db_with_records(vec![r1.clone(), r2.clone()]).await;
        let store = IndexedJsonStore::new(shared.clone());

        let ids = store.all_ids().await;
        assert_eq!(ids.len(), 2);

        // Load docs by id and check slug
        for id in ids {
            let doc = store.get(id).await.expect("doc should exist");
            let slug = doc["slug"].as_str().unwrap();
            assert!(slug == "a" || slug == "b");
        }
    }

    // ─────────────────────────────────────────────────────────────
    // IndexedJsonIndexBackend: make_field + lookup_eq / in / range
    // ─────────────────────────────────────────────────────────────

    #[tokio::test]
    async fn make_field_handles_string_i64_and_bool_and_unknown_field() {
        let base = unique_temp_dir();
        let db = IndexedJson::<IndexRecord>::open(&base).await.unwrap();
        let shared = Arc::new(Mutex::new(db));

        let cfg = IndexConfig::new(["slug", "nav.menu_order", "nav.menu_visible"]);
        let backend = IndexedJsonIndexBackend::new(shared, cfg);

        // slug as string
        let f_slug = backend.make_field("slug", &json!("hello")).unwrap();
        assert_eq!(f_slug.key(), "slug");
        assert_eq!(format!("{f_slug}"), "hello");

        // nav.menu_order as i64
        let f_order = backend.make_field("nav.menu_order", &json!(42)).unwrap();
        assert_eq!(f_order.key(), "nav.menu_order");
        assert_eq!(format!("{f_order}"), "42");

        // nav.menu_visible as bool
        let f_vis = backend
            .make_field("nav.menu_visible", &json!(true))
            .unwrap();
        assert_eq!(f_vis.key(), "nav.menu_visible");
        assert_eq!(format!("{f_vis}"), "true");

        // Wrong type for i64/bool => None
        assert!(backend.make_field("nav.menu_order", &json!("x")).is_none());
        assert!(backend
            .make_field("nav.menu_visible", &json!("x"))
            .is_none());

        // Unknown field => None
        assert!(backend.make_field("unknown.field", &json!("x")).is_none());
    }

    #[tokio::test]
    async fn lookup_eq_respects_indexconfig_and_finds_matches() {
        let mut r1 = IndexRecord::default();
        r1.id = "1".into();
        r1.slug = Some("a".into());
        r1.kind = Some("post".into());
        r1.tax.tags = vec!["rust".into()];

        let mut r2 = IndexRecord::default();
        r2.id = "2".into();
        r2.slug = Some("b".into());
        r2.kind = Some("post".into());
        r2.tax.tags = vec!["wasm".into()];

        let mut r3 = IndexRecord::default();
        r3.id = "3".into();
        r3.slug = Some("c".into());
        r3.kind = Some("page".into());
        r3.tax.tags = vec!["rust".into(), "wasm".into()];

        let shared = new_db_with_records(vec![r1, r2, r3]).await;

        // Only slug and tax.tags are "visible" to the planner
        let cfg = IndexConfig::new(["slug", "tax.tags"]);
        let backend = IndexedJsonIndexBackend::new(shared.clone(), cfg);

        // slug == "a"
        let hits = backend.lookup_eq("slug", &json!("a")).await.unwrap();
        assert_eq!(hits.len(), 1);

        // tax.tags == "rust" (array membership)
        let tag_hits = backend.lookup_eq("tax.tags", &json!("rust")).await.unwrap();
        assert_eq!(tag_hits.len(), 2);

        // field not in IndexConfig => None even though index exists
        assert!(backend.lookup_eq("type", &json!("post")).await.is_none());

        // wrong type for nav.menu_order mapped via make_field => None
        let cfg2 = IndexConfig::new(["nav.menu_order"]);
        let backend2 = IndexedJsonIndexBackend::new(shared.clone(), cfg2);
        assert!(backend2
            .lookup_eq("nav.menu_order", &json!("not-a-number"))
            .await
            .is_none());
    }

    #[tokio::test]
    async fn lookup_in_unions_results_and_handles_empty_and_mixed_values() {
        let mut r1 = IndexRecord::default();
        r1.id = "1".into();
        r1.slug = Some("a".into());

        let mut r2 = IndexRecord::default();
        r2.id = "2".into();
        r2.slug = Some("b".into());

        let mut r3 = IndexRecord::default();
        r3.id = "3".into();
        r3.slug = Some("c".into());

        let shared = new_db_with_records(vec![r1, r2, r3]).await;
        let cfg = IndexConfig::new(["slug"]);
        let backend = IndexedJsonIndexBackend::new(shared, cfg);

        // slug IN ["a", "c"] => ids 1 and 3
        let hits = backend
            .lookup_in("slug", &[json!("a"), json!("c")])
            .await
            .unwrap();
        assert_eq!(hits.len(), 2);

        // slug IN ["x", "y"] => empty set (no matches)
        let no_hits = backend
            .lookup_in("slug", &[json!("x"), json!("y")])
            .await
            .unwrap();
        assert!(no_hits.is_empty());

        // field not indexed => None
        assert!(backend.lookup_in("type", &[json!("post")]).await.is_none());

        // Mixed good/bad values: good still works
        let hits2 = backend
            .lookup_in("slug", &[json!("a"), json!(123), json!("b")])
            .await
            .unwrap();
        assert_eq!(hits2.len(), 2);
    }

    #[tokio::test]
    async fn lookup_range_builds_correct_queries_and_respects_indexconfig() {
        let mut r1 = IndexRecord::default();
        r1.id = "1".into();
        r1.slug = Some("a".into());
        r1.nav.menu_order = Some(1);

        let mut r2 = IndexRecord::default();
        r2.id = "2".into();
        r2.slug = Some("b".into());
        r2.nav.menu_order = Some(5);

        let mut r3 = IndexRecord::default();
        r3.id = "3".into();
        r3.slug = Some("c".into());
        r3.nav.menu_order = Some(10);

        let shared = new_db_with_records(vec![r1, r2, r3]).await;
        let cfg = IndexConfig::new(["nav.menu_order"]);
        let backend = IndexedJsonIndexBackend::new(shared, cfg);

        // Range [1, 5] inclusive => first two records
        let hits = backend
            .lookup_range("nav.menu_order", Some(&json!(1)), Some(&json!(5)))
            .await
            .unwrap();
        assert_eq!(hits.len(), 2);

        // min-only: >= 5 => last two
        let hits_min = backend
            .lookup_range("nav.menu_order", Some(&json!(5)), None)
            .await
            .unwrap();
        assert_eq!(hits_min.len(), 2);

        // max-only: <= 5 => first two
        let hits_max = backend
            .lookup_range("nav.menu_order", None, Some(&json!(5)))
            .await
            .unwrap();
        assert_eq!(hits_max.len(), 2);

        // field not indexed => None
        let cfg2 = IndexConfig::new(["slug"]);
        let backend2 = IndexedJsonIndexBackend::new(backend.db.clone(), cfg2);
        assert!(backend2
            .lookup_range("nav.menu_order", Some(&json!(1)), Some(&json!(10)))
            .await
            .is_none());

        // Bad types for min/max => both parts dropped => None
        let cfg3 = IndexConfig::new(["nav.menu_order"]);
        let backend3 = IndexedJsonIndexBackend::new(backend.db.clone(), cfg3);
        assert!(backend3
            .lookup_range("nav.menu_order", Some(&json!("x")), Some(&json!("y")))
            .await
            .is_none());
    }
}
