// src/source_filter.rs
use crate::fm::ContentSource;
use regex::Regex;
use snafu::{ResultExt, Snafu};
use std::error::Error as StdError;
use std::path::Path;

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

#[derive(Debug, Snafu)]
pub enum BuildError {
    #[snafu(display("No valid extensions were provided"))]
    Empty,

    #[snafu(display("Failed to compile filename regex: {source}"))]
    Regex { source: regex::Error },
}

#[derive(Debug, Snafu)]
pub enum FindError {
    #[snafu(display("Could not build filename regex: {source}"))]
    Build { source: BuildError },

    #[snafu(display("Could not collect sources: {source}"))]
    Collect {
        source: Box<dyn StdError + Send + Sync>,
    },
}

/// A trait for locating “content sources” within a directory tree.
///
/// Implementors should walk `root`, apply the optional **filename** regex
/// to *basenames* (for filtering supported content formats),
/// and return all matching files as `ContentSource` objects.
pub trait SourceFinder {
    fn collect<P: AsRef<Path>>(
        &self,
        root: P,
        name_filter: Option<&Regex>,
    ) -> Result<Vec<Box<dyn ContentSource>>, Box<dyn StdError + Send + Sync>>;
}

/// Build a single case-insensitive regex that matches **filenames only**
/// with any of the provided extensions (leading dot optional).
pub fn build_filename_regex<I, S>(extensions: I) -> Result<Regex, BuildError>
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
        return Err(BuildError::Empty);
    }

    let alts = set
        .into_iter()
        .map(|e| regex::escape(&e))
        .collect::<Vec<_>>()
        .join("|");

    // Basename only: no path separators; requires a dot + one of the extensions.
    let pattern = format!(r"(?i)^[^/\\]+\.({})$", alts);
    Regex::new(&pattern).context(RegexSnafu)
}

/// Module-level helper:
/// 1) builds the default filename regex from `DEFAULT_CONTENT_EXTS`
/// 2) delegates to the provided `SourceFinder::collect`
pub fn find_sources(
    finder: &impl SourceFinder,
    root: &Path,
) -> Result<Vec<Box<dyn ContentSource>>, FindError> {
    let re = build_filename_regex(DEFAULT_CONTENT_EXTS).context(BuildSnafu)?;
    finder
        .collect(root, Some(&re))
        .map_err(|e| FindError::Collect { source: e })
}

#[cfg(test)]
mod tests {
    use super::*;
    use mockall::mock;
    use regex::Regex;
    use std::error::Error as StdError;
    use std::path::{Path, PathBuf};
    use tempfile::tempdir;

    // Bring in-crate FM for the DummySource implementation.
    use crate::fm;

    // -------------------------
    // Minimal ContentSource used by the mock return values
    // -------------------------
    struct DummySource(String);

    impl fm::ContentSource for DummySource {
        fn read_to_string(&self) -> Result<String, Box<dyn StdError + Send + Sync>> {
            Ok(self.0.clone())
        }

        fn try_parse(&self) -> Result<fm::Parsed, Box<dyn StdError + Send + Sync>> {
            Ok(fm::parse_front_matter(self)?)
        }
    }

