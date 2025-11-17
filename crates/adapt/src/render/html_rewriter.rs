// crates/adapt/src/render/html_rewriter.rs

use crate::core::recommendation::{BodyPatch, BodyPatchKind, DomOp};
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
    // Let the compiler infer the concrete `(Cow<Selector>, ElementContentHandlers)` type.
    let mut elements = Vec::new();

    for patch in patches {
        if let BodyPatchKind::HtmlDom { selector, ops } = &patch.kind {
            let sel = selector.clone();
            let ops = ops.clone();

            // Build a closure for this selector.
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
                            let escaped = html_escape::encode_text(text);
                            let _ = el.set_inner_content(&escaped, ContentType::Text);
                        }

                        DomOp::AppendHtml(html) => {
                            let _ = el.append(&html, ContentType::Html);
                        }

                        DomOp::PrependHtml(html) => {
                            let _ = el.prepend(&html, ContentType::Html);
                        }

                        DomOp::ReplaceWithHtml(html) => {
                            let _ = el.replace(&html, ContentType::Html);
                        }

                        DomOp::ReplaceWithText(text) => {
                            let escaped = html_escape::encode_text(text);
                            let _ = el.replace(&escaped, ContentType::Text);
                        }

                        DomOp::InsertBeforeHtml(html) => {
                            let _ = el.before(html, ContentType::Html);
                        }

                        DomOp::InsertBeforeText(text) => {
                            let escaped = html_escape::encode_text(text);
                            let _ = el.before(&escaped, ContentType::Text);
                        }

                        DomOp::InsertAfterHtml(html) => {
                            let _ = el.after(html, ContentType::Html);
                        }

                        DomOp::InsertAfterText(text) => {
                            let escaped = html_escape::encode_text(text);
                            let _ = el.after(&escaped, ContentType::Text);
                        }

                        DomOp::Remove => {
                            // Remove the element and its content.
                            let _ = el.remove();
                        }

                        DomOp::Unwrap => {
                            // Remove only the element, keep its children.
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
