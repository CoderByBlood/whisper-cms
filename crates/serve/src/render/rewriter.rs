// crates/adapt/src/render/html_rewriter.rs

use super::recommendation::{BodyPatch, BodyPatchKind, DomOp};
use lol_html::{
    element,
    html_content::{ContentType, Element},
    HtmlRewriter, Settings,
};
use std::{io, marker::PhantomData};

/// Build a lol_html `Settings` object from a slice of BodyPatch values.
///
/// This is called *after* BodyRegex patches have run,
/// and only when the final content type is confirmed to be HTML.
pub fn build_lol_settings_from_body_patches<'h, 's>(patches: &'h [BodyPatch]) -> Settings<'h, 's> {
    let mut elements = Vec::new();

    for patch in patches {
        if let BodyPatchKind::HtmlDom { selector, ops } = &patch.kind {
            let sel = selector.clone();
            let ops = ops.clone();

            let handler = element!(sel.as_str(), move |el: &mut Element| {
                for op in &ops {
                    match op {
                        DomOp::SetAttr { name, value } => {
                            let _ = el.set_attribute(name, value);
                        }
                        DomOp::RemoveAttr { name } => {
                            let _ = el.remove_attribute(name);
                        }

                        DomOp::AddClass(cls) => {
                            if let Some(orig) = el.get_attribute("class") {
                                let new = format!("{} {}", orig, cls);
                                let _ = el.set_attribute("class", new.trim());
                            } else {
                                let _ = el.set_attribute("class", cls);
                            }
                        }

                        DomOp::RemoveClass(cls) => {
                            if let Some(orig) = el.get_attribute("class") {
                                let parts: Vec<&str> =
                                    orig.split_whitespace().filter(|c| *c != cls).collect();
                                let new = parts.join(" ");
                                let _ = el.set_attribute("class", new.as_str());
                            }
                        }

                        DomOp::SetInnerHtml(html) => {
                            let _ = el.set_inner_content(html, ContentType::Html);
                        }

                        DomOp::SetInnerText(text) => {
                            // Let lol_html escape text; no manual escaping.
                            let _ = el.set_inner_content(text, ContentType::Text);
                        }

                        DomOp::AppendHtml(html) => {
                            let _ = el.append(html, ContentType::Html);
                        }

                        DomOp::PrependHtml(html) => {
                            let _ = el.prepend(html, ContentType::Html);
                        }

                        DomOp::ReplaceWithHtml(html) => {
                            let _ = el.replace(html, ContentType::Html);
                        }

                        DomOp::ReplaceWithText(text) => {
                            // Let lol_html escape text; no manual escaping.
                            let _ = el.replace(text, ContentType::Text);
                        }

                        DomOp::InsertBeforeHtml(html) => {
                            let _ = el.before(html, ContentType::Html);
                        }

                        DomOp::InsertBeforeText(text) => {
                            let _ = el.before(text, ContentType::Text);
                        }

                        DomOp::InsertAfterHtml(html) => {
                            let _ = el.after(html, ContentType::Html);
                        }

                        DomOp::InsertAfterText(text) => {
                            let _ = el.after(text, ContentType::Text);
                        }

                        DomOp::Remove => {
                            let _ = el.remove();
                        }

                        DomOp::Unwrap => {
                            let _ = el.remove_and_keep_content();
                        }
                    }
                }
                Ok(())
            });

            elements.push(handler);
        }
    }

    Settings {
        element_content_handlers: elements,
        ..Settings::default()
    }
}

/// A wrapper struct that owns a lol_html HtmlRewriter and provides a streaming-friendly API.
///
/// You give it:
/// - lol_html settings (built from patches)
/// - a sink implementing `FnMut(&[u8])`
///
/// Then call `.write(chunk)` repeatedly, and finally `.end()`.
pub struct HtmlDomRewriter<'h, 's, F>
where
    F: FnMut(&[u8]),
{
    // HtmlRewriter in lol_html 2.7 has a single lifetime parameter.
    rewriter: HtmlRewriter<'h, F>,
    // Use 's so it's not an unused lifetime parameter.
    _phantom: PhantomData<&'s ()>,
}

