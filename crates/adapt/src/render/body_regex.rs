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
