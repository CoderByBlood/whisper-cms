use crate::fs::scan::{scan_folder_with_filters, File};
use adapt::{
    filter::SourceFinder,
    fm::{parse_front_matter, ContentSource, FrontMatterError},
};
use std::sync::Arc;

pub struct FileContentSource {
    file: Arc<File>,
}

impl FileContentSource {
    pub fn new(file: Arc<File>) -> Self {
        Self { file }
    }
}

impl ContentSource for FileContentSource {
    fn read_to_string(&self) -> Result<String, FrontMatterError> {
        Ok(self.file.read_string()?)
    }

    fn try_parse(&self) -> Result<adapt::fm::Parsed, FrontMatterError> {
        Ok(parse_front_matter(self)?)
    }
}

pub struct ScannedFolderSourceFinder;

impl SourceFinder for ScannedFolderSourceFinder {
    fn collect<P: AsRef<std::path::Path>>(
        &self,
        root: P,
        name_filter: Option<&regex::Regex>,
    ) -> Result<Vec<Box<dyn ContentSource>>, Box<dyn std::error::Error + Send + Sync>> {
        Ok(scan_folder_with_filters(root, None, name_filter)?
            .files()
            .iter()
            .map(|f| Box::new(FileContentSource::new(f.clone())) as Box<dyn ContentSource>)
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fs::scan::scan_folder;
    use std::fs;
    use std::io::Write;
    use std::path::Path;
    use tempfile::tempdir;

    /// Small helper to write text to a file (creates parents).
    fn write_text(path: &Path, s: &str) -> std::io::Result<()> {
        if let Some(p) = path.parent() {
            fs::create_dir_all(p)?;
        }
        let mut f = fs::File::create(path)?;
        write!(f, "{}", s)?;
        Ok(())
    }

    fn fcs_for(store_root: &Path, rel: &str) -> FileContentSource {
        let store = scan_folder(store_root).expect("scan_folder");
        let file = store
            .get_by_relative(rel)
            .unwrap_or_else(|| panic!("missing indexed file: {rel}"));
        FileContentSource { file }
    }

    #[test]
    fn parse_yaml_front_matter() {
        let dir = tempdir().unwrap();
        let root = dir.path();

        let rel = "posts/y1.md";
        write_text(
            &root.join(rel),
            "---\n\
             title: Hello\n\
             tags: [a, b]\n\
             count: 3\n\
             ---\n\
             # Body\n\
             Some content.\n",
        )
        .unwrap();

        let src = fcs_for(root, rel);
        let parsed = src.try_parse().expect("try_parse");

        assert_eq!(parsed.format, Some(adapt::fm::FrontFormat::Yaml));
        assert!(parsed.front_matter.is_some());
        assert!(parsed.body.starts_with("# Body"));
    }

    #[test]
    fn parse_yaml_crlf_and_bom() {
        let dir = tempdir().unwrap();
        let root = dir.path();

        let rel = "posts/y2.md";
        write_text(
            &root.join(rel),
            "\u{FEFF}---\r\n\
             title: With BOM + CRLF\r\n\
             ---\r\n\
             Body\r\n",
        )
        .unwrap();

        let src = fcs_for(root, rel);
        let parsed = src.try_parse().expect("try_parse");
        assert_eq!(parsed.format, Some(adapt::fm::FrontFormat::Yaml));
        assert!(parsed.body.starts_with("Body"));
    }

    #[test]
    fn parse_toml_front_matter() {
        let dir = tempdir().unwrap();
        let root = dir.path();

        let rel = "posts/t1.md";
        write_text(
            &root.join(rel),
            "+++\n\
             title = 'Hi'\n\
             count = 7\n\
             tags = ['x','y']\n\
             +++\n\
             Body here\n",
        )
        .unwrap();

        let src = fcs_for(root, rel);
        let parsed = src.try_parse().expect("try_parse");

        assert_eq!(parsed.format, Some(adapt::fm::FrontFormat::Toml));
        assert!(parsed.front_matter.is_some());
        assert!(parsed.body.starts_with("Body here"));
    }

    #[test]
    fn parse_json_front_matter_unfenced() {
        let dir = tempdir().unwrap();
        let root = dir.path();

        let rel = "posts/j1.md";
        write_text(
            &root.join(rel),
            "{\n  \"title\": \"Yo\",\n  \"draft\": true\n}\n\
             This is the body.\n",
        )
        .unwrap();

        let src = fcs_for(root, rel);
        let parsed = src.try_parse().expect("try_parse");

        assert_eq!(parsed.format, Some(adapt::fm::FrontFormat::Json));
        assert!(parsed.front_matter.is_some());
        assert!(parsed.body.starts_with("This is the body."));
    }

    #[test]
    fn no_front_matter_entire_file_is_body() {
        let dir = tempdir().unwrap();
        let root = dir.path();

        let rel = "posts/none.txt";
        write_text(&root.join(rel), "Just a body.\nWith multiple lines.\n").unwrap();

        let src = fcs_for(root, rel);
        let parsed = src.try_parse().expect("try_parse");

        assert_eq!(parsed.format, None);
        assert!(parsed.front_matter.is_none());
        assert_eq!(parsed.body, "Just a body.\nWith multiple lines.\n");
    }

    #[test]
    fn yaml_unterminated_fence_is_treated_as_body() {
        let dir = tempdir().unwrap();
        let root = dir.path();

        let rel = "posts/y_unterminated.md";
        write_text(
            &root.join(rel),
            "---\n\
             title: Missing close fence\n\
             Still in the same block\n\
             Body text that follows\n",
        )
        .unwrap();

        let src = fcs_for(root, rel);
        let parsed = src.try_parse().expect("try_parse");

        // Under our contract, unterminated → no FM; entire file is body.
        assert_eq!(parsed.format, None);
        assert!(parsed.front_matter.is_none());
        assert!(parsed.body.contains("Missing close fence"));
    }

    #[test]
    fn invalid_toml_reports_error() {
        let dir = tempdir().unwrap();
        let root = dir.path();

        let rel = "posts/t_bad.md";
        write_text(
            &root.join(rel),
            "+++\n\
             title = 'Hi\n\
             +++\n\
             body\n",
        )
        .unwrap();

        let src = fcs_for(root, rel);
        let err = src.try_parse().unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("TOML front matter parse error"),
            "unexpected error: {msg}"
        );
    }

    #[test]
    fn invalid_json_reports_error() {
        let dir = tempdir().unwrap();
        let root = dir.path();

        let rel = "posts/j_bad.md";
        write_text(
            &root.join(rel),
            "{ not valid json }\n\
             Body\n",
        )
        .unwrap();

        let src = fcs_for(root, rel);
        let err = src.try_parse().unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("JSON front matter parse error"),
            "unexpected error: {msg}"
        );
    }