impl<'h, 's, F> HtmlDomRewriter<'h, 's, F>
where
    F: FnMut(&[u8]),
{
    pub fn new(settings: Settings<'h, 's>, sink: F) -> Self {
        Self {
            rewriter: HtmlRewriter::new(settings, sink),
            _phantom: PhantomData,
        }
    }

    /// Feed HTML text to the streaming rewriter.
    pub fn write(&mut self, text: &str) -> io::Result<()> {
        self.rewriter
            .write(text.as_bytes())
            .map_err(|e| io::Error::new(io::ErrorKind::Other, e))
    }

    /// Finalize the output.
    pub fn end(self) -> io::Result<()> {
        self.rewriter
            .end()
            .map_err(|e| io::Error::new(io::ErrorKind::Other, e))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::render::recommendation::{BodyPatch, DomOp};
    use lol_html::rewrite_str;

    // Helper to run rewrite_str using our Settings builder.
    fn apply_patches(html: &str, patches: &[BodyPatch]) -> String {
        let settings = build_lol_settings_from_body_patches(patches);
        rewrite_str(html, settings).expect("lol_html rewrite_str failed")
    }

    #[test]
    fn build_lol_settings_with_no_html_patches_is_noop() {
        // No patches → html must pass through unchanged.
        let patches: Vec<BodyPatch> = Vec::new();
        let html = "<div><p>Hello</p></div>";
        let result = apply_patches(html, &patches);
        assert_eq!(result, html);
    }

    #[test]
    fn html_dom_patches_only_affect_matching_selector() {
        // Patch only <p> elements; <span> must remain unchanged.
        let patch = BodyPatch::new_html_dom(
            "p".into(),
            vec![DomOp::SetInnerHtml("CHANGED".into())],
            "test-plugin".into(),
        );

        let html = "<div><p>Original</p><span>Keep</span></div>";
        let result = apply_patches(html, &[patch]);

        assert!(
            result.contains("<p>CHANGED</p>"),
            "expected <p> inner HTML to change, got: {result}"
        );
        assert!(
            result.contains("<span>Keep</span>"),
            "expected <span> content to remain, got: {result}"
        );
    }

    #[test]
    fn add_class_adds_new_class_when_none_present() {
        let patch = BodyPatch::new_html_dom(
            "div".into(),
            vec![DomOp::AddClass("new-class".into())],
            "test".into(),
        );

        let html = r#"<div id="t"></div>"#;
        let result = apply_patches(html, &[patch]);

        assert!(
            result.contains(r#"class="new-class""#),
            "expected new class attribute, got: {result}"
        );
    }

    #[test]
    fn add_class_appends_to_existing_class_list() {
        let patch = BodyPatch::new_html_dom(
            "div".into(),
            vec![DomOp::AddClass("new".into())],
            "test".into(),
        );

        let html = r#"<div id="t" class="old"></div>"#;
        let result = apply_patches(html, &[patch]);

        // Class order and spacing are implementation details, but "old new" is expected.
        assert!(
            result.contains(r#"class="old new""#) || result.contains(r#"class="old  new""#),
            "expected existing class to be preserved and new added, got: {result}"
        );
    }

    #[test]
    fn remove_class_removes_only_target_class() {
        let patch = BodyPatch::new_html_dom(
            "div".into(),
            vec![DomOp::RemoveClass("b".into())],
            "test".into(),
        );

        let html = r#"<div id="t" class="a b c"></div>"#;
        let result = apply_patches(html, &[patch]);

        assert!(
            !result.contains(" b "),
            "expected class 'b' to be removed, got: {result}"
        );
        assert!(
            result.contains("a") && result.contains("c"),
            "expected other classes to remain, got: {result}"
        );
    }

    #[test]
    fn set_inner_html_replaces_existing_content_unescaped() {
        let patch = BodyPatch::new_html_dom(
            "p".into(),
            vec![DomOp::SetInnerHtml("<b>bold</b>".into())],
            "test".into(),
        );

        let html = "<p>Old</p>";
        let result = apply_patches(html, &[patch]);

        assert!(
            result.contains("<p><b>bold</b></p>"),
            "expected inner HTML to be replaced with raw HTML, got: {result}"
        );
    }

    #[test]
    fn set_inner_text_replaces_content_with_escaped_text() {
        let patch = BodyPatch::new_html_dom(
            "p".into(),
            vec![DomOp::SetInnerText("<b>not-bold</b>".into())],
            "test".into(),
        );

        let html = "<p>Old</p>";
        let result = apply_patches(html, &[patch]);

        // Text should be escaped, not interpreted as HTML.
        assert!(
            result.contains("&lt;b&gt;not-bold&lt;/b&gt;"),
            "expected inner text to be HTML-escaped, got: {result}"
        );
        assert!(
            !result.contains("<b>not-bold</b>"),
            "unexpected raw <b> tag in result: {result}"
        );
    }

    #[test]
    fn append_and_prepend_html_modify_children_in_correct_order() {
        let patch = BodyPatch::new_html_dom(
            "div".into(),
            vec![
                DomOp::PrependHtml("<b>first</b>".into()),
                DomOp::AppendHtml("<i>last</i>".into()),
            ],
            "test".into(),
        );

        let html = "<div><span>middle</span></div>";
        let result = apply_patches(html, &[patch]);

        // Expected logical ordering:
        // <div><b>first</b><span>middle</span><i>last</i></div>
        assert!(
            result.contains("<b>first</b>"),
            "expected prepended HTML, got: {result}"
        );
        assert!(
            result.contains("<i>last</i>"),
            "expected appended HTML, got: {result}"
        );
        let first_idx = result.find("<b>first</b>").unwrap();
        let middle_idx = result.find("<span>middle</span>").unwrap();
        let last_idx = result.find("<i>last</i>").unwrap();
        assert!(first_idx < middle_idx && middle_idx < last_idx);
    }

    #[test]
    fn replace_with_html_replaces_entire_element() {
        let patch = BodyPatch::new_html_dom(
            "span".into(),
            vec![DomOp::ReplaceWithHtml("<b>NEW</b>".into())],
            "test".into(),
        );

        let html = "<div><span>old</span></div>";
        let result = apply_patches(html, &[patch]);

        assert!(
            !result.contains("<span>"),
            "expected span to be removed, got: {result}"
        );
        assert!(
            result.contains("<b>NEW</b>"),
            "expected replacement HTML, got: {result}"
        );
    }

    #[test]
    fn replace_with_text_replaces_element_with_escaped_text() {
        let patch = BodyPatch::new_html_dom(
            "span".into(),
            vec![DomOp::ReplaceWithText("<b>txt</b>".into())],
            "test".into(),
        );

        let html = "<div><span>old</span></div>";
        let result = apply_patches(html, &[patch]);

        assert!(
            !result.contains("<span>"),
            "expected span to be removed, got: {result}"
        );
        assert!(
            result.contains("&lt;b&gt;txt&lt;/b&gt;"),
            "expected escaped text replacement, got: {result}"
        );
        assert!(
            !result.contains("<b>txt</b>"),
            "unexpected raw <b> tag in result: {result}"
        );
    }

    #[test]
    fn insert_before_and_after_html_inserts_sibling_nodes() {
        let patch = BodyPatch::new_html_dom(
            "span".into(),
            vec![
                DomOp::InsertBeforeHtml("<b>before</b>".into()),
                DomOp::InsertAfterHtml("<i>after</i>".into()),
            ],
            "test".into(),
        );

        let html = "<div><span>here</span></div>";
        let result = apply_patches(html, &[patch]);

        // Expected: <div><b>before</b><span>here</span><i>after</i></div>
        let before_idx = result
            .find("<b>before</b>")
            .unwrap_or_else(|| panic!("before not found in: {result}"));
        let span_idx = result
            .find("<span>here</span>")
            .unwrap_or_else(|| panic!("span not found in: {result}"));
        let after_idx = result
            .find("<i>after</i>")
            .unwrap_or_else(|| panic!("after not found in: {result}"));
        assert!(before_idx < span_idx && span_idx < after_idx);
    }

    #[test]
    fn remove_deletes_element_and_its_contents() {
        let patch =
            BodyPatch::new_html_dom("span.to-remove".into(), vec![DomOp::Remove], "test".into());

        let html = r#"<div><span class="to-remove">bye</span><span>stay</span></div>"#;
        let result = apply_patches(html, &[patch]);

        assert!(
            !result.contains("bye"),
            "expected removed element content to disappear, got: {result}"
        );
        assert!(
            result.contains("<span>stay</span>"),
            "expected other elements to remain, got: {result}"
        );
    }

    #[test]
    fn unwrap_removes_element_but_keeps_children() {
        let patch = BodyPatch::new_html_dom("span.wrap".into(), vec![DomOp::Unwrap], "test".into());

        let html = r#"<div><span class="wrap"><b>inner</b></span></div>"#;
        let result = apply_patches(html, &[patch]);

        // We expect <span class="wrap"> to be gone, but <b>inner</b> to remain.
        assert!(
            !result.contains("span class=\"wrap\""),
            "expected wrapper span to be removed, got: {result}"
        );
        assert!(
            result.contains("<b>inner</b>"),
            "expected children to be kept after unwrap, got: {result}"
        );
    }

    #[test]
    fn non_html_body_patches_are_ignored_by_settings_builder() {
        // Only HTML DOM patches should affect the output; regex/json patches are ignored here.
        let html_only_patch = BodyPatch::new_html_dom(
            "p".into(),
            vec![DomOp::SetInnerHtml("H".into())],
            "html-only".into(),
        );

        // Construct a dummy "regex-like" patch by reusing HtmlDom kind with a selector
        // that never matches; the point of the test is that build_lol_settings only
        // cares about `BodyPatchKind::HtmlDom` variants, and ignores others in the
        // pipeline (they are handled by other stages).
        // In practice, actual regex/json patches will come through `BodyPatchKind::Regex`
        // and `BodyPatchKind::JsonPatch`, which are already ignored in the function.
        let json_patch_like =
            BodyPatch::new_json_patch(serde_json::json!({ "op": "add" }), "json".to_string());

        let html = "<p>X</p>";
        let result = apply_patches(html, &[html_only_patch, json_patch_like]);

        assert_eq!(result, "<p>H</p>");
    }

    #[test]
    fn html_dom_rewriter_streaming_matches_rewrite_str_output() {
        let patch = BodyPatch::new_html_dom(
            "p".into(),
            vec![DomOp::SetInnerHtml("STREAM".into())],
            "stream".into(),
        );

        let html = "<div><p>a</p><p>b</p></div>";

        // One-shot path: use rewrite_str with its own Settings built from a bound slice.
        let expected_patches = [patch.clone()];
        let expected_settings = build_lol_settings_from_body_patches(&expected_patches);
        let expected = rewrite_str(html, expected_settings).expect("rewrite_str failed");

        // Streaming path: build a second Settings from another bound slice.
        let streaming_patches = [patch];
        let streaming_settings = build_lol_settings_from_body_patches(&streaming_patches);

        let mut out = Vec::new();
        {
            let mut rewriter = HtmlDomRewriter::new(streaming_settings, |chunk: &[u8]| {
                out.extend_from_slice(chunk)
            });

            // Write in multiple chunks to exercise streaming behavior.
            rewriter.write("<div><p>a").unwrap();
            rewriter.write("</p><p>b</p>").unwrap();
            rewriter.write("</div>").unwrap();
            rewriter.end().unwrap();
        }

        let streamed = String::from_utf8(out).expect("streamed output not UTF-8");
        assert_eq!(
            streamed, expected,
            "streaming HtmlDomRewriter should match rewrite_str output"
        );
    }

    #[test]
    fn html_dom_rewriter_handles_empty_input_gracefully() {
        let patches: Vec<BodyPatch> = Vec::new();
        let settings = build_lol_settings_from_body_patches(&patches);

        let mut out = Vec::new();
        {
            let rewriter =
                HtmlDomRewriter::new(settings, |chunk: &[u8]| out.extend_from_slice(chunk));
            // No writes, just end.
            rewriter.end().unwrap();
        }

        let streamed = String::from_utf8(out).expect("streamed output not UTF-8");
        assert_eq!(streamed, "", "expected empty output for empty input");
    }
}
