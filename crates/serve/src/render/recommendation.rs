// crates/adapt/src/render/recommendation.rs

use http::header::HeaderName;
use http::HeaderMap;
use json_patch::{patch as apply_json_patch_doc, Patch, PatchError};
use serde::{Deserialize, Serialize};
use serde_json::Value as Json;

/// All recommendations produced during a request.
///
/// This backs `ctx.recommend` on the JS side. Plugins can propose:
/// * header patches
/// * model JSON patches
/// * body patches (regex, HTML DOM, JSON patch)
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct Recommendations {
    pub header_patches: Vec<HeaderPatch>,
    pub model_patches: Vec<ModelPatch>,
    pub body_patches: Vec<BodyPatch>,
}

impl Recommendations {
    pub fn is_empty(&self) -> bool {
        self.header_patches.is_empty()
            && self.model_patches.is_empty()
            && self.body_patches.is_empty()
    }

    /// Apply all header patches in-order to the given map.
    pub fn apply_to_headers(&self, headers: &mut HeaderMap) {
        for hp in &self.header_patches {
            hp.apply(headers);
        }
    }

    /// Apply all model patches (JSON Patch) to the given model value.
    pub fn apply_to_model(&self, model: &mut Json) -> Result<(), PatchError> {
        for mp in &self.model_patches {
            mp.apply_to_model(model)?;
        }
        Ok(())
    }
}

/// Patch type for headers: set, append, or remove.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum HeaderPatchKind {
    Set,
    Append,
    Remove,
}

/// A single header patch.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HeaderPatch {
    pub kind: HeaderPatchKind,
    pub name: String,
    pub value: Option<String>,
    pub source_plugin: String,
}

impl HeaderPatch {
    pub fn set(name: String, value: String, source_plugin: String) -> Self {
        Self {
            kind: HeaderPatchKind::Set,
            name,
            value: Some(value),
            source_plugin,
        }
    }

    pub fn append(name: String, value: String, source_plugin: String) -> Self {
        Self {
            kind: HeaderPatchKind::Append,
            name,
            value: Some(value),
            source_plugin,
        }
    }

    pub fn remove(name: String, source_plugin: String) -> Self {
        Self {
            kind: HeaderPatchKind::Remove,
            name,
            value: None,
            source_plugin,
        }
    }

    /// Internal helper used by the host.
    pub fn apply(&self, headers: &mut HeaderMap) {
        use HeaderPatchKind::*;

        // Parse header name into an owned `HeaderName` to avoid lifetime issues
        let header_name: HeaderName = match self.name.parse() {
            Ok(n) => n,
            Err(_) => {
                // Invalid header name from plugin – ignore patch.
                return;
            }
        };

        match self.kind {
            Set => {
                if let Some(ref v) = self.value {
                    if let Ok(hv) = v.parse() {
                        headers.insert(header_name, hv);
                    }
                }
            }
            Append => {
                if let Some(ref v) = self.value {
                    if let Ok(hv) = v.parse() {
                        headers.append(header_name, hv);
                    }
                }
            }
            Remove => {
                headers.remove(header_name);
            }
        }
    }

    /// Compatibility shim for HTTP layer (`theme.rs`) which expects
    /// `hp.apply_to_headers(&mut headers)`.
    pub fn apply_to_headers(&self, headers: &mut HeaderMap) {
        self.apply(headers);
    }
}

/// A JSON Patch to be applied to the *model* (template data), not the body.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelPatch {
    /// RFC 6902 JSON Patch document (array of operations).
    pub patch: Json,
    pub source_plugin: String,
}

impl ModelPatch {
    pub fn apply_to_model(&self, model: &mut Json) -> Result<(), PatchError> {
        // Deserialize the stored JSON value into a `json_patch::Patch`.
        // If the JSON can't be parsed into a Patch, we treat it as a no-op.
        match serde_json::from_value::<Patch>(self.patch.clone()) {
            Ok(patch) => apply_json_patch_doc(model, &patch),
            Err(_) => Ok(()),
        }
    }
}

/// Body-level patch emitted by plugins.
///
/// These are applied to the rendered body stream by the render pipeline.
/// Regex patches run first (on the raw text), then:
///   * for HTML: HtmlDom patches via lol_html
///   * for JSON: JsonPatch patches via json-patch
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BodyPatch {
    pub kind: BodyPatchKind,
    pub source_plugin: String,
}

impl BodyPatch {
    pub fn new_regex(pattern: String, replacement: String, source_plugin: String) -> Self {
        Self {
            kind: BodyPatchKind::Regex {
                pattern,
                replacement,
            },
            source_plugin,
        }
    }

