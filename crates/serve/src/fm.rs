//! Front matter parser with standard fences:
//! - YAML: `---` (fenced, closing `---`)
//! - TOML: `+++` (fenced, closing `+++`)
//! - JSON: first non-whitespace is `{` (unfenced top-level object)

use serde::{Deserialize, Serialize};
use serde_json as json;
use serde_yml::{self as yml};
use std::error::Error as StdError;
use thiserror::Error;
use toml;

pub type Result<T> = std::result::Result<T, FrontMatterError>;

/// Which format (if any) was detected.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FrontFormat {
    Yaml,
    Toml,
    Json,
}

/// Schema-less front matter, preserved in its native value type.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum FrontValue {
    Yaml(yml::Value),
    Toml(toml::Value),
    Json(json::Value),
}

/// Result of parsing an input document.
#[derive(Debug)]
pub struct Parsed {
    pub format: Option<FrontFormat>,
    pub front_matter: Option<FrontValue>,
    pub body: String,
}

#[derive(Debug, Error)]
pub enum FrontMatterError {
    /// Underlying I/O while reading the file.
    #[error("I/O error while reading: {0}")]
    Io(#[from] std::io::Error),

    /// YAML front matter parse error.
    #[error("YAML front matter parse error: {0}")]
    Yaml(#[from] serde_yml::Error),

    /// TOML front matter parse error.
    #[error("TOML front matter parse error: {0}")]
    Toml(#[from] toml::de::Error),

    /// JSON front matter parse error.
    #[error("JSON front matter parse error: {0}")]
    Json(#[from] serde_json::Error),

    /// Transparent catch-all for any other error you want to bubble up.
    /// Keeps the original Display/Debug and source chain intact.
    #[error(transparent)]
    Mapped(#[from] Box<dyn StdError + Send + Sync>),
}

/// Parse a document with optional **YAML/TOML/JSON** front matter.
/// - YAML: top-of-file `---` fence … closing `---`
/// - TOML: top-of-file `+++` fence … closing `+++`
/// - JSON: first non-whitespace is `{` (parses one balanced top-level object)
pub fn parse_front_matter(txt: String) -> Result<Parsed> {
    let mut text = txt;

    // Strip UTF-8 BOM if present
    if text.as_bytes().starts_with(&[0xEF, 0xBB, 0xBF]) {
        text.drain(..3);
    }

    // YAML fenced with --- ... ---
    if let Some(rest) = text
        .strip_prefix("---\n")
        .or_else(|| text.strip_prefix("---\r\n"))
    {
        if let Some((fm, body)) = take_until_fence(rest, "---") {
            let val: yml::Value = yml::from_str(fm)?;
            return Ok(Parsed {
                format: Some(FrontFormat::Yaml),
                front_matter: Some(FrontValue::Yaml(val)),
                body: body.to_owned(),
            });
        } else {
            // Unterminated fence → treat as no front matter
            return Ok(Parsed {
                format: None,
                front_matter: None,
                body: text,
            });
        }
    }

    // TOML fenced with +++ ... +++
    if let Some(rest) = text
        .strip_prefix("+++\n")
        .or_else(|| text.strip_prefix("+++\r\n"))
    {
        if let Some((fm, body)) = take_until_fence(rest, "+++") {
            let val: toml::Value = toml::from_str(fm)?;
            return Ok(Parsed {
                format: Some(FrontFormat::Toml),
                front_matter: Some(FrontValue::Toml(val)),
                body: body.to_owned(),
            });
        } else {
            return Ok(Parsed {
                format: None,
                front_matter: None,
                body: text,
            });
        }
    }

    // JSON: first non-WS must be '{' and we parse one top-level object; body follows it.
    if first_non_ws_is_lbrace(&text) {
        if let Some((json_str, body)) = slice_top_level_json_object(&text) {
            let val: json::Value = json::from_str(json_str)?;
            return Ok(Parsed {
                format: Some(FrontFormat::Json),
                front_matter: Some(FrontValue::Json(val)),
                body: body.to_owned(),
            });
        } else {
            // Looks like JSON but not a balanced object → try parse to return a proper error
            let res: std::result::Result<json::Value, _> = json::from_str(&text);
            if let Err(source) = res {
                return Err(FrontMatterError::Json(source));
            }
            // If it (unexpectedly) parses, treat as no front matter
            return Ok(Parsed {
                format: None,
                front_matter: None,
                body: text,
            });
        }
    }

    // No front matter
    Ok(Parsed {
        format: None,
        front_matter: None,
        body: text,
    })
}

/// Scan `rest` for a line that is exactly the fence, returning (front_matter, body).
fn take_until_fence<'a>(rest: &'a str, fence: &str) -> Option<(&'a str, &'a str)> {
    let mut idx = 0;
    for line in rest.split_inclusive(['\n']) {
        let trimmed = line.trim_end_matches('\n').trim_end_matches('\r');
        if trimmed == fence {
            let fm_end = idx;
            let body_start = idx + line.len();
            let fm = &rest[..fm_end];
            let body = &rest[body_start..];
            return Some((fm, body));
        }
        idx += line.len();
    }
    None
}

fn first_non_ws_is_lbrace(s: &str) -> bool {
    s.chars().skip_while(|c| c.is_whitespace()).next() == Some('{')
}

/// Grab a balanced top-level JSON object from the start (after leading WS).
/// Returns (json_object_str, body_after_object_starting_next_line_or_ws)
fn slice_top_level_json_object<'a>(s: &'a str) -> Option<(&'a str, &'a str)> {
    let start = s.find(|c: char| !c.is_whitespace())?;
    if s.as_bytes().get(start)? != &b'{' {
        return None;
    }
    let mut depth = 0usize;
    let mut i = start;
    let bytes = s.as_bytes();
    let mut in_str = false;
    let mut esc = false;

    while i < bytes.len() {
        let b = bytes[i];
        if in_str {
            if esc {
                esc = false;
            } else if b == b'\\' {
                esc = true;
            } else if b == b'"' {
                in_str = false;
            }
            i += 1;
            continue;
        }

        match b {
            b'"' => in_str = true,
            b'{' => depth += 1,
            b'}' => {
                if depth == 0 {
                    return None;
                }
                depth -= 1;
                if depth == 0 {
                    let json_str = &s[start..=i];
                    // Skip a single immediate newline (\n or \r\n) after the object
                    let mut j = i + 1;
                    if j < bytes.len() && (bytes[j] == b'\n' || bytes[j] == b'\r') {
                        if bytes[j] == b'\r' && j + 1 < bytes.len() && bytes[j + 1] == b'\n' {
                            j += 2;
                        } else {
                            j += 1;
                        }
                    }
                    let body = &s[j..];
                    return Some((json_str, body));
                }
            }
            _ => {}
        }
        i += 1;
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    struct InMem(&'static str);
    impl InMem {
        fn read_to_string(&self) -> Result<String> {
            Ok(self.0.to_string())
        }

        fn try_parse(&self) -> Result<Parsed> {
            Ok(parse_front_matter(self.read_to_string()?)?)
        }
    }

    // ---------- YAML (--- ... ---) ----------

    #[test]
    fn yaml_basic_ok() {
        let doc = InMem(
            "---\n\
             title: Hello\n\
             tags: [a, b]\n\
             count: 3\n\
             ---\n\
             # Body\n\
             Some content.\n",
        );
        let p = doc.try_parse().expect("parse");
        assert_eq!(p.format, Some(FrontFormat::Yaml));
        match p.front_matter {
            Some(FrontValue::Yaml(v)) => {
                let m = v.as_mapping().expect("yaml mapping");
                assert!(m.get(&yml::Value::from("title")).is_some());
            }
            _ => panic!("expected YAML"),
        }
        assert!(p.body.starts_with("# Body"));
    }

    #[test]
    fn yaml_crlf_ok() {
        let doc = InMem(
            "---\r\n\
             title: Hello\r\n\
             ---\r\n\
             Body with CRLF\r\n",
        );
        let p = doc.try_parse().expect("parse");
        assert_eq!(p.format, Some(FrontFormat::Yaml));
        assert!(p.body.starts_with("Body with CRLF"));
    }

    #[test]
    fn yaml_body_with_fence_inside_ok() {
        let doc = InMem(
            "---\n\
             title: Fence Test\n\
             ---\n\
             Line 1\n\
             ---\n\
             This '---' occurs in the body and must be preserved.\n",
        );
        let p = doc.try_parse().expect("parse");
        assert_eq!(p.format, Some(FrontFormat::Yaml));
        assert!(p.body.contains("This '---' occurs in the body"));
        assert!(p.body.matches("---").count() >= 1);
    }

    #[test]
    fn yaml_unterminated_fence_treated_as_body() {
        let doc = InMem(
            "---\n\
             title: Missing close fence\n\
             Still in the same block\n\
             Body text that follows\n",
        );
        let p = doc.try_parse().expect("parse");
        assert_eq!(p.format, None);
        assert!(p.front_matter.is_none());
        assert!(p.body.contains("Missing close fence"));
    }

    #[test]
    fn yaml_empty_block_ok() {
        let doc = InMem(
            "---\n\
             ---\n\
             body\n",
        );
        let p = doc.try_parse().expect("parse");
        assert_eq!(p.format, Some(FrontFormat::Yaml));
        match p.front_matter {
            Some(FrontValue::Yaml(v)) => assert!(v.is_null()),
            _ => panic!("expected YAML null"),
        }
        assert!(p.body.starts_with("body"));
    }

    // ---------- TOML (+++ ... +++) ----------

    #[test]
    fn toml_basic_ok() {
        let doc = InMem(
            "+++\n\
             title = 'Hi'\n\
             count = 7\n\
             tags = ['x','y']\n\
             +++\n\
             Body here\n",
        );
        let p = doc.try_parse().expect("parse");
        assert_eq!(p.format, Some(FrontFormat::Toml));
        match p.front_matter {
            Some(FrontValue::Toml(v)) => assert!(v.get("title").is_some()),
            _ => panic!("expected TOML"),
        }
        assert!(p.body.starts_with("Body here"));
    }

    #[test]
    fn toml_empty_block_ok() {
        let doc = InMem(
            "+++\n\
             +++\n\
             body\n",
        );
        let p = doc.try_parse().expect("parse");
        assert_eq!(p.format, Some(FrontFormat::Toml));
        match p.front_matter {
            Some(FrontValue::Toml(v)) => {
                assert!(v.as_table().map(|t| t.is_empty()).unwrap_or(false));
            }
            _ => panic!("expected TOML empty table"),
        }
    }

    #[test]
    fn toml_invalid_syntax_errors() {
        let doc = InMem(
            "+++\n\
             title = 'Hi\n\
             +++\n\
             body\n",
        );
        let err = doc.try_parse().unwrap_err();
        match err {
            _ => {}
        }
    }

    // ---------- JSON (unfenced, top-level object) ----------

    #[test]
    fn json_basic_ok() {
        let doc = InMem(
            "{\n  \"title\": \"Yo\",\n  \"draft\": true\n}\n\
             This is the body.\n",
        );
        let p = doc.try_parse().expect("parse");
        assert_eq!(p.format, Some(FrontFormat::Json));
        match p.front_matter {
            Some(FrontValue::Json(v)) => assert_eq!(v["draft"], json::Value::Bool(true)),
            _ => panic!("expected JSON"),
        }
        assert!(p.body.starts_with("This is the body."));
    }

    #[test]
    fn json_empty_object_ok() {
        let doc = InMem(
            "{ }\n\
             body\n",
        );
        let p = doc.try_parse().expect("parse");
        assert_eq!(p.format, Some(FrontFormat::Json));
        match p.front_matter {
            Some(FrontValue::Json(v)) => assert!(v.as_object().unwrap().is_empty()),
            _ => panic!("expected JSON"),
        }
        assert!(p.body.starts_with("body"));
    }

    #[test]
    fn json_invalid_front_matter_errors() {
        let doc = InMem(
            "{ not valid json }\n\
             Body\n",
        );
        let err = doc.try_parse().unwrap_err();
        match err {
            _ => {}
        }
    }

    // ---------- No front matter / heuristics ----------

    #[test]
    fn no_front_matter_all_body() {
        let doc = InMem("Just a body.\nWith multiple lines.\n");
        let p = doc.try_parse().expect("parse");
        assert_eq!(p.format, None);
        assert!(p.front_matter.is_none());
        assert_eq!(p.body, "Just a body.\nWith multiple lines.\n");
    }

    #[test]
    fn bom_before_yaml_is_ignored() {
        let doc = InMem(
            "\u{FEFF}---\n\
             title: With BOM\n\
             ---\n\
             body\n",
        );
        let p = doc.try_parse().expect("parse");
        assert_eq!(p.format, Some(FrontFormat::Yaml));
        assert!(p.body.starts_with("body"));
    }

    #[test]
    fn mixed_newlines_yaml_ok() {
        let doc = InMem(
            "---\r\n\
             title: crlf\r\n\
             ---\n\
             body\n",
        );
        let p = doc.try_parse().expect("parse");
        assert_eq!(p.format, Some(FrontFormat::Yaml));
        assert!(p.body.starts_with("body"));
    }

    #[test]
    fn detection_none_when_no_fence_and_not_json() {
        let doc = InMem("   \n  not-json-start\n---\n");
        let p = doc.try_parse().expect("parse");
        assert_eq!(p.format, None);
        assert!(p.front_matter.is_none());
    }

    #[test]
    fn body_with_braces_not_at_start_is_not_json_fm() {
        let doc = InMem(
            "This line starts the body.\n\
             { \"k\": 1 }\n",
        );
        let p = doc.try_parse().expect("parse");
        assert_eq!(p.format, None);
        assert!(p.front_matter.is_none());
        assert!(p.body.starts_with("This line starts the body."));
    }
}
