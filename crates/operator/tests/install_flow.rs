use std::time::Duration;

use axum::{
    body::{to_bytes, Body},
    http::{Request, StatusCode},
    response::Response,
    Router,
};
use base64::{engine::general_purpose, Engine as _};
use infra::config::paths::{with_paths, Paths};
use tempfile::TempDir;
use tower::ServiceExt; // oneshot

use operator::{app_router};
use operator::{phase::Phase, state::OperState};

type AppSvc = tower_http::normalize_path::NormalizePath<Router>;

// === Build app like main ===
fn build_test_app(app: OperState) -> AppSvc {
    app_router(app)
}

// === Small IO helpers ===
async fn read(resp: Response) -> (StatusCode, String) {
    let status = resp.status();
    let bytes = to_bytes(resp.into_body(), 1 * 1024 * 1024).await.unwrap(); // Axum 0.8
    (status, String::from_utf8_lossy(&bytes).into_owned())
}

async fn get(app: &AppSvc, path: &str) -> (StatusCode, String) {
    let req = Request::get(path).body(Body::empty()).unwrap();
    let resp = app.clone().oneshot(req).await.unwrap();
    read(resp).await
}

// === Auth wiring for tests ===
// We’ll send x-client-cert-der (base64 DER). Your middleware hashes these bytes and
const HDR_CLIENT_CERT_DER: &str = "x-client-cert-der";

fn test_cert_der() -> Vec<u8> {
    // Any byte string works; auth doesn’t parse the cert, it only hashes the DER.
    b"test-cert-der".to_vec()
}

fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    h.update(bytes);
    hex::encode(h.finalize())
}

fn write_fingerprint_file(site: &std::path::Path, fp_hex: &str) {
    let dir = site.join(".whisper/auth");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join(format!("{fp_hex}.toml")),
        r#"role = "operator"
name = "test"
"#,
    )
    .unwrap();
}

fn apply_auth_headers(mut req: Request<Body>, der: &[u8]) -> Request<Body> {
    use axum::http::header::HeaderValue;
    let b64 = general_purpose::STANDARD.encode(der);
    req.headers_mut()
        .insert(HDR_CLIENT_CERT_DER, HeaderValue::from_str(&b64).unwrap());
    // Some setups care about scheme; harmless to include:
    req.headers_mut()
        .insert("x-forwarded-proto", HeaderValue::from_static("https"));
    req
}

async fn get_authed(app: &AppSvc, path: &str, der: &[u8]) -> (StatusCode, String) {
    let req = Request::get(path).body(Body::empty()).unwrap();
    let req = apply_auth_headers(req, der);
    let resp = app.clone().oneshot(req).await.unwrap();
    read(resp).await
}

async fn post_form_authed(
    app: &AppSvc,
    path: &str,
    form: &[(&str, &str)],
    der: &[u8],
) -> (StatusCode, String) {
    let body = serde_urlencoded::to_string(form).unwrap();
    let req = Request::post(path)
        .header("content-type", "application/x-www-form-urlencoded")
        .body(Body::from(body))
        .unwrap();
    let req = apply_auth_headers(req, der);
    let resp = app.clone().oneshot(req).await.unwrap();
    read(resp).await
}

// Fresh site + app in Install phase + matching fingerprint file
async fn fresh_install_app_with_auth(tmp: &TempDir) -> (&TempDir, AppSvc, Vec<u8>) {
    // Clear any stale resume marker
    let _ = std::fs::remove_file(tmp.path().join("config/install.json"));

    // Prepare DER + fingerprint file
    let der = test_cert_der();
    let fp_hex = sha256_hex(&der);
    write_fingerprint_file(tmp.path(), &fp_hex);

    let app_state = OperState::new(tmp.path());
    app_state
        .phase
        .transition_to(&app_state, Phase::Install)
        .await
        .unwrap();

    let app = build_test_app(app_state);
    (tmp, app, der)
}

// ===================== TESTS =====================

#[tokio::test]
async fn boot_maintenance_and_welcome_redirect() {
    let tmp = TempDir::new().unwrap();
    let paths = Paths::new(tmp.path());
    let with = async move {
        let (_tmp, app, der) = fresh_install_app_with_auth(&tmp).await;

        // Unauthed root is gated now
        let (s_root_unauth, _) = get(&app, "/").await;
        assert_eq!(s_root_unauth, StatusCode::UNAUTHORIZED);

        // Authed root during Install shows maintenance (503)
        let (s_root_auth, _) = get_authed(&app, "/", &der).await;
        assert_eq!(s_root_auth, StatusCode::SERVICE_UNAVAILABLE);

        // Authed /install redirects to the first step
        let (s_install, _) = get_authed(&app, "/install", &der).await;
        assert!(s_install.is_redirection());
    };

    with_paths(paths, with).await;
}