    pub fn new_html_dom(selector: String, ops: Vec<DomOp>, source_plugin: String) -> Self {
        Self {
            kind: BodyPatchKind::HtmlDom { selector, ops },
            source_plugin,
        }
    }

    pub fn new_json_patch(patch: Json, source_plugin: String) -> Self {
        Self {
            kind: BodyPatchKind::JsonPatch { patch },
            source_plugin,
        }
    }

    /// Apply this body patch as a JSON Patch to the given JSON value, if applicable.
    ///
    /// Non-JsonPatch variants are treated as no-ops for JSON bodies; they are handled
    /// elsewhere in the pipeline.
    pub fn apply_json_patch(&self, body: &mut Json) -> Result<(), PatchError> {
        match &self.kind {
            BodyPatchKind::JsonPatch { patch } => {
                match serde_json::from_value::<Patch>(patch.clone()) {
                    Ok(patch) => apply_json_patch_doc(body, &patch),
                    Err(_) => Ok(()),
                }
            }
            _ => Ok(()),
        }
    }
}

/// Kinds of body patches: regex, HTML DOM, or JSON Patch.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum BodyPatchKind {
    /// Regex replacement on the *response body text* (HTML or JSON).
    Regex {
        pattern: String,
        replacement: String,
    },

    /// DOM-aware HTML transforms via lol_html.
    HtmlDom { selector: String, ops: Vec<DomOp> },

    /// JSON Patch document (RFC 6902) to be applied to a JSON body.
    JsonPatch { patch: Json },
}

/// A DOM operation, mirroring the high-level API of `lol_html`.
///
/// These are intentionally close to lol_html's surface area so that the
/// HtmlRewriter can map them directly to element operations.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum DomOp {
    // Attribute operations
    SetAttr {
        name: String,
        value: String,
    },
    RemoveAttr {
        name: String,
    },

    // Class helpers
    AddClass(String),
    RemoveClass(String),

    // Replace inner content
    SetInnerHtml(String),
    SetInnerText(String),

    // Append / prepend to inner content
    AppendHtml(String),
    PrependHtml(String),

    // Replace element with new content
    ReplaceWithHtml(String),
    ReplaceWithText(String),

    // Insert siblings
    InsertBeforeHtml(String),
    InsertBeforeText(String),
    InsertAfterHtml(String),
    InsertAfterText(String),

    // Remove element or unwrap
    Remove,
    /// Remove the element but keep its children.
    Unwrap,
}

#[cfg(test)]
mod tests {
    use super::*;
    use http::HeaderValue;
    use serde_json::json;

    // ─────────────────────────────────────────────────────────────────────
    // Recommendations
    // ─────────────────────────────────────────────────────────────────────

    #[test]
    fn recommendations_is_empty_true_for_default() {
        let recs = Recommendations::default();
        assert!(recs.is_empty());
    }

    #[test]
    fn recommendations_is_empty_false_when_any_patch_present() {
        let mut recs = Recommendations::default();
        recs.header_patches.push(HeaderPatch::set(
            "x-test".to_string(),
            "1".to_string(),
            "plugin".to_string(),
        ));
        assert!(!recs.is_empty());

        let mut recs2 = Recommendations::default();
        recs2.model_patches.push(ModelPatch {
            patch: json!([]),
            source_plugin: "plugin".to_string(),
        });
        assert!(!recs2.is_empty());

        let mut recs3 = Recommendations::default();
        recs3.body_patches.push(BodyPatch::new_regex(
            "foo".to_string(),
            "bar".to_string(),
            "plugin".to_string(),
        ));
        assert!(!recs3.is_empty());
    }

    #[test]
    fn recommendations_apply_to_headers_applies_all_header_patches_in_order() {
        let mut recs = Recommendations::default();
        recs.header_patches.push(HeaderPatch::set(
            "x-order".to_string(),
            "one".to_string(),
            "p1".to_string(),
        ));
        recs.header_patches.push(HeaderPatch::append(
            "x-order".to_string(),
            "two".to_string(),
            "p2".to_string(),
        ));

        let mut headers = HeaderMap::new();
        recs.apply_to_headers(&mut headers);

        let vals: Vec<_> = headers
            .get_all("x-order")
            .iter()
            .map(|v| v.to_str().unwrap().to_string())
            .collect();

        assert_eq!(vals, vec!["one".to_string(), "two".to_string()]);
    }

