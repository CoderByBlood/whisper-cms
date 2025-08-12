use serde::Deserialize;
use serde::de::Deserializer;

#[derive(Deserialize)]
pub struct LangForm { pub language: String } // "en-US"

#[derive(Deserialize)]
pub struct DbForm {
    pub db_kind: String,            // "embedded" | "remote"
    #[serde(default, deserialize_with = "de_checkbox_bool")] pub split_content: bool,

    // Embedded fields (optional; default if empty)
    pub ops_path: Option<String>,       // e.g., "data/ops.db"
    pub content_path: Option<String>,   // e.g., "data/content.db"

    // Remote fields
    pub ops_url: Option<String>,
    pub ops_token: Option<String>,
    pub content_url: Option<String>,
    pub content_token: Option<String>,
}

#[derive(Deserialize)]
pub struct SiteForm {
    pub site_name: String,
    pub base_url: String,
    pub timezone: String,
    pub admin_password: String,
}


fn de_checkbox_bool<'de, D>(de: D) -> Result<bool, D::Error>
where
    D: Deserializer<'de>,
{
    // Support one or many values (in case a hidden+checkbox slips in)
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Many<T> { One(T), Many(Vec<T>) }

    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Repr { Bool(bool), Str(String), Int(i64) }

    // If the field is missing entirely, default to false
    let val: Option<Many<Repr>> = Option::deserialize(de)?;
    let repr = match val {
        None => return Ok(false),
        Some(Many::One(r)) => r,
        Some(Many::Many(mut v)) => v.pop().unwrap_or(Repr::Str(String::new())),
    };

    let b = match repr {
        Repr::Bool(b) => b,
        Repr::Int(n) => n != 0,
        Repr::Str(s) => {
            let s = s.trim().to_ascii_lowercase();
            matches!(s.as_str(), "true" | "on" | "1" | "yes" | "y")
        }
    };
    Ok(b)
}