#[tokio::test]
async fn db_form_validation_and_checkbox_semantics() {
    let tmp = TempDir::new().unwrap();
    let paths = Paths::new(tmp.path());
    let with = async {
        let (_tmp, app, der) = fresh_install_app_with_auth(&tmp).await;

        // Step 1: language (happy)
        let (s_lang, _) =
            post_form_authed(&app, "/install/lang", &[("language", "en-US")], &der).await;
        assert!(s_lang.is_redirection());

        // Step 2: database (unhappy) — remote without ops_url should 4xx
        let (s_bad, _b_bad) =
            post_form_authed(&app, "/install/db", &[("db_kind", "remote")], &der).await;
        assert!(s_bad.is_client_error());

        // Step 2: database (happy) — embedded, split omitted → false
        let (s_emb, _) = post_form_authed(
            &app,
            "/install/db",
            &[
                ("db_kind", "embedded"),
                ("ops_path", "data/ops.db"),
                ("content_path", "data/content.db"),
            ],
            &der,
        )
        .await;
        assert!(s_emb.is_redirection());

        // Step 2: database (remote, split='on') — accept checkbox semantics
        let (s_remote_on, _) = post_form_authed(
            &app,
            "/install/db",
            &[
                ("db_kind", "remote"),
                ("ops_url", "libsql://example.turso.io"),
                ("ops_token", "tok1"),
                ("split_content", "on"),
                ("content_url", "libsql://example.turso.io"),
                ("content_token", "tok2"),
            ],
            &der,
        )
        .await;
        assert!(s_remote_on.is_redirection());

        // Step 2: database (remote, no split) — omit split & content_* fields
        let (s_remote_nosplit, _) = post_form_authed(
            &app,
            "/install/db",
            &[
                ("db_kind", "remote"),
                ("ops_url", "libsql://example.turso.io"),
                ("ops_token", "tok1"),
            ],
            &der,
        )
        .await;
        assert!(s_remote_nosplit.is_redirection());
    };

    with_paths(paths, with).await;
}

#[tokio::test]
async fn site_validation_rejects_bad_base_url() {
    let tmp = TempDir::new().unwrap();
    let paths = Paths::new(tmp.path());
    let with = async {
        let (_tmp, app, der) = fresh_install_app_with_auth(&tmp).await;

        // lang ok
        let _ = post_form_authed(&app, "/install/lang", &[("language", "en-US")], &der).await;

        // db: embedded ok
        let _ = post_form_authed(
            &app,
            "/install/db",
            &[
                ("db_kind", "embedded"),
                ("ops_path", "data/ops.db"),
                ("content_path", "data/content.db"),
            ],
            &der,
        )
        .await;

        // site: bad base_url (no suffix and not localhost) should 4xx
        let (s_bad, _b_bad) = post_form_authed(
            &app,
            "/install/site",
            &[
                ("site_name", "Test Site"),
                ("base_url", "https://intranet"),
                ("timezone", "UTC"),
                ("admin_password", "CorrectHorseBatteryStaple"),
            ],
            &der,
        )
        .await;
        assert!(s_bad.is_client_error());
    };

    with_paths(paths, with).await;
}