    #[test]
    fn recommendations_apply_to_model_chains_patches() {
        // Two patches: replace /foo then add /bar
        let p1 = ModelPatch {
            patch: json!([{ "op": "add", "path": "/foo", "value": 1 }]),
            source_plugin: "p1".to_string(),
        };
        let p2 = ModelPatch {
            patch: json!([{ "op": "add", "path": "/bar", "value": 2 }]),
            source_plugin: "p2".to_string(),
        };

        let mut recs = Recommendations::default();
        recs.model_patches.push(p1);
        recs.model_patches.push(p2);

        let mut model = json!({});
        recs.apply_to_model(&mut model)
            .expect("patches should succeed");

        assert_eq!(model["foo"], json!(1));
        assert_eq!(model["bar"], json!(2));
    }

    // ─────────────────────────────────────────────────────────────────────
    // HeaderPatch
    // ─────────────────────────────────────────────────────────────────────

    #[test]
    fn header_patch_set_insert_header() {
        let hp = HeaderPatch::set(
            "x-test".to_string(),
            "value".to_string(),
            "plugin".to_string(),
        );

        let mut headers = HeaderMap::new();
        hp.apply(&mut headers);

        let v = headers.get("x-test").expect("header should exist");
        assert_eq!(v.to_str().unwrap(), "value");
    }

    #[test]
    fn header_patch_append_appends_values() {
        let hp1 = HeaderPatch::append("x-multi".to_string(), "one".to_string(), "p1".to_string());
        let hp2 = HeaderPatch::append("x-multi".to_string(), "two".to_string(), "p2".to_string());

        let mut headers = HeaderMap::new();
        hp1.apply(&mut headers);
        hp2.apply(&mut headers);

        let vals: Vec<_> = headers
            .get_all("x-multi")
            .iter()
            .map(|v| v.to_str().unwrap().to_string())
            .collect();

        assert_eq!(vals, vec!["one".to_string(), "two".to_string()]);
    }

    #[test]
    fn header_patch_remove_removes_header() {
        let mut headers = HeaderMap::new();
        headers.insert("x-remove", HeaderValue::from_static("value"));

        let hp = HeaderPatch::remove("x-remove".to_string(), "plugin".to_string());
        hp.apply(&mut headers);

        assert!(!headers.contains_key("x-remove"));
    }

    #[test]
    fn header_patch_invalid_name_is_silently_ignored() {
        let hp = HeaderPatch::set(
            "bad name".to_string(), // invalid header name
            "value".to_string(),
            "plugin".to_string(),
        );

        let mut headers = HeaderMap::new();
        hp.apply(&mut headers);

        assert!(headers.is_empty(), "no headers should be inserted");
    }

    #[test]
    fn header_patch_invalid_value_is_silently_ignored() {
        // Name is fine, value is invalid.
        let hp = HeaderPatch::set(
            "x-bad-val".to_string(),
            "line1\nline2".to_string(), // newline not allowed
            "plugin".to_string(),
        );

        let mut headers = HeaderMap::new();
        hp.apply(&mut headers);

        assert!(
            !headers.contains_key("x-bad-val"),
            "invalid value should not be inserted"
        );
    }

    #[test]
    fn header_patch_apply_to_headers_delegates_to_apply() {
        let hp = HeaderPatch::set(
            "x-direct".to_string(),
            "ok".to_string(),
            "plugin".to_string(),
        );

        let mut headers = HeaderMap::new();
        hp.apply_to_headers(&mut headers);

        assert_eq!(headers.get("x-direct").unwrap().to_str().unwrap(), "ok");
    }

    // ─────────────────────────────────────────────────────────────────────
    // ModelPatch
    // ─────────────────────────────────────────────────────────────────────

    #[test]
    fn model_patch_apply_valid_patch_changes_model() {
        let patch_json = json!([
            { "op": "add", "path": "/foo", "value": 1 },
            { "op": "replace", "path": "/foo", "value": 2 }
        ]);

        let mp = ModelPatch {
            patch: patch_json,
            source_plugin: "plugin".to_string(),
        };

        let mut model = json!({});
        mp.apply_to_model(&mut model)
            .expect("valid patch should succeed");

        assert_eq!(model["foo"], json!(2));
    }

    #[test]
    fn model_patch_invalid_patch_json_is_treated_as_noop() {
        // Not a valid JSON Patch document (object instead of array).
        let mp = ModelPatch {
            patch: json!({ "op": "add", "path": "/foo", "value": 1 }),
            source_plugin: "plugin".to_string(),
        };

        let mut model = json!({ "original": true });
        let res = mp.apply_to_model(&mut model);

        // We treat invalid patch JSON as a no-op, not an error.
        assert!(res.is_ok());
        assert_eq!(model, json!({ "original": true }));
    }

    #[test]
    fn model_patch_patch_error_is_propagated() {
        // This is syntactically valid JSON Patch but will fail at apply time
        // (removing a non-existent path).
        let mp = ModelPatch {
            patch: json!([{ "op": "remove", "path": "/does_not_exist" }]),
            source_plugin: "plugin".to_string(),
        };

        let mut model = json!({ "foo": 1 });
        let res = mp.apply_to_model(&mut model);

        assert!(res.is_err(), "expected PatchError for invalid operation");
    }

