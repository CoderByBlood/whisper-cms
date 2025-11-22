use super::error::RenderError;
use regex::Regex;
use std::io::{Result as IoResult, Write};

/// One compiled regex patch.
#[derive(Debug)]
struct RegexPatch {
    re: Regex,
    replacement: String,
}

/// Streaming writer that applies a sequence of regex replacements
/// to UTF-8 text as it flows through.
///
/// This is designed for body text (HTML or JSON) and assumes UTF-8.
/// It keeps a small tail buffer to reduce the chance of splitting
/// matches across boundaries. Multi-line regex is allowed but not
/// guaranteed to work across chunk boundaries.
pub struct BodyRegexWriter<W: Write> {
    inner: W,
    patches: Vec<RegexPatch>,
    buffer: String,
    tail_len: usize,
}

impl<W: Write> BodyRegexWriter<W> {
    /// Create a new BodyRegexWriter with given compiled regexes.
    pub fn new(inner: W, patches: Vec<(Regex, String)>, tail_len: usize) -> Self {
        let patches = patches
            .into_iter()
            .map(|(re, replacement)| RegexPatch { re, replacement })
            .collect();

        Self {
            inner,
            patches,
            buffer: String::new(),
            tail_len,
        }
    }

    /// Process everything currently in the buffer, writing all of it out.
    pub fn finish(mut self) -> Result<W, RenderError> {
        let output = self.apply_patches(&self.buffer);
        self.inner
            .write_all(output.as_bytes())
            .map_err(RenderError::Io)?;
        self.buffer.clear();
        Ok(self.inner)
    }

    fn apply_patches(&self, text: &str) -> String {
        let mut s = text.to_owned();
        for patch in &self.patches {
            s = patch
                .re
                .replace_all(&s, patch.replacement.as_str())
                .into_owned();
        }
        s
    }

    fn flush_safe_prefix(&mut self) -> IoResult<()> {
        if self.buffer.len() <= self.tail_len {
            return Ok(());
        }

        let split_at = self.buffer.len() - self.tail_len;
        let safe_prefix = self.buffer[..split_at].to_string();
        let tail = self.buffer[split_at..].to_string();

        let transformed = self.apply_patches(&safe_prefix);
        self.inner.write_all(transformed.as_bytes())?;
        self.buffer = tail;
        Ok(())
    }
}

impl<W: Write> Write for BodyRegexWriter<W> {
    fn write(&mut self, buf: &[u8]) -> IoResult<usize> {
        // Assume UTF-8; panic on invalid UTF-8 is acceptable given encoding constraint.
        let s = std::str::from_utf8(buf).expect("BodyRegexWriter expects UTF-8 input");
        self.buffer.push_str(s);

        // Flush all but a small tail.
        self.flush_safe_prefix()?;

        Ok(buf.len())
    }

