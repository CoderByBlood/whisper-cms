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
                ..
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
        out.write_all(rewritten.as_bytes())?;
    } else {
        // No HtmlDom patches; write regex-transformed HTML directly.
        out.write_all(html_text.as_bytes())?;
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
                ..
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
        // NOTE: json-patch expects a Patch (Vec<PatchOperation>), not raw JSON.
        let patch: json_patch::Patch = serde_json::from_value(patch_doc.clone())?;
        json_patch::patch(&mut patched_value, &patch)?;
    }

    // 5) Serialize final JSON to writer.
    serde_json::to_writer(&mut out, &patched_value)?;
    Ok(())
}