    // ─────────────────────────────────────────────────────────────────────
    // BodyPatch
    // ─────────────────────────────────────────────────────────────────────

    #[test]
    fn body_patch_constructors_set_expected_kinds() {
        let bp_regex = BodyPatch::new_regex("foo".into(), "bar".into(), "p".into());
        match bp_regex.kind {
            BodyPatchKind::Regex {
                ref pattern,
                ref replacement,
            } => {
                assert_eq!(pattern, "foo");
                assert_eq!(replacement, "bar");
            }
            _ => panic!("expected Regex kind"),
        }

        let bp_html = BodyPatch::new_html_dom(
            "div".into(),
            vec![DomOp::SetInnerText("hi".into())],
            "p".into(),
        );
        match bp_html.kind {
            BodyPatchKind::HtmlDom {
                ref selector,
                ref ops,
            } => {
                assert_eq!(selector, "div");
                assert_eq!(ops.len(), 1);
            }
            _ => panic!("expected HtmlDom kind"),
        }

        let json_patch = json!([{ "op": "add", "path": "/a", "value": 1 }]);
        let bp_json = BodyPatch::new_json_patch(json_patch.clone(), "p".into());
        match bp_json.kind {
            BodyPatchKind::JsonPatch { ref patch } => {
                assert_eq!(patch, &json_patch);
            }
            _ => panic!("expected JsonPatch kind"),
        }
    }

    #[test]
    fn body_patch_apply_json_patch_valid() {
        let patch = json!([{ "op": "add", "path": "/foo", "value": 1 }]);
        let bp = BodyPatch::new_json_patch(patch, "plugin".into());

        let mut body = json!({});
        bp.apply_json_patch(&mut body)
            .expect("valid JSON Patch should succeed");

        assert_eq!(body["foo"], json!(1));
    }

    #[test]
    fn body_patch_apply_json_patch_invalid_patch_json_is_noop() {
        let patch = json!({ "op": "add", "path": "/foo", "value": 1 }); // invalid shape
        let bp = BodyPatch::new_json_patch(patch, "plugin".into());

        let mut body = json!({ "original": true });
        let res = bp.apply_json_patch(&mut body);

        assert!(res.is_ok(), "invalid patch JSON is treated as no-op");
        assert_eq!(body, json!({ "original": true }));
    }

    #[test]
    fn body_patch_apply_json_patch_non_jsonpatch_kinds_are_noop() {
        let bp_regex = BodyPatch::new_regex("foo".into(), "bar".into(), "p".into());
        let bp_html = BodyPatch::new_html_dom("div".into(), vec![], "p".into());

        let mut body1 = json!({ "x": 1 });
        let mut body2 = json!({ "y": 2 });

        bp_regex
            .apply_json_patch(&mut body1)
            .expect("regex kind should be no-op for JSON");
        bp_html
            .apply_json_patch(&mut body2)
            .expect("html kind should be no-op for JSON");

        assert_eq!(body1, json!({ "x": 1 }));
        assert_eq!(body2, json!({ "y": 2 }));
    }

    // ─────────────────────────────────────────────────────────────────────
    // DomOp basic serde round-trip
    // ─────────────────────────────────────────────────────────────────────

    #[test]
    fn domop_serde_roundtrip() {
        let ops = vec![
            DomOp::SetAttr {
                name: "data-x".into(),
                value: "1".into(),
            },
            DomOp::RemoveAttr {
                name: "data-y".into(),
            },
            DomOp::AddClass("a".into()),
            DomOp::RemoveClass("b".into()),
            DomOp::SetInnerHtml("<b>hi</b>".into()),
            DomOp::SetInnerText("hi".into()),
            DomOp::AppendHtml("<span>tail</span>".into()),
            DomOp::PrependHtml("<span>head</span>".into()),
            DomOp::ReplaceWithHtml("<p>new</p>".into()),
            DomOp::ReplaceWithText("plain".into()),
            DomOp::InsertBeforeHtml("<hr/>".into()),
            DomOp::InsertBeforeText("before".into()),
            DomOp::InsertAfterHtml("<hr/>".into()),
            DomOp::InsertAfterText("after".into()),
            DomOp::Remove,
            DomOp::Unwrap,
        ];

        let val = serde_json::to_value(&ops).expect("serialize DomOp Vec");
        let decoded: Vec<DomOp> = serde_json::from_value(val).expect("deserialize DomOp Vec");

        assert_eq!(decoded.len(), ops.len());
    }
}
