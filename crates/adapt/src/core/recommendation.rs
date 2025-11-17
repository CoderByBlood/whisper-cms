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
