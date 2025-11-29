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

pub fn build_filename_regex<I, S>(extensions: I) -> Result<Regex, regex::Error>
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::{BTreeSet, HashSet};
    use std::path::Path;

    // ---------- split_filename ----------

    #[test]
    fn split_filename_basic_cases() {
        assert_eq!(
            split_filename("file.txt"),
            ("file".into(), Some("txt".into()))
        );
        assert_eq!(
            split_filename("archive.tar.gz"),
            ("archive.tar".into(), Some("gz".into()))
        );
        assert_eq!(split_filename("noext"), ("noext".into(), None));
        assert_eq!(split_filename(".gitignore"), (".gitignore".into(), None));
        // Trailing dot means "no extension", dot remains part of the base
        assert_eq!(split_filename("report."), ("report.".into(), None));
        assert_eq!(split_filename(""), ("".into(), None));
    }

    #[test]
    fn split_filename_handles_dots_and_hidden_files() {
        assert_eq!(split_filename("a.b.c"), ("a.b".into(), Some("c".into())));
        assert_eq!(split_filename("a..b"), ("a.".into(), Some("b".into())));
        assert_eq!(
            split_filename(".env.local"),
            (".env".into(), Some("local".into()))
        );
        assert_eq!(split_filename(".hidden"), (".hidden".into(), None));
    }

    #[test]
    fn split_filename_round_trip_property() {
        // The base/ext pair must reconstruct the original filename
        fn reconstruct(base: &str, ext: &Option<String>) -> String {
            match ext {
                Some(e) => format!("{base}.{e}"),
                None => base.to_string(),
            }
        }

        let names = [
            "file.txt",
            "archive.tar.gz",
            "noext",
            ".gitignore",
            "report.",
        ];
        for name in names {
            let (base, ext) = split_filename(name);
            let recomposed = reconstruct(&base, &ext);
            assert_eq!(
                recomposed, name,
                "split_filename should allow perfect reconstruction for {name}"
            );
        }
    }

    // ---------- is_extension_in_set ----------

    #[test]
    fn is_extension_in_set_case_insensitive_and_dot_tolerant() {
        let allowed: HashSet<&str> = ["md", "txt", "html"].into_iter().collect();

        assert!(is_extension_in_set("md", &allowed));
        assert!(is_extension_in_set(".md", &allowed));
        assert!(is_extension_in_set("MD", &allowed));
        assert!(is_extension_in_set(".HTML", &allowed));

        assert!(!is_extension_in_set("", &allowed));
        assert!(!is_extension_in_set(".unknown", &allowed));
        assert!(!is_extension_in_set("jpg", &allowed));
    }

    // ---------- is_textish_extension ----------

    #[test]
    fn textish_extension_matches_default_list() {
        for ext in DEFAULT_CONTENT_EXTS {
            assert!(
                is_textish_extension(ext),
                "expected {ext} to be recognized as textish"
            );
            // Uppercase and dotted variants
            assert!(is_textish_extension(&ext.to_ascii_uppercase()));
            assert!(is_textish_extension(&format!(".{ext}")));
        }

        // Some definitely non-textish extensions
        for ext in ["png", "jpg", "gif", "pdf", "zip"] {
            assert!(!is_textish_extension(ext));
        }
    }

    // ---------- analyze_textish_file ----------

    #[test]
    fn analyze_textish_file_positive_and_normalizes_extension() {
        let got = analyze_textish_file(Path::new("README.MD"));
        assert_eq!(
            got,
            Some(("README.MD".into(), "README".into(), "md".into()))
        );

        let got2 = analyze_textish_file(Path::new("/tmp/post.markdown"));
        assert_eq!(
            got2,
            Some(("post.markdown".into(), "post".into(), "markdown".into()))
        );
    }

    #[test]
    fn analyze_textish_file_negative_cases() {
        assert_eq!(analyze_textish_file(Path::new("image.png")), None);
        assert_eq!(analyze_textish_file(Path::new("LICENSE")), None);
        assert_eq!(analyze_textish_file(Path::new(".gitignore")), None);
        assert_eq!(analyze_textish_file(Path::new("notes.")), None);
    }

    // ---------- textish_filename_regex / build_filename_regex ----------

    #[test]
    fn textish_filename_regex_matches_basename_only() {
        let re = textish_filename_regex().expect("regex build failed");

        // Positive
        assert!(re.is_match("readme.md"));
        assert!(re.is_match("post.MarkDown"));
        assert!(re.is_match("index.HTML"));
        assert!(re.is_match("notes.txt"));
        assert!(re.is_match("multi.dot.name.rst"));

        // Negative: no extension
        assert!(!re.is_match("README"));
        // Hidden file without further dots
        assert!(!re.is_match(".gitignore"));
        // Non-textish extension
        assert!(!re.is_match("image.png"));
        // Path separators
        assert!(!re.is_match("docs/readme.md"));
        assert!(!re.is_match(r"docs\readme.md"));
        // Trailing dot
        assert!(!re.is_match("notes."));
    }

    #[test]
    fn build_filename_regex_allows_custom_sets_and_is_case_insensitive() {
        let re = build_filename_regex(["PNG", "JpG"].into_iter()).expect("regex build failed");

        assert!(re.is_match("a.png"));
        assert!(re.is_match("a.PNG"));
        assert!(re.is_match("a.jpg"));
        assert!(re.is_match("a.JpG"));

        assert!(!re.is_match("a.gif"));
        assert!(!re.is_match("a"));
        assert!(!re.is_match("dir/a.jpg"));
        assert!(!re.is_match(r"dir\a.jpg"));
    }

    #[test]
    fn build_filename_regex_rejects_empty_or_invalid_sets() {
        // Empty iterator → syntax error
        let err = build_filename_regex(std::iter::empty::<&str>()).unwrap_err();
        match err {
            regex::Error::Syntax(s) => assert!(s.to_ascii_lowercase().contains("empty")),
            other => panic!("unexpected error: {other:?}"),
        }

        // Set with only empty or dot entries → error
        let err2 = build_filename_regex(["", ".", " . "]).unwrap_err();
        match err2 {
            regex::Error::Syntax(s) => assert!(s.to_ascii_lowercase().contains("empty")),
            other => panic!("unexpected error: {other:?}"),
        }
    }

    #[test]
    fn default_content_exts_are_unique_and_non_empty() {
        let mut seen = BTreeSet::new();
        for e in DEFAULT_CONTENT_EXTS {
            let trimmed = e.trim().trim_start_matches('.');
            assert!(!trimmed.is_empty(), "extension should not be empty");
            let lc = trimmed.to_ascii_lowercase();
            assert!(
                seen.insert(lc),
                "duplicate extension after normalization: {e}"
            );
        }
    }
}