    // -------------------------
    // Mock a *monomorphic* shim: collect_ref(&Path, Option<&Regex>)
    // Then implement the generic trait for MockFinder using that shim.
    // -------------------------
    mock! {
        pub Finder {
            pub fn collect_ref<'a>(
                &self,
                root: &'a Path,
                name_filter: Option<&'a Regex>,
            ) -> Result<Vec<Box<dyn fm::ContentSource>>, Box<dyn StdError + Send + Sync>>;
        }
    }

    impl super::SourceFinder for MockFinder {
        fn collect<P: AsRef<Path>>(
            &self,
            root: P,
            name_filter: Option<&Regex>,
        ) -> Result<Vec<Box<dyn ContentSource>>, Box<dyn StdError + Send + Sync>> {
            // Forward to the shim with a &Path
            self.collect_ref(root.as_ref(), name_filter)
                // Type erasure: fm::ContentSource == ContentSource in this crate
                .map(|v| v.into_iter().map(|b| b as Box<dyn ContentSource>).collect())
        }
    }

    // -------------------------
    // build_filename_regex tests
    // -------------------------

    #[test]
    fn build_regex_matches_known_extensions_and_is_basename_only() {
        let re = build_filename_regex([".md", "markdown", "ORG", "mkd"]).expect("regex");
        // Positive
        assert!(re.is_match("README.md"));
        assert!(re.is_match("post.markdown"));
        assert!(re.is_match("notes.MKD"));
        assert!(re.is_match("plan.ORG"));
        // Negative
        assert!(!re.is_match("image.png"));
        assert!(!re.is_match("noext"));
        assert!(!re.is_match("dir/file.md"));
        assert!(!re.is_match(r"dir\file.md"));
    }

    #[test]
    fn build_regex_dedup_and_casefold() {
        let re = build_filename_regex(["MD", "md", ".Md", "mD"]).unwrap();
        assert!(re.is_match("x.MD"));
        assert!(re.is_match("x.md"));
        assert!(re.is_match("x.mD"));
    }

    #[test]
    fn build_regex_empty_input_errors() {
        let err = build_filename_regex(["", "   ", "."]).unwrap_err();
        match err {
            BuildError::Empty => {}
            other => panic!("expected BuildError::Empty, got {other:?}"),
        }
    }

    // -------------------------
    // find_sources tests (with mock)
    // -------------------------

    #[test]
    fn find_sources_delegates_to_finder_with_default_regex_and_root() {
        let tmp = tempdir().unwrap();
        let root = tmp.path().to_path_buf();

        // Build sample names dynamically from DEFAULT_CONTENT_EXTS
        let mut samples: Vec<String> = DEFAULT_CONTENT_EXTS
            .iter()
            .map(|ext| format!("sample.{ext}"))
            .collect();

        // Also include mixed-case to confirm (?i)
        if let Some(first) = DEFAULT_CONTENT_EXTS.get(0) {
            samples.push(format!("UPPER.{}", first.to_uppercase()));
        }

        let root_for_expect = root.clone();
        let samples_for_expect = samples.clone();

        let mut mock = MockFinder::new();
        mock.expect_collect_ref()
            // withf receives &Path and &Option<&Regex>
            .withf(move |given_root: &Path, re_opt: &Option<&Regex>| {
                if PathBuf::from(given_root) != root_for_expect {
                    return false;
                }
                let re = match re_opt {
                    Some(r) => *r,
                    None => return false, // we expect Some(regex)
                };

                // Regex must match all sample basenames
                if samples_for_expect.iter().any(|name| !re.is_match(name)) {
                    return false;
                }

                // Basename-only: must NOT match with path separators
                let check_ext = DEFAULT_CONTENT_EXTS.first().cloned().unwrap_or("md");
                let with_slash = format!("dir/file.{check_ext}");
                let with_bslash = format!(r"dir\file.{check_ext}");
                if re.is_match(&with_slash) || re.is_match(&with_bslash) {
                    return false;
                }

                true
            })
            .times(1)
            .returning(|_, _| {
                Ok(vec![
                    Box::new(DummySource("# one".into())) as Box<dyn fm::ContentSource>,
                    Box::new(DummySource("* two".into())) as Box<dyn fm::ContentSource>,
                ])
            });

        let sources = find_sources(&mock, &root).expect("find_sources");
        assert_eq!(sources.len(), 2);
        assert_eq!(sources[0].read_to_string().unwrap(), "# one");
        assert_eq!(sources[1].read_to_string().unwrap(), "* two");
    }

    #[test]
    fn find_sources_bubbles_collect_error() {
        let tmp = tempdir().unwrap();
        let root = tmp.path().to_path_buf();

        let mut mock = MockFinder::new();
        mock.expect_collect_ref()
            // Return a boxed std error (matches trait)
            .returning(|_, _| {
                Err::<_, Box<dyn StdError + Send + Sync>>(Box::new(std::io::Error::new(
                    std::io::ErrorKind::Other,
                    "boom",
                )))
            });

        // Avoid unwrap_err() so we don't need Debug on the Ok type
        let err = match find_sources(&mock, &root) {
            Err(e) => e,
            Ok(_) => panic!("expected error from find_sources, got Ok"),
        };

        match err {
            FindError::Collect { source } => {
                assert!(
                    source.to_string().contains("boom"),
                    "unexpected error: {source:?}"
                );
            }
            other => panic!("expected FindError::Collect, got {other:?}"),
        }
    }

    #[test]
    fn default_regex_covers_all_configured_exts() {
        let re = build_filename_regex(DEFAULT_CONTENT_EXTS).expect("regex");
        let samples = [
            "a.md",
            "b.markdown",
            "c.mkd",
            "d.mkdn", // Markdown
            "e.adoc",
            "f.asciidoc", // AsciiDoc
            "g.rst",      // reST
            "h.org",      // Org
            "i.html",
            "j.htm",
            "k.xhtml", // HTML
            "l.txt",
            "m.text", // Text
        ];
        for s in samples {
            assert!(re.is_match(s), "regex should match {s}");
        }
    }
}
