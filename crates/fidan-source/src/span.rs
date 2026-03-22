use crate::FileId;
use serde::{Deserialize, Serialize};

/// A byte offset into a source file. Zero-based.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Default, Serialize, Deserialize,
)]
pub struct ByteOffset(pub u32);

/// A half-open byte range `[start, end)` inside a single source file.
///
/// Spans are intentionally cheap to copy (`Copy`).  They are *source-only*:
/// they carry no semantic information, just a location.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
pub struct Span {
    pub file: FileId,
    pub start: u32, // byte offset, inclusive
    pub end: u32,   // byte offset, exclusive
}

impl Span {
    pub fn new(file: FileId, start: u32, end: u32) -> Self {
        debug_assert!(start <= end, "Span start > end: {} > {}", start, end);
        Self { file, start, end }
    }

    /// A zero-length span pointing at a single byte position.
    pub fn point(file: FileId, offset: u32) -> Self {
        Self {
            file,
            start: offset,
            end: offset,
        }
    }

    /// Merge two spans (must be from the same file) into one that covers both.
    pub fn merge(self, other: Span) -> Span {
        debug_assert_eq!(
            self.file, other.file,
            "cannot merge spans from different files"
        );
        Span {
            file: self.file,
            start: self.start.min(other.start),
            end: self.end.max(other.end),
        }
    }

    pub fn len(&self) -> u32 {
        self.end - self.start
    }

    pub fn is_empty(&self) -> bool {
        self.start == self.end
    }
}

/// A human-readable source location: 1-based line and column.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Location {
    pub line: u32,
    pub col: u32,
}

impl std::fmt::Display for Location {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}:{}", self.line, self.col)
    }
}
