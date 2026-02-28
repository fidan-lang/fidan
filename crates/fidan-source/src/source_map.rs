use std::sync::{Arc, RwLock};
use crate::{FileId, SourceFile, Location};

/// Central registry for all source files in a compilation session.
///
/// Thread-safe so the driver can share it across parallel compilation tasks.
#[derive(Debug, Default)]
pub struct SourceMap {
    files: RwLock<Vec<Arc<SourceFile>>>,
}

impl SourceMap {
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a new source file and return its `FileId`.
    pub fn add_file(&self, name: impl Into<Arc<str>>, src: impl Into<Arc<str>>) -> Arc<SourceFile> {
        let mut files = self.files.write().unwrap();
        let id = FileId(files.len() as u32);
        let file = Arc::new(SourceFile::new(id, name, src));
        files.push(Arc::clone(&file));
        file
    }

    /// Look up a file by its `FileId`.
    pub fn get(&self, id: FileId) -> Arc<SourceFile> {
        let files = self.files.read().unwrap();
        Arc::clone(&files[id.0 as usize])
    }

    /// Convenience: resolve a byte offset in a file to a `Location`.
    pub fn location_of(&self, file: FileId, offset: u32) -> Location {
        let f = self.get(file);
        let (line, col) = f.line_col(offset);
        Location { line, col }
    }
}