    fn flush(&mut self) -> IoResult<()> {
        self.inner.flush()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use regex::Regex;
    use std::io::{self, Write};

    // Simple helper to run a body through BodyRegexWriter in one go.
    fn run_with_body_regex(input: &str, patches: Vec<(Regex, String)>, tail_len: usize) -> String {
        let inner: Vec<u8> = Vec::new();
        let mut writer = BodyRegexWriter::new(inner, patches, tail_len);

        // Write the input through the streaming writer.
        writer
            .write_all(input.as_bytes())
            .expect("write should succeed");

        let inner = writer.finish().expect("finish should succeed");
        String::from_utf8(inner).expect("utf8 output")
    }

    #[test]
    fn single_patch_entire_string_replaced() {
        let patches = vec![(Regex::new("foo").unwrap(), "bar".to_string())];
        let inner: Vec<u8> = Vec::new();
        let mut writer = BodyRegexWriter::new(inner, patches, 4); // tail_len >= 3

        writer.write_all(b"foo ").unwrap();
        writer.write_all(b"foo").unwrap();

        let inner = writer.finish().expect("finish should succeed");
        let output = String::from_utf8(inner).unwrap();
        assert_eq!(output, "bar bar");
    }

    #[test]
    fn multiple_patches_applied_in_order() {
        // First replace digits with "#", then "#" with "!".
        let patches = vec![
            (Regex::new(r"\d+").unwrap(), "#".to_string()),
            (Regex::new(r"#").unwrap(), "!".to_string()),
        ];
        let result = run_with_body_regex("abc123xyz", patches, 16);
        assert_eq!(result, "abc!xyz");
    }

    #[test]
    fn no_patches_yields_original_content() {
        let patches: Vec<(Regex, String)> = Vec::new();
        let mut inner: Vec<u8> = Vec::new();
        let mut writer = BodyRegexWriter::new(inner, patches, 4);

        writer.write_all(b"Hello, world!").unwrap();
        inner = writer.finish().expect("finish should succeed");
        let output = String::from_utf8(inner).unwrap();
        assert_eq!(output, "Hello, world!");
    }

    #[test]
    fn cross_chunk_match_succeeds_when_tail_len_is_large_enough() {
        // "World" spans chunks; tail_len is >= pattern length.
        let patches = vec![(Regex::new("World").unwrap(), "Earth".to_string())];

        let inner: Vec<u8> = Vec::new();
        let mut writer = BodyRegexWriter::new(inner, patches, 5);

        writer.write_all(b"HelloWo").unwrap();
        writer.write_all(b"rld").unwrap();

        let inner = writer.finish().expect("finish should succeed");
        let output = String::from_utf8(inner).unwrap();
        assert_eq!(output, "HelloEarth");
    }

    #[test]
    fn tail_len_larger_than_buffer_defers_writes_until_finish() {
        let patches = vec![(Regex::new("x").unwrap(), "y".to_string())];
        let inner: Vec<u8> = Vec::new();
        let mut writer = BodyRegexWriter::new(inner, patches, 1024);

        writer.write_all(b"abcxyz").unwrap();
        // buffer length < tail_len, so nothing flushed yet

        let inner = writer.finish().expect("finish should succeed");
        let output = String::from_utf8(inner).unwrap();
        assert_eq!(output, "abcyyz"); // two 'x' -> 'y'
    }

    #[test]
    fn finish_writes_remaining_buffer_only_once() {
        let patches = vec![(Regex::new("a").unwrap(), "b".to_string())];
        let inner: Vec<u8> = Vec::new();
        let mut writer = BodyRegexWriter::new(inner, patches, 10);

        writer.write_all(b"aaa").unwrap();
        // Nothing flushed yet (buffer len 3 < tail 10)

        let inner = writer.finish().expect("finish should succeed");
        let output = String::from_utf8(inner).unwrap();
        assert_eq!(output, "bbb");
    }

    struct CountingWriter {
        pub bytes: Vec<u8>,
        pub flushes: usize,
    }

    impl Write for CountingWriter {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            self.bytes.extend_from_slice(buf);
            Ok(buf.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            self.flushes += 1;
            Ok(())
        }
    }

    #[test]
    fn flush_does_not_force_buffer_out() {
        let patches: Vec<(Regex, String)> = Vec::new();
        let inner = CountingWriter {
            bytes: Vec::new(),
            flushes: 0,
        };
        let mut writer = BodyRegexWriter::new(inner, patches, 100);

        writer.write_all(b"hello").unwrap();
        // buffer len 5 < tail_len 100 -> no write to inner yet

        writer.flush().unwrap();

        let inner = writer.finish().expect("finish should succeed");
        let output = String::from_utf8(inner.bytes.clone()).unwrap();

        // flush only flushed the inner writer, not the buffered content
        // (which is only written on finish).
        assert_eq!(inner.flushes, 1);
        assert_eq!(output, "hello");
    }

    struct FailingWriterOnWrite;

    impl Write for FailingWriterOnWrite {
        fn write(&mut self, _buf: &[u8]) -> io::Result<usize> {
            Err(io::Error::new(io::ErrorKind::Other, "boom"))
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn write_propagates_io_error_from_inner_writer() {
        let patches = vec![(Regex::new("a").unwrap(), "b".to_string())];
        // tail_len = 0 to force flush on every write
        let mut writer = BodyRegexWriter::new(FailingWriterOnWrite, patches, 0);

        let res = writer.write(b"abc");
        assert!(res.is_err());
    }

    #[derive(Debug)]
    struct FailingWriterOnFinish;

    impl Write for FailingWriterOnFinish {
        fn write(&mut self, _buf: &[u8]) -> io::Result<usize> {
            Err(io::Error::new(io::ErrorKind::Other, "boom on write"))
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn finish_propagates_io_error_as_render_error() {
        let patches = vec![(Regex::new("a").unwrap(), "b".to_string())];
        // buffer smaller than tail_len so nothing written before finish
        let inner = FailingWriterOnFinish;
        let mut writer = BodyRegexWriter::new(inner, patches, 10);

        writer.write_all(b"aaa").unwrap();

        let res = writer.finish();
        match res {
            Err(RenderError::Io(e)) => {
                assert_eq!(e.kind(), io::ErrorKind::Other);
            }
            other => panic!("finish should surface inner write error, got {:?}", other),
        }
    }

    #[test]
    #[should_panic(expected = "BodyRegexWriter expects UTF-8 input")]
    fn write_panics_on_invalid_utf8() {
        let patches: Vec<(Regex, String)> = Vec::new();
        let inner: Vec<u8> = Vec::new();
        let mut writer = BodyRegexWriter::new(inner, patches, 4);

        // 0xFF is invalid in UTF-8; this should trigger the expect().
        let _ = writer.write(&[0xff]).unwrap();
    }
}
