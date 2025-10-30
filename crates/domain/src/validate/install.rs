use anyhow::{bail, Result};
use types::{DbKind, InstallPlan};
use url::Url;

pub fn validate_plan(p: &InstallPlan) -> anyhow::Result<()> {
    // language
    if p.language != "en-US" { bail!("unsupported language"); }

    // base_url: absolute + host suffix (or localhost) – use your existing rule
    super::site::validate_base_url(&p.base_url.to_string()).map_err(anyhow::Error::msg)?;

    // db urls
    validate_db_url(&p.db_ops_url)?;
    validate_db_url(&p.db_content_url)?;
    if matches!(p.db_kind, DbKind::Remote) && (!p.split_content && p.db_content_url != p.db_ops_url) {
        // ok if equal; otherwise fine when split_content=true
    }
    Ok(())
}

pub fn validate_db_url(u: &Url) -> Result<()> {
    // Accept libsql/http(s) or sqlite://
    match u.scheme() {
        "sqlite" | "file" | "libsql" | "http" | "https" => Ok(()),
        _ => bail!("unsupported db url scheme: {}", u.scheme()),
    }
}