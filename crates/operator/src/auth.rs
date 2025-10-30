use axum::{
    body::Body,
    extract::State,
    http::{HeaderMap, HeaderValue, Request, StatusCode},
    middleware::Next,
    response::{IntoResponse, Response},
};
use base64::{engine::general_purpose, Engine};
use percent_encoding::percent_decode_str;
use rustls_pemfile::Item;
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::{
    fs,
    path::{Path, PathBuf},
};

/// Headers we expect from Nginx:
/// - X-Whisper-Internal: shared secret to trust the proxy hop
/// - X-Client-Cert: escaped PEM (nginx $ssl_client_escaped_cert)
const HDR_CLIENT_CERT: &str = "x-client-cert";
const HDR_CLIENT_CERT_DER: &str = "x-client-cert-der";

#[allow(dead_code)]
#[derive(Debug, Deserialize)]
pub struct FingerUser {
    pub version: Option<u32>,
    pub role: Option<String>, // expect "operator"
    pub name: Option<String>,
    pub email: Option<String>,
    // optional enforcement window (UTC RFC3339)
    pub not_before: Option<String>,
    pub not_after: Option<String>,
    // optional display fields:
    pub subject: Option<String>,
    pub issuer: Option<String>,
}

#[tracing::instrument(skip_all)]
pub async fn gate(
    State(app): State<crate::state::OperState>,
    req: Request<Body>,
    next: Next,
) -> Response {
    //tracing::debug!("Request={0:?}", req);
    // 1) Extract client cert from header passed by nginx
    let der = match extract_cert_der_or_pem(req.headers()) {
        Ok(b) => b,
        Err(msg) => return (StatusCode::UNAUTHORIZED, msg).into_response(),
    };

    //tracing::debug!("Testing der={der:?}");
    // 2) Fingerprint + authorize via <auth_dir>/<sha256>.toml (unchanged)
    let fp = sha256_hex(&der);

    // 3) Load authorization record from auth_dir/<fp>.toml
    tracing::debug!("Fingering User");
    let rec = match read_user(&app.auth_dir, &fp) {
        Ok(Some(r)) => r,
        Ok(None) => return (StatusCode::FORBIDDEN, "unknown fingerprint").into_response(),
        Err(_) => return (StatusCode::FORBIDDEN, "auth file error").into_response(),
    };

    // 4) Basic policy checks
    if !matches!(rec.role.as_deref(), Some("operator")) {
        return (StatusCode::FORBIDDEN, "unauthorized role").into_response();
    }
    if let (Some(nb), Some(na)) = (&rec.not_before, &rec.not_after) {
        if let (Ok(nb), Ok(na)) = (
            time::OffsetDateTime::parse(nb, &time::format_description::well_known::Rfc3339),
            time::OffsetDateTime::parse(na, &time::format_description::well_known::Rfc3339),
        ) {
            let now = time::OffsetDateTime::now_utc();
            if now < nb || now > na {
                return (StatusCode::FORBIDDEN, "certificate window invalid").into_response();
            }
        }
    }

    // 5) (Optional) attach identity for handlers (Extension/Fingerprint) — skip for now

    next.run(req).await
}

// ---------- helpers ----------

// $ssl_client_escaped_cert → PEM string
#[tracing::instrument(skip_all)]
fn unescape_nginx_pem(val: &HeaderValue) -> Result<String, String> {
    let s = val.to_str().map_err(|_| "header not utf-8".to_string())?;

    // Prefer percent-decoding (covers %0A, %20, etc.)
    if s.contains('%') {
        let decoded = percent_decode_str(s)
            .decode_utf8()
            .map_err(|_| "bad percent-encoding".to_string())?;
        return Ok(decoded.into_owned());
    }

    // Fallback: some setups use "\n" escaping
    if s.contains("\\n") {
        return Ok(s.replace("\\n", "\n"));
    }

    // Already plain PEM
    Ok(s.to_string())
}

#[tracing::instrument(skip_all)]
fn decode_b64(s: &str) -> Result<Vec<u8>, String> {
    general_purpose::STANDARD
        .decode(s)
        // try a few common variants in case padding/URL-safe differs
        .or_else(|_| general_purpose::STANDARD_NO_PAD.decode(s))
        .or_else(|_| general_purpose::URL_SAFE.decode(s))
        .or_else(|_| general_purpose::URL_SAFE_NO_PAD.decode(s))
        .map_err(|e| format!("bad base64: {e}"))
}

#[tracing::instrument(skip_all)]
fn extract_cert_der_or_pem(headers: &HeaderMap) -> Result<Vec<u8>, String> {
    if let Some(v) = headers.get(HDR_CLIENT_CERT_DER) {
        let b64 = v.to_str().map_err(|_| "bad cert header utf8".to_string())?;
        return decode_b64(b64).map_err(|_| "bad base64".to_string());
    }
    if let Some(v) = headers.get(HDR_CLIENT_CERT) {
        let pem = unescape_nginx_pem(v)?; // percent-decodes if needed
        return pem_to_der(&pem); // rustls-pemfile path
    }
    Err("missing client cert".into())
}

// Extract first PEM block → DER bytes
#[tracing::instrument(skip_all)]
fn pem_to_der(pem: &str) -> Result<Vec<u8>, String> {
    let mut rdr = std::io::Cursor::new(pem.as_bytes());
    match rustls_pemfile::read_one(&mut rdr).map_err(|e| e.to_string())? {
        Some(Item::X509Certificate(der)) => Ok(der.as_ref().to_vec()),
        _ => Err("no X509 certificate found".into()),
    }
}

#[tracing::instrument(skip_all)]
fn sha256_hex(bytes: &[u8]) -> String {
    let mut h = Sha256::new();
    h.update(bytes);
    hex::encode(h.finalize())
}

#[tracing::instrument(skip_all)]
fn read_user(dir: &Path, fp_hex: &str) -> anyhow::Result<Option<FingerUser>> {
    let path: PathBuf = dir.join(format!("{fp_hex}.toml"));
    tracing::debug!(?path);
    if !path.exists() {
        tracing::debug!("Returning NONE");
        return Ok(None);
    }
    let text = fs::read_to_string(path)?;
    tracing::debug!(text);
    let u: FingerUser = toml::from_str(&text)?;
    Ok(Some(u))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unescapes_pem() {
        let raw = HeaderValue::from_str(
            "-----BEGIN CERTIFICATE-----\\nZm9v\\n-----END CERTIFICATE-----\\n",
        )
        .unwrap();
        let pem = unescape_nginx_pem(&raw).unwrap();
        assert!(pem.contains("BEGIN CERTIFICATE"));
        assert!(pem.contains("\n"));
    }

    #[test]
    fn sha256_works() {
        let fp = sha256_hex(b"abc");
        // known SHA-256 of "abc"
        assert_eq!(
            fp,
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }
}
