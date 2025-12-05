// crates/edge/src/router.rs

use crate::fs::{ext::ThemeBinding, index::ContentMgr};
use actix_web::{
    dev::HttpServiceFactory, http::Method as ActixMethod, web, HttpMessage, HttpRequest,
    HttpResponse,
};
use adapt::runtime::bootstrap::RuntimeHandles;
use adapt::runtime::plugin_actor::PluginRuntimeClient;
use adapt::runtime::theme_actor::ThemeRuntimeClient;
use domain::content::Content;
use serve::{
    render::{
        http::{RequestContext, ResponseBodySpec},
        pipeline::{render_html_string_to, render_html_template_to, render_json_to},
        template::TemplateRegistry,
    },
    resolver::{build_request_context, resolve},
};
use std::collections::HashMap;
use std::path::PathBuf;
use tracing::{debug, error};

/// Per-theme state carried on the scope.
#[derive(Clone)]
struct ThemeAppState {
    theme_client: ThemeRuntimeClient,
    plugin_client: PluginRuntimeClient,
    plugin_ids: Vec<String>,
    /// Theme identifier (as known to the JS runtime).
    theme_id: String,
    /// Filesystem root for this theme's templates directory.
    ///
    /// Typically `<theme-dir>/templates`.
    template_root: PathBuf,
    content_mgr: ContentMgr,
}

// ─────────────────────────────────────────────────────────────────────────────
// Router construction
// ─────────────────────────────────────────────────────────────────────────────

/// Build the main Actix scope given:
/// - runtime handles (theme + plugin actors)
/// - a list of theme bindings (mount path → theme id + template root)
///
/// This returns an `impl HttpServiceFactory` you can mount directly on
/// `HttpServer::new(move || build_app_router(handles.clone(), bindings.clone()))`.
#[tracing::instrument(skip_all)]
pub fn build_app_router(
    root_dir: PathBuf,
    handles: RuntimeHandles,
    bindings: Vec<ThemeBinding>,
) -> impl HttpServiceFactory {
    let theme_client = handles.theme_client.clone();
    let plugin_client = handles.plugin_client.clone();
    let plugin_ids: Vec<String> = handles
        .plugin_configs
        .iter()
        .map(|cfg| cfg.id.clone())
        .collect();

    // Root "container" scope; we add one nested scope per ThemeBinding.
    let mut root = web::scope("");

    for binding in bindings {
        let mount_path = binding.mount_path.clone();
        let theme_id = binding.theme_id.clone();
        let template_root = binding.template_root.clone();

        let state = ThemeAppState {
            theme_client: theme_client.clone(),
            plugin_client: plugin_client.clone(),
            plugin_ids: plugin_ids.clone(),
            theme_id,
            template_root,
            content_mgr: ContentMgr::new(root_dir.clone()),
        };

        // Normalize root theme mount: treat "/" as "" so that both "/"
        // and "/index.html" (and everything else) route correctly.
        let scope_path = if mount_path == "/" { "" } else { &mount_path };

        let scope = web::scope(scope_path)
            .app_data(web::Data::new(state))
            // "/" under this mount (for the docsy demo: "/")
            .route("/", web::to(theme_route_handler))
            // everything else under this mount, e.g. "/index.html",
            // "/docs/search.html", etc.
            .route("/{tail:.*}", web::to(theme_route_handler));

        root = root.service(scope);
    }

    root
}

// ─────────────────────────────────────────────────────────────────────────────
// Helpers
// ─────────────────────────────────────────────────────────────────────────────

/// Very small query parser into `HashMap<String, String>`.
fn parse_query_params(raw_query: &str) -> HashMap<String, String> {
    let mut out = HashMap::new();

    if raw_query.is_empty() {
        return out;
    }

    for pair in raw_query.split('&') {
        if pair.is_empty() {
            continue;
        }
        let mut it = pair.splitn(2, '=');
        let k = it.next().unwrap_or("").to_string();
        if k.is_empty() {
            continue;
        }
        let v = it.next().unwrap_or("").to_string();
        out.insert(k, v);
    }

    out
}

/// Convert Actix method → `http` 1.x Method for the resolver.
fn to_http_method(m: &ActixMethod) -> http::Method {
    http::Method::from_bytes(m.as_str().as_bytes()).unwrap_or(http::Method::GET)
}

/// Convert Actix header map → `http` 1.x HeaderMap for the resolver.
fn to_http_headers(actix_headers: &actix_web::http::header::HeaderMap) -> http::HeaderMap {
    let mut out = http::HeaderMap::new();

    for (name, value) in actix_headers.iter() {
        if let Ok(v_str) = value.to_str() {
            if let Ok(hname) = http::header::HeaderName::from_bytes(name.as_str().as_bytes()) {
                if let Ok(hval) = http::HeaderValue::from_str(v_str) {
                    out.insert(hname, hval);
                }
            }
        }
    }

    out
}

