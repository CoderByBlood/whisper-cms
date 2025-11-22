// crates/adapt/src/render/pipeline.rs

use super::error::RenderError;
use super::html_rewriter::build_lol_settings_from_body_patches;
use super::template::TemplateEngine;
use crate::core::recommendation::{BodyPatch, BodyPatchKind};
use lol_html::rewrite_str;
use regex::Regex;
use serde::Serialize;
use serde_json::Value as Json;
use std::io::Write;

/// Render an HTML template + model into the given writer,
/// applying body-level regex and HtmlDom patches in the
/// correct order:
///
/// TemplateEngine → Regex → HtmlDom → out
pub fn render_html_template_to<T, M, W>(
    engine: &T,
    template_name: &str,
    model: &M,
    body_patches: &[BodyPatch],
    mut out: W,
) -> Result<(), RenderError>
where
    T: TemplateEngine,
    M: Serialize,
    W: Write,
{
    // Partition body patches.
    let mut regex_specs = Vec::new();
    let mut html_dom_patches = Vec::new();

    for patch in body_patches {
        match &patch.kind {
            BodyPatchKind::Regex {
                pattern,
                replacement,
            } => {
                // Compile regex; invalid ones are treated as patch errors.
                match Regex::new(pattern) {
                    Ok(re) => {
                        regex_specs.push((re, replacement.clone()));
                    }
                    Err(e) => {
                        // Log & skip in real system; here we treat as error.
                        return Err(RenderError::InvalidRegex {
                            pattern: pattern.clone(),
                            error: e.to_string(),
                        });
                    }
                }
            }
            BodyPatchKind::HtmlDom { .. } => {
                html_dom_patches.push(patch.clone());
            }
            BodyPatchKind::JsonPatch { .. } => {
                // Ignored for HTML responses.
            }
        }
    }

    // 1) Render template to an in-memory UTF-8 string.
    //
    // We render to a Vec<u8> first and then interpret as UTF-8.
    // Templates are expected to be valid UTF-8; using `from_utf8_lossy`
    // avoids introducing a new error variant while remaining robust.
    let mut buf = Vec::new();
    engine.render_to_write(template_name, model, &mut buf)?;
    let mut html_text = String::from_utf8_lossy(&buf).into_owned();

    // 2) Apply regex patches over the full HTML text (in order).
    for (re, replacement) in &regex_specs {
        html_text = re
            .replace_all(&html_text, replacement.as_str())
            .into_owned();
    }

    // 3) Apply HtmlDom patches using lol_html if any exist.
    if !html_dom_patches.is_empty() {
        let settings = build_lol_settings_from_body_patches(&html_dom_patches);
        let rewritten =
            rewrite_str(&html_text, settings).map_err(|e| RenderError::LolHtml(e.to_string()))?;
        out.write_all(rewritten.as_bytes())
            .map_err(RenderError::Io)?;
    } else {
        // No HtmlDom patches; write regex-transformed HTML directly.
        out.write_all(html_text.as_bytes())
            .map_err(RenderError::Io)?;
    }

    Ok(())
}

/// Render a *raw HTML string* into the given writer, applying:
///
/// 1) Regex patches on the HTML text
/// 2) HtmlDom patches via lol_html
pub fn render_html_string_to<W: Write>(
    html: &str,
    body_patches: &[BodyPatch],
    mut out: W,
) -> Result<(), RenderError> {
    let mut regex_specs = Vec::new();
    let mut html_dom_patches = Vec::new();

    for patch in body_patches {
        match &patch.kind {
            BodyPatchKind::Regex {
                pattern,
                replacement,
            } => match Regex::new(pattern) {
                Ok(re) => {
                    regex_specs.push((re, replacement.clone()));
                }
                Err(e) => {
                    return Err(RenderError::InvalidRegex {
                        pattern: pattern.clone(),
                        error: e.to_string(),
                    });
                }
            },
            BodyPatchKind::HtmlDom { .. } => {
                html_dom_patches.push(patch.clone());
            }
            BodyPatchKind::JsonPatch { .. } => {
                // Ignored for HTML responses.
            }
        }
    }

    // 1) Start from the raw HTML string.
    let mut html_text = html.to_owned();

    // 2) Apply regex patches over the full HTML text (in order).
    for (re, replacement) in &regex_specs {
        html_text = re
            .replace_all(&html_text, replacement.as_str())
            .into_owned();
    }

    // 3) Apply HtmlDom patches using lol_html if any exist.
    if !html_dom_patches.is_empty() {
        let settings = build_lol_settings_from_body_patches(&html_dom_patches);
        let rewritten =
            rewrite_str(&html_text, settings).map_err(|e| RenderError::LolHtml(e.to_string()))?;
        out.write_all(rewritten.as_bytes())
            .map_err(RenderError::Io)?;
    } else {
        // No HtmlDom patches; write regex-transformed HTML directly.
        out.write_all(html_text.as_bytes())
            .map_err(RenderError::Io)?;
    }

    Ok(())
}

