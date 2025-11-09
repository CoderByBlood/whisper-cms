//! Filename utilities (std-only LazyLock)
//!
//! - `split_filename`: split into (base, Option<extension>) using the **last dot** only
//! - `is_extension_in_set`: general case-insensitive membership check
//! - `is_textish_extension`: delegates to `is_extension_in_set` with a built-in set
//!
//! Requires Rust 1.70+ (for `std::sync::LazyLock`).
//!
//! Cargo.toml:
//! [dependencies]
//! regex = "1.11"

use regex::Regex;
use std::collections::HashSet;
use std::path::Path;
use std::sync::LazyLock;

/// De-facto “pure Rust convertible” content extensions we support.
/// Markdown, AsciiDoc, reStructuredText, Org, plus passthrough HTML/TXT.
pub const DEFAULT_CONTENT_EXTS: &[&str] = &[
    "md", "markdown", "mkd", "mkdn", // Markdown family
    "adoc", "asciidoc", // AsciiDoc
    "rst",      // reStructuredText
    "org",      // Org Mode
    "html", "htm", "xhtml", // HTML
    "txt", "text", // Plain text
];

/// Split a filename into `(base, Option<extension>)` using only the **last dot** as separator.
///
/// Behavior:
/// - "file.txt"           → ("file", Some("txt"))
/// - "archive.tar.gz"     → ("archive.tar", Some("gz"))
/// - "noext"              → ("noext", None)
/// - ".gitignore"         → (".gitignore", None)
/// - "report."            → ("report", None)
pub fn split_filename(name: &str) -> (String, Option<String>) {
    static RE_LAST_DOT: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"^(?P<base>.*?)(?:\.(?P<ext>[^./\\]+))?$").unwrap());

    if name.is_empty() {
        return (String::new(), None);
    }

    // Hidden file with no further dots (e.g., ".gitignore", ".env") → no extension.
    if name.starts_with('.') && !name[1..].contains('.') {
        return (name.to_string(), None);
    }

    match RE_LAST_DOT.captures(name) {
        Some(caps) => {
            let base = caps
                .name("base")
                .map(|m| m.as_str())
                .unwrap_or("")
                .to_string();
            let ext = caps.name("ext").map(|m| m.as_str().to_string());
            // Treat empty ext (e.g., trailing dot) as None.
            let ext = match ext {
                Some(e) if e.is_empty() => None,
                other => other,
            };
            (base, ext)
        }
        None => (name.to_string(), None),
    }
}

/// General, case-insensitive membership test for extensions.
/// Accepts with or without leading dot (".md" or "md").
pub fn is_extension_in_set(ext: &str, allowed: &HashSet<&str>) -> bool {
    if ext.is_empty() {
        return false;
    }
    let key = ext.strip_prefix('.').unwrap_or(ext).to_ascii_lowercase();
    allowed.contains(key.as_str())
}

/// Text/markup extension check (delegates to `is_extension_in_set`).
pub fn is_textish_extension(ext: &str) -> bool {
    static TEXTISH: LazyLock<HashSet<&'static str>> =
        LazyLock::new(|| DEFAULT_CONTENT_EXTS.iter().copied().collect());

    is_extension_in_set(ext, &TEXTISH)
}

/// If the given path points to a “textish” file, return `(filename, base, ext)`.
///
/// - `filename`: the leaf name (no directories)
/// - `base`: the part before the last `.`, as in `split_filename`
/// - `ext`: the confirmed textish extension (lowercased)
///
/// Returns `None` if:
/// - the path has no filename component
/// - or the extension is not considered textish
///
/// # Examples
/// ```
/// use std::path::Path;
/// // Import from this crate so the doctest can resolve the symbol.
/// use serve::filter::analyze_textish_file;
///
/// assert_eq!(
///     analyze_textish_file(Path::new("/tmp/readme.md")),
///     Some(("readme.md".into(), "readme".into(), "md".into()))
/// );
///
/// assert_eq!(
///     analyze_textish_file(Path::new("images/logo.png")),
///     None
/// );
/// ```
pub fn analyze_textish_file(path: impl AsRef<Path>) -> Option<(String, String, String)> {
    // 1. Get the filename (no directories)
    let filename_os = path.as_ref().file_name()?;
    let filename = filename_os.to_string_lossy();

    // 2. Split into base and extension
    let (base, ext_opt) = split_filename(&filename);

    // 3. If the extension is textish, return all parts
    if let Some(ext) = ext_opt {
        if is_textish_extension(&ext) {
            // Normalize extension to lowercase for consistency
            return Some((filename.to_string(), base, ext.to_ascii_lowercase()));
        }
    }

    None
}

/// Build a single case-insensitive regex that matches **filenames only**
/// with any of the provided extensions (leading dot optional).
pub fn textish_filename_regex() -> Result<Regex, regex::Error> {
    build_filename_regex(DEFAULT_CONTENT_EXTS)
}

fn build_filename_regex<I, S>(extensions: I) -> Result<Regex, regex::Error>
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    use std::collections::BTreeSet;

    let mut set: BTreeSet<String> = BTreeSet::new();
    for ext in extensions {
        let raw = ext.as_ref().trim();
        if raw.is_empty() {
            continue;
        }
        let trimmed = raw.strip_prefix('.').unwrap_or(raw);
        if trimmed.is_empty() {
            continue;
        }
        set.insert(trimmed.to_ascii_lowercase());
    }

    if set.is_empty() {
        return Err(regex::Error::Syntax("Empty extension set".to_string()));
    }

    let alts = set
        .into_iter()
        .map(|e| regex::escape(&e))
        .collect::<Vec<_>>()
        .join("|");

    // Basename only: no path separators; requires a dot + one of the extensions.
    let pattern = format!(r"(?i)^[^/\\]+\.({})$", alts);
    Regex::new(&pattern)
}