// ─────────────────────────────────────────────────────────────────────────────
// Handler
// ─────────────────────────────────────────────────────────────────────────────

/// Actix handler for all requests under a given theme mount.
///
/// State carries `theme_client`, `plugin_client`, plugin IDs, and the template root.
#[tracing::instrument(skip_all)]
async fn theme_route_handler(state: web::Data<ThemeAppState>, req: HttpRequest) -> HttpResponse {
    let ThemeAppState {
        theme_client,
        plugin_client,
        plugin_ids,
        theme_id,
        template_root,
        content_mgr,
    } = state.get_ref().clone();

    let path_for_log = req.uri().path().to_string();
    debug!("theme_route_handler hit for path: {}", path_for_log);

    // Prefer a RequestContext injected by some earlier layer (if any),
    // otherwise build it directly here from the resolver.
    let base_ctx: RequestContext = if let Some(existing) = req.extensions().get::<RequestContext>()
    {
        existing.clone()
    } else {
        let path = req.uri().path().to_string();
        let actix_method = req.method().clone();
        let headers_actix = req.headers().clone();
        let raw_query = req.uri().query().unwrap_or_default().to_string();
        let query_params = parse_query_params(&raw_query);

        let method = to_http_method(&actix_method);
        let headers = to_http_headers(&headers_actix);

        let resolved = match resolve(&content_mgr, &path, &method).await {
            Ok(r) => r,
            Err(_e) => Content::empty(),
        };

        build_request_context(path, method, headers, query_params, resolved)
    };

    debug!("theme_id: {}", theme_id);

    // ─────────────────────────────────────────────────────────────────────
    // Run plugin BEFORE hooks (in configured order).
    // ─────────────────────────────────────────────────────────────────────
    let mut ctx = base_ctx;
    for plugin_id in &plugin_ids {
        debug!("Running before_plugin for plugin_id={}", plugin_id);
        match plugin_client.before_plugin(plugin_id.clone(), ctx).await {
            Ok(new_ctx) => {
                ctx = new_ctx;
            }
            Err(e) => {
                error!(
                    "before_plugin failed for plugin_id={} on theme {}: {}",
                    plugin_id, theme_id, e
                );
                return HttpResponse::InternalServerError().body("Plugin before error");
            }
        }
    }

    // NOTE: we currently do NOT run after_plugin hooks, because the theme
    // runtime API returns a ResponseBodySpec, not an updated RequestContext.
    // To wire `after_*` correctly, we'd need to let the theme runtime operate
    // on `&mut RequestContext` and return it, or move the plugin calls inside
    // the theme actor so they share the same context instance.

    // Ask the theme actor to render a ResponseBodySpec from the (possibly
    // plugin-mutated) RequestContext.
    let result = theme_client.render(&theme_id, ctx).await;
    debug!("The ResponseBodySpec: {:?}", result);

    // NOTE: body patches (from plugins/themes) are not yet wired here.
    let body_patches: &[serve::render::recommendation::BodyPatch] = &[];

    match result {
        // HtmlTemplate – detect engine + render from /templates
        Ok(ResponseBodySpec::HtmlTemplate { template, model }) => {
            let registry = TemplateRegistry::new(template_root);

            let mut buf = Vec::new();
            if let Err(e) =
                render_html_template_to(&registry, &template, &model, body_patches, &mut buf)
            {
                error!(
                    "HtmlTemplate render failed for theme {} and template {}: {}",
                    theme_id, template, e
                );
                HttpResponse::InternalServerError().body("Template rendering error")
            } else {
                HttpResponse::Ok()
                    .insert_header(("Content-Type", "text/html; charset=utf-8"))
                    .body(buf)
            }
        }

        // HtmlString – routed through same render pipeline (body patches empty for now).
        Ok(ResponseBodySpec::HtmlString(html)) => {
            let mut buf = Vec::new();
            if let Err(e) = render_html_string_to(&html, body_patches, &mut buf) {
                error!("HtmlString render failed for theme {}: {}", theme_id, e);
                HttpResponse::InternalServerError().body("HTML rendering error")
            } else {
                HttpResponse::Ok()
                    .insert_header(("Content-Type", "text/html; charset=utf-8"))
                    .body(buf)
            }
        }

        // JsonValue – regex / JSON body patches (empty for now).
        Ok(ResponseBodySpec::JsonValue(val)) => {
            let mut buf = Vec::new();
            if let Err(e) = render_json_to(&val, body_patches, &mut buf) {
                error!("JSON render failed for theme {}: {}", theme_id, e);
                HttpResponse::InternalServerError().body("JSON rendering error")
            } else {
                HttpResponse::Ok()
                    .insert_header(("Content-Type", "application/json"))
                    .body(buf)
            }
        }

        Ok(ResponseBodySpec::None | ResponseBodySpec::Unset) => HttpResponse::NoContent().finish(),

        Err(e) => {
            error!("Theme runtime error: {}", e);
            HttpResponse::InternalServerError().body("Theme runtime error")
        }
    }
}
