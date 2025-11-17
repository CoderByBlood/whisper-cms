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
