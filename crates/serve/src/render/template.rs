// crates/serve/src/render/template.rs

use super::error::RenderError;
use handlebars::{
    handlebars_helper, Context, Handlebars, Helper, HelperDef, HelperResult, Output, RenderContext,
};
use handlebars_misc_helpers as misc;
use minijinja::{Environment as MiniJinjaEnv, Error as MiniJinjaError};
use serde::Serialize;
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use tera::{Context as TeraContext, Error as TeraError, Tera};

/// Trait for template engines that can render to an arbitrary `Write`.
///
/// This is intentionally minimal and is implemented by both `HbsEngine`
/// (historical) and the multi-engine `TemplateRegistry`.
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

// ─────────────────────────────────────────────────────────────────────────────
// Backwards-compatible Handlebars-only engine
// ─────────────────────────────────────────────────────────────────────────────

/// Handlebars-based template engine implementation.
///
/// Kept for backwards compatibility. Newer code should use
/// `TemplateRegistry`, which can select among multiple engines.
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

// ─────────────────────────────────────────────────────────────────────────────
// Multi-engine registry (Handlebars / MiniJinja / Tera)
// ─────────────────────────────────────────────────────────────────────────────

/// Internal enum to decide which engine to use.
/// This does **not** escape this module (no dyn on the hot path).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EngineKind {
    Handlebars,
    MiniJinja,
    Tera,
}

/// Very small helper: turn any display-ish value into an `io::Error`.
impl EngineKind {
    fn from_extension(ext: &str) -> Option<Self> {
        let e = ext.to_ascii_lowercase();
        match e.as_str() {
            // Handlebars
            "hbs" | "handlebars" => Some(EngineKind::Handlebars),

            // MiniJinja
            "j2" | "jinja2" | "jinja" | "mj" => Some(EngineKind::MiniJinja),

            // Tera
            "tera" => Some(EngineKind::Tera),

            _ => None,
        }
    }
}

#[derive(Clone, Copy)]
struct DumpRoot;

impl HelperDef for DumpRoot {
    fn call<'reg: 'rc, 'rc>(
        &self,
        _h: &Helper<'rc>,
        _r: &Handlebars<'reg>,
        ctx: &Context,
        _rc: &mut RenderContext<'reg, 'rc>,
        out: &mut dyn Output,
    ) -> HelperResult {
        let json = ctx.data();
        let s = serde_json::to_string_pretty(json).unwrap_or_else(|_| "<invalid json>".to_string());
        out.write(s.as_str())?;
        Ok(())
    }
}

/// A per-theme registry that can render templates via Handlebars, MiniJinja,
/// or Tera based solely on the template filename’s extension.
///
/// There is intentionally **no caching**: each call reads the template
/// file from disk and constructs the engine environment just for that call.
/// This keeps the implementation simple and matches your “no caching” answer.
pub struct TemplateRegistry {
    template_root: PathBuf,
}

impl TemplateRegistry {
    /// Create a new registry rooted at `<theme_dir>/templates`.
    ///
    /// The directory is not required to exist at construction time – errors
    /// are reported only when attempting to render a specific template.
    pub fn new(template_root: PathBuf) -> Self {
        Self { template_root }
    }

    /// Helper for creating an `io::Error` from a display-able value.
    fn io_other(msg: impl Into<String>) -> io::Error {
        io::Error::new(io::ErrorKind::Other, msg.into())
    }

    /// Resolve a logical template name like `"home.hbs"` against the root.
    fn resolve_path(&self, name: &str) -> PathBuf {
        self.template_root.join(name)
    }

    /// Read the template source and decide which engine to use.
    fn load_template(&self, template_name: &str) -> Result<(EngineKind, String), RenderError> {
        let path = self.resolve_path(template_name);

        let src = fs::read_to_string(&path).map_err(|e| {
            RenderError::Io(Self::io_other(format!(
                "failed to read template {:?}: {}",
                path, e
            )))
        })?;

        let ext = Path::new(template_name)
            .extension()
            .and_then(|s| s.to_str())
            .unwrap_or("");

        let kind = EngineKind::from_extension(ext).ok_or_else(|| {
            RenderError::Io(Self::io_other(format!(
                "unsupported template extension {:?} for {:?}",
                ext, template_name
            )))
        })?;

        Ok((kind, src))
    }

    fn render_with_handlebars<M, W>(
        &self,
        template_name: &str,
        src: &str,
        model: &M,
        out: &mut W,
    ) -> Result<(), RenderError>
    where
        M: Serialize,
        W: Write,
    {
        handlebars_helper!(dump_json: |v: Json| {
            serde_json::to_string_pretty(&v).unwrap_or_else(|_| "<invalid json>".into())
        });

        let mut hbs = Handlebars::new();
        hbs.register_template_string(template_name, src)
            .map_err(RenderError::from)?;
        misc::register(&mut hbs);
        hbs.register_helper("dump", Box::new(dump_json));
        hbs.register_helper("dump_root", Box::new(DumpRoot));
        hbs.render_to_write(template_name, model, out)
            .map_err(RenderError::from)
    }

    fn render_with_minijinja<M, W>(
        &self,
        template_name: &str,
        src: &str,
        model: &M,
        out: &mut W,
    ) -> Result<(), RenderError>
    where
        M: Serialize,
        W: Write,
    {
        let mut env = MiniJinjaEnv::new();

        env.add_template(template_name, src)
            .map_err(|e: MiniJinjaError| RenderError::Io(Self::io_other(e.to_string())))?;

        let tmpl = env
            .get_template(template_name)
            .map_err(|e: MiniJinjaError| RenderError::Io(Self::io_other(e.to_string())))?;

        let rendered = tmpl
            .render(model)
            .map_err(|e: MiniJinjaError| RenderError::Io(Self::io_other(e.to_string())))?;

        out.write_all(rendered.as_bytes()).map_err(RenderError::Io)
    }

    fn render_with_tera<M, W>(
        &self,
        template_name: &str,
        src: &str,
        model: &M,
        out: &mut W,
    ) -> Result<(), RenderError>
    where
        M: Serialize,
        W: Write,
    {
        let mut tera = Tera::default();

        tera.add_raw_template(template_name, src)
            .map_err(|e: TeraError| RenderError::Io(Self::io_other(e.to_string())))?;

        let ctx = TeraContext::from_serialize(model)
            .map_err(|e: TeraError| RenderError::Io(Self::io_other(e.to_string())))?;

        let rendered = tera
            .render(template_name, &ctx)
            .map_err(|e: TeraError| RenderError::Io(Self::io_other(e.to_string())))?;

        out.write_all(rendered.as_bytes()).map_err(RenderError::Io)
    }
}

impl TemplateEngine for TemplateRegistry {
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
        let (kind, src) = self.load_template(template_name)?;

        match kind {
            EngineKind::Handlebars => self.render_with_handlebars(template_name, &src, model, out),
            EngineKind::MiniJinja => self.render_with_minijinja(template_name, &src, model, out),
            EngineKind::Tera => self.render_with_tera(template_name, &src, model, out),
        }
    }
}
