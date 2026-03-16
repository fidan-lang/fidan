use std::sync::Arc;

/// Opaque identifier for a loaded source file.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Default)]
pub struct FileId(pub u32);

/// A loaded source file: its path, raw text, and pre-computed line start offsets.
#[derive(Debug, Clone)]
pub struct SourceFile {
    pub id: FileId,
    /// The file path as given on the command line (may be relative).
    pub name: Arc<str>,
    /// The raw UTF-8 source text.
    pub src: Arc<str>,
    /// Byte offset of the start of each line.
    /// `line_starts[0]` is always 0 (start of the file).
    pub line_starts: Vec<u32>,
}

impl SourceFile {
    /// Create a new `SourceFile`, computing line-start offsets eagerly.
    pub fn new(id: FileId, name: impl Into<Arc<str>>, src: impl Into<Arc<str>>) -> Self {
        let src: Arc<str> = src.into();
        let line_starts = Self::compute_line_starts(&src);
        Self {
            id,
            name: name.into(),
            src,
            line_starts,
        }
    }

    fn compute_line_starts(src: &str) -> Vec<u32> {
        let mut starts = vec![0u32];
        for (i, b) in src.bytes().enumerate() {
            if b == b'\n' {
                starts.push(i as u32 + 1);
            }
        }
        starts
    }

    /// Convert a byte offset into a (1-based line, 1-based column) pair.
    pub fn line_col(&self, offset: u32) -> (u32, u32) {
        let line_idx = self
            .line_starts
            .partition_point(|&start| start <= offset)
            .saturating_sub(1);
        let col = offset - self.line_starts[line_idx] + 1;
        (line_idx as u32 + 1, col)
    }

    /// Return the text of a single line (0-indexed), without the trailing newline.
    pub fn line_text(&self, line_idx: usize) -> &str {
        let start = self.line_starts[line_idx] as usize;
        let end = if line_idx + 1 < self.line_starts.len() {
            // Trim the \n
            (self.line_starts[line_idx + 1] as usize).saturating_sub(1)
        } else {
            self.src.len()
        };
        &self.src[start..end]
    }
}
