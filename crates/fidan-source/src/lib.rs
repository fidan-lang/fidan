//! `fidan-source` — Source file management: FileId, SourceFile, SourceMap, Span.

mod source_file;
mod source_map;
mod span;

pub use source_file::{FileId, SourceFile};
pub use source_map::SourceMap;
pub use span::{ByteOffset, Location, Span};
