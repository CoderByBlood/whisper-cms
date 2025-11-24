// crates/adapt/src/render/template.rs

use super::error::RenderError;
use handlebars::Handlebars;
use serde::Serialize;
use std::io::Write;

/// Trait for template engines that can render to an arbitrary `Write`.
pub trait TemplateEngine: Send + Sync {
    fn render_to_write<M, W>(
        &self,
        template_name: &str,
        model: &M,
        out: &mut W,
    ) -> Result<(), RenderError>
    where
        M: Serialize,
        W: Write;
}

/// Handlebars-based template engine implementation.
pub struct HbsEngine {
    handlebars: Handlebars<'static>,
}

impl HbsEngine {
    pub fn new() -> Self {
        Self {
            handlebars: Handlebars::new(),
        }
    }

    /// Register a template by name.
    pub fn register_template_str(&mut self, name: &str, template: &str) -> Result<(), RenderError> {
        self.handlebars
            .register_template_string(name, template)
            .map_err(RenderError::from)
    }
}

impl TemplateEngine for HbsEngine {
    fn render_to_write<M, W>(
        &self,
        template_name: &str,
        model: &M,
        out: &mut W,
    ) -> Result<(), RenderError>
    where
        M: Serialize,
        W: Write,
    {
        self.handlebars
            .render_to_write(template_name, model, out)
            .map_err(RenderError::from)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::Serialize;
    use serde_json::json;
    use std::io::{self, Write};
    use std::str;

    #[derive(Serialize)]
    struct User {
        name: String,
    }

    #[test]
    fn hbs_engine_renders_simple_template_with_struct_model() {
        let mut engine = HbsEngine::new();
        engine
            .register_template_str("greeting", "Hello, {{name}}!")
            .expect("template registration should succeed");

        let model = User {
            name: "Alice".to_string(),
        };

        let mut out: Vec<u8> = Vec::new();
        engine
            .render_to_write("greeting", &model, &mut out)
            .expect("render should succeed");

        let s = str::from_utf8(&out).expect("output should be valid utf8");
        assert_eq!(s, "Hello, Alice!");
    }

    #[test]
    fn hbs_engine_renders_with_json_model() {
        let mut engine = HbsEngine::new();
        engine
            .register_template_str("greeting", "Hello, {{name}}!")
            .expect("template registration should succeed");

        let model = json!({ "name": "Bob" });

        let mut out: Vec<u8> = Vec::new();
        engine
            .render_to_write("greeting", &model, &mut out)
            .expect("render should succeed");

        let s = str::from_utf8(&out).expect("output should be valid utf8");
        assert_eq!(s, "Hello, Bob!");
    }

    #[test]
    fn hbs_engine_escapes_html_by_default() {
        let mut engine = HbsEngine::new();
        engine
            .register_template_str("html", "{{value}}")
            .expect("template registration should succeed");

        let model = json!({ "value": "<b>hi</b>" });

        let mut out: Vec<u8> = Vec::new();
        engine
            .render_to_write("html", &model, &mut out)
            .expect("render should succeed");

        let s = str::from_utf8(&out).expect("output should be valid utf8");
        assert_eq!(s, "&lt;b&gt;hi&lt;/b&gt;");
    }

    #[test]
    fn hbs_engine_supports_unescaped_html_with_triple_mustache() {
        use serde_json::json;

        let mut engine = HbsEngine::new();
        engine
            .register_template_str("html", "{{{raw_html}}} {{raw_html}}")
            .expect("template registration should succeed");

        let model = json!({
            "raw_html": "<b>hi</b>",
        });

        let mut out: Vec<u8> = Vec::new();
        engine
            .render_to_write("html", &model, &mut out)
            .expect("render should succeed");

        let rendered = std::str::from_utf8(&out).expect("output should be utf8");

        // First occurrence unescaped, second escaped.
        assert_eq!(rendered, "<b>hi</b> &lt;b&gt;hi&lt;/b&gt;");
    }

    #[test]
    fn render_missing_template_returns_error() {
        let engine = HbsEngine::new();
        let model = json!({});
        let mut out: Vec<u8> = Vec::new();

        let result = engine.render_to_write("does_not_exist", &model, &mut out);
        assert!(
            result.is_err(),
            "rendering a missing template should return an error"
        );
    }

    #[test]
    fn registering_invalid_template_returns_error() {
        let mut engine = HbsEngine::new();
        // Unclosed if-block should cause Handlebars to error.
        let result = engine.register_template_str("bad", "{{#if foo}}");

        assert!(
            result.is_err(),
            "registering an invalid template should return an error"
        );
    }

    struct FailingWriter;

    impl Write for FailingWriter {
        fn write(&mut self, _buf: &[u8]) -> io::Result<usize> {
            Err(io::Error::new(io::ErrorKind::Other, "write failed"))
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn writer_io_error_propagates_as_render_error() {
        let mut engine = HbsEngine::new();
        engine
            .register_template_str("simple", "Hello")
            .expect("template registration should succeed");

        let model = json!({});
        let mut failing = FailingWriter;

        let result = engine.render_to_write("simple", &model, &mut failing);
        assert!(
            result.is_err(),
            "IO error from writer should surface as RenderError"
        );
    }
}