/// Render a JSON value into the given writer, applying:
///
/// 1) Regex patches on the JSON text
/// 2) JSON Patch body patches
///
/// Then serializing the final value to UTF-8.
pub fn render_json_to<W: Write>(
    value: &Json,
    body_patches: &[BodyPatch],
    mut out: W,
) -> Result<(), RenderError> {
    // Partition patches.
    let mut regex_specs = Vec::new();
    let mut json_patches = Vec::new();

    for patch in body_patches {
        match &patch.kind {
            BodyPatchKind::Regex {
                pattern,
                replacement,
            } => match Regex::new(pattern) {
                Ok(re) => {
                    regex_specs.push((re, replacement.clone()));
                }
                Err(e) => {
                    return Err(RenderError::InvalidRegex {
                        pattern: pattern.clone(),
                        error: e.to_string(),
                    });
                }
            },
            BodyPatchKind::JsonPatch { patch, .. } => {
                json_patches.push(patch.clone());
            }
            BodyPatchKind::HtmlDom { .. } => {
                // Ignored for JSON responses.
            }
        }
    }

    // 1) Serialize original JSON to string.
    let mut json_text = serde_json::to_string(value)?;

    // 2) Apply all body regex patches (in order).
    for (re, replacement) in &regex_specs {
        json_text = re
            .replace_all(&json_text, replacement.as_str())
            .into_owned();
    }

    // 3) Attempt to parse back to JSON for JsonPatch body patches.
    let mut patched_value: Json = match serde_json::from_str(&json_text) {
        Ok(v) => v,
        Err(e) => {
            return Err(RenderError::JsonAfterRegex(e.to_string()));
        }
    };

    // 4) Apply JSON Patch body patches.
    for patch_doc in json_patches {
        let patch: json_patch::Patch = serde_json::from_value(patch_doc.clone())?;
        json_patch::patch(&mut patched_value, &patch)?;
    }

    // 5) Serialize final JSON to writer.
    serde_json::to_writer(&mut out, &patched_value)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::recommendation::{BodyPatch, DomOp};
    use crate::render::template::HbsEngine;
    use serde::Serialize;
    use serde_json::json;

    #[derive(Serialize)]
    struct SimpleModel<'a> {
        name: &'a str,
    }

    // ─────────────────────────────────────────────────────────────────────
    // HTML: basic rendering + regex + HtmlDom + errors
    // ─────────────────────────────────────────────────────────────────────

    #[test]
    fn render_html_template_no_patches_produces_expected_output() {
        let mut engine = HbsEngine::new();
        engine
            .register_template_str("hello", "Hello, {{name}}!")
            .expect("template registration should succeed");

        let model = SimpleModel { name: "Alice" };
        let mut out = Vec::new();

        render_html_template_to(&engine, "hello", &model, &[], &mut out)
            .expect("render_html_template_to should succeed");

        let s = String::from_utf8(out).unwrap();
        assert_eq!(s, "Hello, Alice!");
    }

    #[test]
    fn render_html_template_applies_regex_patches_in_order() {
        let mut engine = HbsEngine::new();
        engine
            .register_template_str("hello", "Hello, Alice! Alice!")
            .expect("template registration should succeed");

        let model = SimpleModel { name: "ignored" };

        // First replace "Alice" with "Bob", then "Bob" with "Carol".
        let patches = vec![
            BodyPatch::new_regex("Alice".into(), "Bob".into(), "p1".into()),
            BodyPatch::new_regex("Bob".into(), "Carol".into(), "p2".into()),
        ];

        let mut out = Vec::new();
        render_html_template_to(&engine, "hello", &model, &patches, &mut out)
            .expect("render_html_template_to should succeed");

        let s = String::from_utf8(out).unwrap();
        assert_eq!(s, "Hello, Carol! Carol!");
    }

    #[test]
    fn render_html_template_ignores_json_patches_for_html() {
        let mut engine = HbsEngine::new();
        engine
            .register_template_str("hello", "<p>{{name}}</p>")
            .expect("template registration should succeed");

        let model = SimpleModel { name: "Alice" };

        // JsonPatch body patches should be ignored for HTML.
        let json_patch_doc = json!([
            { "op": "replace", "path": "/name", "value": "Bob" }
        ]);
        let patches = vec![BodyPatch::new_json_patch(
            json_patch_doc,
            "json-plugin".into(),
        )];

        let mut out = Vec::new();
        render_html_template_to(&engine, "hello", &model, &patches, &mut out)
            .expect("render_html_template_to should succeed");

        let s = String::from_utf8(out).unwrap();
        assert_eq!(s, "<p>Alice</p>");
    }

    #[test]
    fn render_html_template_with_invalid_regex_returns_error() {
        let mut engine = HbsEngine::new();
        engine
            .register_template_str("hello", "Hello")
            .expect("template registration should succeed");

        let model = SimpleModel { name: "ignored" };

        // Invalid regex: unbalanced '('
        let patches = vec![BodyPatch::new_regex("(".into(), "x".into(), "bad".into())];

        let mut out = Vec::new();
        let res = render_html_template_to(&engine, "hello", &model, &patches, &mut out);

        match res {
            Err(RenderError::InvalidRegex { pattern, .. }) => {
                assert_eq!(pattern, "(");
            }
            other => panic!("expected InvalidRegex error, got {:?}", other),
        }
    }

    #[test]
    fn render_html_template_applies_html_dom_patches_if_present() {
        let mut engine = HbsEngine::new();
        engine
            .register_template_str("page", "<html><body><p class=\"x\">Hello</p></body></html>")
            .expect("template registration should succeed");

        let model = ();

        // HtmlDom patch: change inner text of <p.x> to "Hi".
        let dom_ops = vec![DomOp::SetInnerText("Hi".into())];
        let patches = vec![BodyPatch::new_html_dom(
            "p.x".into(),
            dom_ops,
            "dom-plugin".into(),
        )];

        let mut out = Vec::new();
        let res = render_html_template_to(&engine, "page", &model, &patches, &mut out);

        // If html_rewriter / lol_html wiring is correct, this should succeed
        // and the <p> content should be replaced.
        match res {
            Ok(()) => {
                let s = String::from_utf8(out).unwrap();
                assert!(
                    s.contains("<p class=\"x\">Hi</p>"),
                    "rewritten html should contain modified <p>, got: {}",
                    s
                );
            }
            Err(e) => panic!("expected ok for HtmlDom path, got {:?}", e),
        }
    }

    // ─────────────────────────────────────────────────────────────────────
    // JSON: basic rendering + regex + JsonPatch + error paths
    // ─────────────────────────────────────────────────────────────────────

    #[test]
    fn render_json_no_patches_writes_original_value() {
        let value = json!({ "a": 1, "b": "x" });

        let mut out = Vec::new();
        render_json_to(&value, &[], &mut out).expect("render_json_to should succeed");

        let s = String::from_utf8(out).unwrap();
        let parsed: Json = serde_json::from_str(&s).unwrap();
        assert_eq!(parsed, value);
    }

    #[test]
    fn render_json_applies_regex_patches_on_serialized_text() {
        let value = json!({ "message": "Hello" });

        // Replace "Hello" with "Hi" in the JSON text.
        let patches = vec![BodyPatch::new_regex(
            "Hello".into(),
            "Hi".into(),
            "p1".into(),
        )];

        let mut out = Vec::new();
        render_json_to(&value, &patches, &mut out).expect("render_json_to should succeed");

        let s = String::from_utf8(out).unwrap();
        let parsed: Json = serde_json::from_str(&s).unwrap();
        assert_eq!(parsed, json!({ "message": "Hi" }));
    }

    #[test]
    fn render_json_with_invalid_regex_returns_error() {
        let value = json!({ "a": 1 });

        let patches = vec![BodyPatch::new_regex("(".into(), "x".into(), "bad".into())];

        let mut out = Vec::new();
        let res = render_json_to(&value, &patches, &mut out);

        match res {
            Err(RenderError::InvalidRegex { pattern, .. }) => {
                assert_eq!(pattern, "(");
            }
            other => panic!("expected InvalidRegex error, got {:?}", other),
        }
    }

    #[test]
    fn render_json_invalid_after_regex_yields_json_after_regex_error() {
        let value = json!({ "a": 1 });

        // Remove all double quotes from the JSON text, producing invalid JSON `{a:1}`.
        let patches = vec![BodyPatch::new_regex(
            "\"".into(),
            "".into(),
            "break-json".into(),
        )];

        let mut out = Vec::new();
        let res = render_json_to(&value, &patches, &mut out);

        match res {
            Err(RenderError::JsonAfterRegex(msg)) => {
                // Don't overfit on serde_json wording; just ensure we got a non-empty message.
                assert!(
                    !msg.trim().is_empty(),
                    "expected a non-empty serde_json error message, got: {:?}",
                    msg
                );
            }
            other => panic!("expected JsonAfterRegex error, got {:?}", other),
        }
    }

    #[test]
    fn render_json_applies_json_patch_body_patches() {
        let value = json!({ "a": 1 });

        // JsonPatch: add field /b = 2
        let patch_doc = json!([
            { "op": "add", "path": "/b", "value": 2 }
        ]);

        let patches = vec![BodyPatch::new_json_patch(patch_doc, "json-patch".into())];

        let mut out = Vec::new();
        render_json_to(&value, &patches, &mut out).expect("render_json_to should succeed");

        let s = String::from_utf8(out).unwrap();
        let parsed: Json = serde_json::from_str(&s).unwrap();
        assert_eq!(parsed, json!({ "a": 1, "b": 2 }));
    }

    #[test]
    fn render_json_invalid_patch_document_returns_error() {
        let value = json!({ "a": 1 });

        // Not a valid JSON Patch document (must be an array of ops).
        let bad_patch_doc = json!({"op": "add", "path": "/b", "value": 2});

        let patches = vec![BodyPatch::new_json_patch(bad_patch_doc, "bad-patch".into())];

        let mut out = Vec::new();
        let res = render_json_to(&value, &patches, &mut out);

        // We don't assert the exact variant here, just that it's an error
        // originating from json-patch / serde_json conversion.
        assert!(
            res.is_err(),
            "expected error from invalid JSON Patch document"
        );
    }

    // ─────────────────────────────────────────────────────────────────────
    // Smoke test: combination of regex + jsonPatch on JSON
    // ─────────────────────────────────────────────────────────────────────

    #[test]
    fn render_json_applies_regex_then_json_patch_in_sequence() {
        // Start with {"msg":"Hello","count":1}
        let value = json!({ "msg": "Hello", "count": 1 });

        // First, regex changes "Hello" -> "Hi".
        let regex_patch = BodyPatch::new_regex("Hello".into(), "Hi".into(), "regex".into());

        // Then, json patch increments count to 2 via replace.
        let patch_doc = json!([
            { "op": "replace", "path": "/count", "value": 2 }
        ]);
        let json_patch = BodyPatch::new_json_patch(patch_doc, "json-patch".into());

        let patches = vec![regex_patch, json_patch];

        let mut out = Vec::new();
        render_json_to(&value, &patches, &mut out).expect("render_json_to should succeed");

        let s = String::from_utf8(out).unwrap();
        let parsed: Json = serde_json::from_str(&s).unwrap();
        assert_eq!(parsed, json!({ "msg": "Hi", "count": 2 }));
    }
}
