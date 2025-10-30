use anyhow::Result;
use url::{Host, Url};

/// Require http/https scheme and a host.
#[tracing::instrument(skip_all)]
pub fn validate_base_url(s: &str) -> Result<(), String> {
    let url = Url::parse(s).map_err(|e| e.to_string())?;

    match url.scheme() {
        "http" | "https" => {}
        other => return Err(format!("unsupported scheme: {other}")),
    }

    // Host must be localhost, an IP, or a domain with a dot.
    let ok_host = match url.host() {
        Some(Host::Domain(d)) => d == "localhost" || d.contains('.'),
        Some(Host::Ipv4(_)) | Some(Host::Ipv6(_)) => true,
        None => false,
    };
    if !ok_host {
        return Err("host must be localhost, an IP, or a domain with a suffix".into());
    }

    // Avoid accidental creds in config
    if !url.username().is_empty() || url.password().is_some() {
        return Err("userinfo not allowed in base URL".into());
    }

    Ok(())
}

#[tracing::instrument(skip_all)]
pub fn validate_site_name(name: &str) -> Result<(), String> {
    if name.trim().is_empty() {
        return Err("site name empty".into());
    }
    if name.len() > 120 {
        return Err("site name too long".into());
    }
    Ok(())
}

/// MVP: non-empty. (You can tighten later with a curated IANA list or a tz crate.)
#[tracing::instrument(skip_all)]
pub fn validate_timezone(tz: &str) -> Result<(), String> {
    if tz.trim().is_empty() {
        return Err("timezone empty".into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base_url_ok() {
        assert!(validate_base_url("https://example.com").is_ok());
        assert!(validate_base_url("http://sub.intranet.local:8080").is_ok());
        assert!(validate_base_url("http://localhost:3000").is_ok());
        assert!(validate_base_url("https://127.0.0.1").is_ok());
        assert!(validate_base_url("https://[::1]").is_ok());
    }

    #[test]
    fn base_url_bad() {
        // wrong scheme
        assert!(validate_base_url("mailto:foo").is_err());
        // empty authority (three slashes) -> no host
        assert!(validate_base_url("https:///nohost").is_err());
        // single-label domain without suffix (explicitly disallowed)
        assert!(validate_base_url("https://intranet").is_err());
        // userinfo not allowed
        assert!(validate_base_url("https://user:pass@example.com").is_err());
    }

    #[test]
    fn name_ok_and_bad() {
        assert!(validate_site_name("My Site").is_ok());
        assert!(validate_site_name("   ").is_err());
    }

    #[test]
    fn tz_ok_and_bad() {
        assert!(validate_timezone("UTC").is_ok());
        assert!(validate_timezone("   ").is_err());
    }
}