    #[test]
    fn yaml_body_may_contain_fence_text() {
        let dir = tempdir().unwrap();
        let root = dir.path();

        let rel = "posts/y_body_fence.md";
        write_text(
            &root.join(rel),
            "---\n\
             title: Fence Test\n\
             ---\n\
             Line 1\n\
             ---\n\
             This '---' occurs in the body and must be preserved.\n",
        )
        .unwrap();

        let src = fcs_for(root, rel);
        let parsed = src.try_parse().expect("try_parse");

        assert_eq!(parsed.format, Some(adapt::fm::FrontFormat::Yaml));
        assert!(parsed.body.contains("This '---' occurs in the body"));
        assert!(parsed.body.matches("---").count() >= 1);
    }

    #[test]
    fn json_empty_object_ok() {
        let dir = tempdir().unwrap();
        let root = dir.path();

        let rel = "posts/j_empty.md";
        write_text(
            &root.join(rel),
            "{ }\n\
             body\n",
        )
        .unwrap();

        let src = fcs_for(root, rel);
        let parsed = src.try_parse().expect("try_parse");

        assert_eq!(parsed.format, Some(adapt::fm::FrontFormat::Json));
        assert!(parsed.front_matter.is_some());
        assert!(parsed.body.starts_with("body"));
    }

    #[test]
    fn braces_in_body_do_not_trigger_json_front_matter() {
        let dir = tempdir().unwrap();
        let root = dir.path();

        let rel = "posts/braces_in_body.md";
        write_text(
            &root.join(rel),
            "This line starts the body.\n\
             { \"k\": 1 }\n",
        )
        .unwrap();

        let src = fcs_for(root, rel);
        let parsed = src.try_parse().expect("try_parse");

        assert_eq!(parsed.format, None);
        assert!(parsed.front_matter.is_none());
        assert!(parsed.body.starts_with("This line starts the body."));
    }
}