#[tokio::test]
async fn full_install_happy_path_embedded_and_router_swap() {
    let tmp = TempDir::new().unwrap();
    let paths = Paths::new(tmp.path());
    let with = async {
        let (_tmp, app, der) = fresh_install_app_with_auth(&tmp).await;

        // 1) lang
        let (s1, _) = post_form_authed(&app, "/install/lang", &[("language", "en-US")], &der).await;
        assert!(s1.is_redirection());

        // 2) db (embedded)
        let (s2, _) = post_form_authed(
            &app,
            "/install/db",
            &[
                ("db_kind", "embedded"),
                ("ops_path", "data/ops.db"),
                ("content_path", "data/content.db"),
            ],
            &der,
        )
        .await;
        assert!(s2.is_redirection());

        // 3) site
        let (s3, _) = post_form_authed(
            &app,
            "/install/site",
            &[
                ("site_name", "My WhisperCMS"),
                ("base_url", "https://example.com"),
                ("timezone", "UTC"),
                ("admin_password", "CorrectHorseBatteryStaple"),
            ],
            &der,
        )
        .await;
        assert!(s3.is_redirection());

        // 4) run page renders (authed)
        let (s4, b4) = get_authed(&app, "/install/run", &der).await;
        assert_eq!(s4, StatusCode::OK);
        assert!(b4.contains("Install progress"));

        // 5) start run (authed; returns 204)
        let (s5, _) = post_form_authed(&app, "/install/run", &[], &der).await;
        assert_eq!(s5, StatusCode::NO_CONTENT);

        // 6) wait for Serve phase: root becomes 200 when authed, stays 401 unauth
        let core_path = tmp.path().join("config/core.toml");
        let resume_path = tmp.path().join("config/install.json");

        let wait_swap = async {
            let mut installed_seen = false;
            let start = std::time::Instant::now();

            loop {
                // Check installed flag to know if steps finished
                if core_path.exists() {
                    if let Ok(txt) = std::fs::read_to_string(&core_path) {
                        if txt.contains("installed = true") {
                            installed_seen = true;
                        }
                    }
                }

                // Check the router
                let (s, b) = get_authed(&app, "/", &der).await;
                if s == StatusCode::OK && b.contains("WhisperCMS is running") {
                    break Ok::<(), String>(());
                }

                // Give a short grace window *after* installed flips
                if installed_seen && start.elapsed() > Duration::from_secs(5) {
                    let mut diag = String::new();
                    if let Ok(txt) = std::fs::read_to_string(&core_path) {
                        diag.push_str(&format!("\ncore.toml:\n{}\n", txt));
                    }
                    if let Ok(txt) = std::fs::read_to_string(&resume_path) {
                        diag.push_str(&format!("install.json:\n{}\n", txt));
                    } else {
                        diag.push_str("\ninstall.json: (missing)\n");
                    }
                    break Err(format!(
                        "installed=true but router not swapped after 5s grace.{}",
                        diag
                    ));
                }

                tokio::time::sleep(Duration::from_millis(120)).await;
                if start.elapsed() > Duration::from_secs(40) {
                    // extend overall ceiling a bit
                    let mut diag = String::new();
                    if let Ok(txt) = std::fs::read_to_string(&core_path) {
                        diag.push_str(&format!("\ncore.toml:\n{}\n", txt));
                    }
                    if let Ok(txt) = std::fs::read_to_string(&resume_path) {
                        diag.push_str(&format!("install.json:\n{}\n", txt));
                    } else {
                        diag.push_str("\ninstall.json: (missing)\n");
                    }
                    break Err(format!("timeout waiting for Serve.{}", diag));
                }
            }
        };

        tokio::time::timeout(Duration::from_secs(45), wait_swap)
            .await
            .expect("timed out waiter task")
            .map_err(|why| panic!("{}", why))
            .unwrap();

        // config written
        assert!(tmp.path().join("config/core.toml").exists());

        // /install/* unmounted after swap:
        // unauth → still 401 (global gate)
        let (s_done_unauth, _) = get(&app, "/install/done").await;
        assert_eq!(s_done_unauth, StatusCode::UNAUTHORIZED);

        // authed → falls back to home (200)
        let (s_done_auth, b_done_auth) = get_authed(&app, "/install/done", &der).await;
        assert_eq!(s_done_auth, StatusCode::OK);
        assert!(b_done_auth.contains("WhisperCMS is running"));

        // Root unauth remains 401 (operator is fully protected)
        let (s_root_unauth, _) = get(&app, "/").await;
        assert_eq!(s_root_unauth, StatusCode::UNAUTHORIZED);

        // Root authed 200
        let (s_root_auth, _) = get_authed(&app, "/", &der).await;
        assert_eq!(s_root_auth, StatusCode::OK);
    };

    with_paths(paths, with).await;
}

#[tokio::test]
async fn sse_progress_endpoint_is_event_stream() {
    let tmp = TempDir::new().unwrap();
    let paths = Paths::new(tmp.path());
    let with = async {
        let (_tmp, app, der) = fresh_install_app_with_auth(&tmp).await;
        let req = apply_auth_headers(
            Request::get("/install/progress")
                .body(Body::empty())
                .unwrap(),
            &der,
        );
        let resp = app.clone().oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let cty = resp
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("");
        assert!(cty.starts_with("text/event-stream"));
    };

    with_paths(paths, with).await;
}
