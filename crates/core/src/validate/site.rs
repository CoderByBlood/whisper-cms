
use url::Url;

pub fn validate_base_url(url: &str) -> Result<(), String> {
    Url::parse(url).map(|_| ()).map_err(|e| e.to_string())
}
pub fn validate_site_name(name: &str) -> Result<(), String> {
    if name.trim().is_empty() { return Err("site name empty".into()); }
    Ok(())
}
pub fn validate_timezone(tz: &str) -> Result<(), String> {
    if tz.trim().is_empty() { return Err("timezone empty".into()); }
    Ok(())
}
